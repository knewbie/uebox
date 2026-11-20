use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Runtime};

use super::{
    endpoint, generate_id, Authorization, Catalog, CatalogView, ModelProfile, ModelSettings,
};

const INVALID: &str = "model_profile_import_invalid";
const LIMIT: &str = "model_profile_import_limit";

#[derive(Clone, Copy, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
enum PortablePurpose {
    Chat,
    Agent,
    Vision,
    Summary,
}

impl From<PortablePurpose> for super::Purpose {
    fn from(purpose: PortablePurpose) -> Self {
        match purpose {
            PortablePurpose::Chat => Self::Chat,
            PortablePurpose::Agent => Self::Agent,
            PortablePurpose::Vision => Self::Vision,
            PortablePurpose::Summary => Self::Summary,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Selection {
    provider_id: String,
    model: String,
}

#[derive(Deserialize)]
enum Protocol {
    #[serde(rename = "openai-chat-completions")]
    ChatCompletions,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PortableProfile {
    provider_id: String,
    provider_name: String,
    model: String,
    title: String,
    endpoint: String,
    #[serde(rename = "protocol")]
    _protocol: Protocol,
    requires_reauthorization: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    version: u32,
    profiles: Vec<PortableProfile>,
    bindings: BTreeMap<PortablePurpose, Selection>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportResult {
    inserted: usize,
    already_imported: usize,
    needs_authorization: usize,
    catalog: CatalogView,
}

fn valid_name(name: &str) -> bool {
    name.trim() == name
        && !name.is_empty()
        && name.len() <= 256
        && !name.chars().any(char::is_control)
}

fn apply_manifest(
    catalog: &mut Catalog,
    manifest: Manifest,
) -> Result<(usize, usize, usize), &'static str> {
    let mut selected = HashMap::new();
    let mut inserted = 0;
    let mut known = 0;
    let mut pending = 0;
    for profile in manifest.profiles {
        if [
            &profile.provider_id,
            &profile.provider_name,
            &profile.model,
            &profile.title,
        ]
        .into_iter()
        .any(|name| !valid_name(name))
        {
            return Err(INVALID);
        }
        let endpoint = endpoint(&profile.endpoint, false)
            .map_err(|_| INVALID)?
            .to_string();
        let identity = format!("{}\0{}\0{}", profile.provider_id, profile.model, endpoint);
        let id: String = Sha256::digest(identity)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let existing = catalog
            .profiles
            .iter()
            .find(|existing| existing.legacy_id.as_deref() == Some(&id));
        let native = if let Some(existing) = existing {
            known += 1;
            existing.id.clone()
        } else {
            let native = generate_id()?;
            let authorization = if profile.requires_reauthorization {
                pending += 1;
                Authorization::Reauthorize
            } else {
                Authorization::None
            };
            catalog.put(ModelProfile {
                id: native.clone(),
                title: profile.title,
                endpoint,
                model: profile.model.clone(),
                authorization,
                legacy_id: Some(id),
            });
            inserted += 1;
            native
        };
        if selected
            .insert((profile.provider_id, profile.model), native)
            .is_some()
        {
            return Err(INVALID);
        }
    }
    for (role, target) in manifest.bindings {
        let id = selected
            .get(&(target.provider_id, target.model))
            .ok_or(INVALID)?;
        catalog
            .bindings
            .entry(role.into())
            .or_insert_with(|| id.clone());
    }
    catalog.validate().map_err(|_| INVALID)?;
    Ok((inserted, known, pending))
}

#[tauri::command]
pub(crate) async fn import_model_profiles<R: Runtime>(
    app: AppHandle<R>,
    manifest: String,
) -> Result<ImportResult, &'static str> {
    if manifest.len() > 512 * 1024 {
        return Err(LIMIT);
    }
    let document: serde_json::Value = serde_json::from_str(&manifest).map_err(|_| INVALID)?;
    if !document.is_object()
        || !document["profiles"]
            .as_array()
            .is_some_and(|rows| rows.iter().all(serde_json::Value::is_object))
        || !document["bindings"]
            .as_object()
            .is_some_and(|rows| rows.values().all(serde_json::Value::is_object))
    {
        return Err(INVALID);
    }
    let manifest: Manifest = serde_json::from_value(document).map_err(|_| INVALID)?;
    if manifest.format != "uebox-model-profiles" || manifest.version != 1 {
        return Err("model_profile_import_version");
    }
    if manifest.profiles.len() > 128 {
        return Err(LIMIT);
    }
    ModelSettings::transaction(app, true, move |file, _| {
        let file = file.ok_or(super::UNAVAILABLE)?;
        let mut catalog = file.read()?;
        let (inserted, already_imported, needs_authorization) =
            apply_manifest(&mut catalog, manifest)?;
        file.write(&catalog)?;
        Ok(ImportResult {
            inserted,
            already_imported,
            needs_authorization,
            catalog: catalog.into(),
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Manifest {
        serde_json::from_value(json!({
            "format": "uebox-model-profiles",
            "version": 1,
            "profiles": [{
                "providerId": "legacy-provider",
                "providerName": "Fixture provider",
                "model": "fixture-model",
                "title": "Fixture",
                "endpoint": "https://example.test/v1/chat/completions",
                "protocol": "openai-chat-completions",
                "requiresReauthorization": true
            }],
            "bindings": {"agent": {"providerId": "legacy-provider", "model": "fixture-model"}}
        }))
        .unwrap()
    }

    #[test]
    fn repeated_transfer_preserves_native_authorization_and_role_choices() {
        let mut catalog = Catalog::default();
        assert_eq!(apply_manifest(&mut catalog, fixture()), Ok((1, 0, 1)));
        assert!(matches!(
            catalog.profiles[0].authorization,
            Authorization::Reauthorize
        ));
        let id = catalog.profiles[0].id.clone();
        assert_eq!(catalog.bindings[&super::super::Purpose::Agent], id);
        catalog.profiles[0].authorization = Authorization::None;
        catalog.profiles[0].title = "Renamed".into();
        assert_eq!(apply_manifest(&mut catalog, fixture()), Ok((0, 1, 0)));
        assert!(matches!(
            catalog.profiles[0].authorization,
            Authorization::None
        ));
        assert_eq!(catalog.profiles[0].title, "Renamed");
    }

    #[test]
    fn transfer_rejects_credentials_or_paths_instead_of_silently_discarding_them() {
        let mut invalid: serde_json::Value = json!({
            "format": "uebox-model-profiles", "version": 1, "bindings": {},
            "profiles": [{
                "providerId": "sample", "providerName": "Fixture", "model": "model",
                "title": "Fixture", "endpoint": "https://example.test/v1/chat/completions",
                "protocol": "openai-chat-completions", "requiresReauthorization": false,
                "bearer": "synthetic"
            }]
        });
        assert!(serde_json::from_value::<Manifest>(invalid.clone()).is_err());
        invalid["profiles"][0]
            .as_object_mut()
            .unwrap()
            .remove("bearer");
        invalid["profiles"][0]["endpoint"] = json!("file:///tmp/main");
        let mut catalog = Catalog::default();
        assert_eq!(
            apply_manifest(&mut catalog, serde_json::from_value(invalid).unwrap()),
            Err(INVALID)
        );
        assert!(catalog.profiles.is_empty());
    }
}
