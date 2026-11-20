//! Model settings backed by the local profile and the platform credential store.
#[cfg(any(feature = "unreal", feature = "unity", feature = "godot"))]
mod agent_model;
mod catalog;
mod disk;
#[cfg(test)]
mod tests;
pub(crate) mod transfer;
#[cfg(any(feature = "unreal", feature = "unity", feature = "godot"))]
pub(crate) use agent_model::{AgentModel, AgentModelSource};

use std::{collections::BTreeMap, sync::Arc};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};
use uebox_credentials::{CredentialPurpose, CredentialRef, CredentialStore, Secret};

use catalog::{endpoint, profile_id_valid, Authorization, Catalog, ModelProfile, Purpose};
use disk::CatalogFile;

const UNAVAILABLE: &str = "settings_unavailable";
const NOT_FOUND: &str = "model_profile_not_found";
#[cfg(any(feature = "unreal", feature = "unity", feature = "godot"))]
const MISSING_CREDENTIAL: &str = "credential_unavailable";

#[derive(Clone)]
pub(crate) struct ModelSettings {
    credentials: Arc<CredentialStore>,
    #[cfg(test)]
    test_data_dir: Option<std::path::PathBuf>,
    #[cfg(any(feature = "unreal", feature = "unity", feature = "godot"))]
    active_credentials: Arc<agent_model::ActiveCredentials>,
}

impl Default for ModelSettings {
    fn default() -> Self {
        Self {
            credentials: Arc::new(CredentialStore::native()),
            #[cfg(test)]
            test_data_dir: None,
            #[cfg(any(feature = "unreal", feature = "unity", feature = "godot"))]
            active_credentials: Default::default(),
        }
    }
}

impl ModelSettings {
    fn data_dir<R: Runtime>(&self, app: &AppHandle<R>) -> Result<std::path::PathBuf, &'static str> {
        #[cfg(test)]
        if let Some(path) = &self.test_data_dir {
            return Ok(path.clone());
        }
        app.path().app_local_data_dir().map_err(|_| UNAVAILABLE)
    }

    async fn transaction<R, T>(
        app: AppHandle<R>,
        write: bool,
        operation: impl FnOnce(Option<CatalogFile>, &CredentialStore) -> Result<T, &'static str>
            + Send
            + 'static,
    ) -> Result<T, &'static str>
    where
        R: Runtime,
        T: Send + 'static,
    {
        let settings = app.state::<Self>();
        let credentials = Arc::clone(&settings.credentials);
        let root = settings.data_dir(&app)?;
        tauri::async_runtime::spawn_blocking(move || {
            operation(CatalogFile::open(&root, write)?, &credentials)
        })
        .await
        .map_err(|_| UNAVAILABLE)?
    }
}

impl ModelSettings {
    fn revoke_active_credentials(&self, id: &str) {
        #[cfg(any(feature = "unreal", feature = "unity", feature = "godot"))]
        self.active_credentials.revoke(id);
        #[cfg(not(any(feature = "unreal", feature = "unity", feature = "godot")))]
        let _ = id;
    }
}

fn generate_id() -> Result<String, &'static str> {
    let mut bytes = [0; 16];
    getrandom::getrandom(&mut bytes).map_err(|_| UNAVAILABLE)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn credential_ref(id: &str) -> Result<CredentialRef, &'static str> {
    CredentialRef::new(CredentialPurpose::ModelBearer, id).map_err(|error| error.code())
}

fn read(file: Option<&CatalogFile>) -> Result<Catalog, &'static str> {
    file.map(CatalogFile::read)
        .transpose()
        .map(Option::unwrap_or_default)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileView {
    id: String,
    title: String,
    endpoint: String,
    model: String,
    authorization: &'static str,
    environment: Option<String>,
}

impl From<ModelProfile> for ProfileView {
    fn from(profile: ModelProfile) -> Self {
        let (authorization, environment) = match profile.authorization {
            Authorization::None => ("none", None),
            Authorization::Reauthorize => ("needsAuthorization", None),
            Authorization::Environment { name } => ("environment", Some(name)),
            Authorization::Keychain { .. } => ("keychain", None),
        };
        Self {
            id: profile.id,
            title: profile.title,
            endpoint: profile.endpoint,
            model: profile.model,
            authorization,
            environment,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct CatalogView {
    profiles: Vec<ProfileView>,
    bindings: BTreeMap<Purpose, String>,
}

impl From<Catalog> for CatalogView {
    fn from(catalog: Catalog) -> Self {
        Self {
            profiles: catalog
                .profiles
                .into_iter()
                .map(ProfileView::from)
                .collect(),
            bindings: catalog.bindings,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum AuthorizationUpdate {
    Keep,
    None,
    Environment { name: String },
    Keychain { bearer: String },
}

impl AuthorizationUpdate {
    fn prepare(
        self,
        existing: Option<&ModelProfile>,
        new_endpoint: &str,
    ) -> Result<(Authorization, Option<Secret>), &'static str> {
        Ok(match self {
            Self::Keep => {
                let existing = existing.ok_or(NOT_FOUND)?;
                if !matches!(existing.authorization, Authorization::None)
                    && existing.endpoint != new_endpoint
                {
                    return Err("credential_reauthorization_required");
                }
                (existing.authorization.clone(), None)
            }
            Self::None => (Authorization::None, None),
            Self::Environment { name } => (Authorization::Environment { name }, None),
            Self::Keychain { bearer } => {
                let secret = Secret::new(bearer.into_bytes()).map_err(|error| error.code())?;
                if secret
                    .expose()
                    .iter()
                    .any(|byte| !(0x21..=0x7e).contains(byte))
                {
                    return Err("credential_invalid_secret");
                }
                let reference = generate_id()?;
                (Authorization::Keychain { reference }, Some(secret))
            }
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProfileUpdate {
    id: Option<String>,
    title: String,
    endpoint: String,
    model: String,
    authorization: AuthorizationUpdate,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MutationResult {
    catalog: CatalogView,
    credential_cleanup_pending: bool,
}

fn retire_credential(
    old: Option<ModelProfile>,
    keep: Option<&str>,
    store: &CredentialStore,
) -> bool {
    let Some(Authorization::Keychain { reference }) = old.map(|profile| profile.authorization)
    else {
        return false;
    };
    if Some(reference.as_str()) == keep {
        return false;
    }
    credential_ref(&reference)
        .and_then(|entry| store.delete(&entry).map_err(|error| error.code()))
        .is_err()
}

fn save(
    file: &CatalogFile,
    store: &CredentialStore,
    update: ProfileUpdate,
) -> Result<MutationResult, &'static str> {
    let mut catalog = file.read()?;
    let (id, old) = match update.id {
        Some(id) if profile_id_valid(&id) => {
            let old = catalog.find(&id).cloned().ok_or(NOT_FOUND)?;
            (id, Some(old))
        }
        Some(_) => return Err(NOT_FOUND),
        None => (generate_id()?, None),
    };
    let endpoint = endpoint(&update.endpoint, false)?.to_string();
    let (authorization, pending) = update.authorization.prepare(old.as_ref(), &endpoint)?;
    let reference = match &authorization {
        Authorization::Keychain { reference } => Some(reference.clone()),
        _ => None,
    };
    catalog.put(ModelProfile {
        id,
        title: update.title.trim().to_owned(),
        endpoint,
        model: update.model.trim().to_owned(),
        authorization,
        legacy_id: old.as_ref().and_then(|profile| profile.legacy_id.clone()),
    });
    catalog.validate()?;
    let staged = match (reference.as_deref(), pending) {
        (Some(id), Some(secret)) => {
            let entry = credential_ref(id)?;
            store.set(&entry, &secret).map_err(|error| error.code())?;
            Some(entry)
        }
        _ => None,
    };
    if let Err(error) = file.write(&catalog) {
        if let Some(staged) = staged {
            let _ = store.delete(&staged);
        }
        return Err(error);
    }
    let credential_cleanup_pending = retire_credential(old, reference.as_deref(), store);
    Ok(MutationResult {
        catalog: catalog.into(),
        credential_cleanup_pending,
    })
}

#[tauri::command]
pub(crate) async fn get_model_settings<R: Runtime>(
    app: AppHandle<R>,
) -> Result<CatalogView, &'static str> {
    ModelSettings::transaction(app, false, |file, _| read(file.as_ref()).map(Into::into)).await
}

#[tauri::command]
pub(crate) async fn save_model_profile<R: Runtime>(
    app: AppHandle<R>,
    profile: ProfileUpdate,
) -> Result<MutationResult, &'static str> {
    let settings = app.state::<ModelSettings>().inner().clone();
    ModelSettings::transaction(app, true, move |file, store| {
        let file = file.ok_or(UNAVAILABLE)?;
        let prior = profile.id.clone();
        let result = save(&file, store, profile)?;
        if let Some(prior) = prior {
            settings.revoke_active_credentials(&prior);
        }
        Ok(result)
    })
    .await
}

#[tauri::command]
pub(crate) async fn remove_model_profile<R: Runtime>(
    app: AppHandle<R>,
    profile_id: String,
) -> Result<MutationResult, &'static str> {
    let settings = app.state::<ModelSettings>().inner().clone();
    ModelSettings::transaction(app, false, move |file, store| {
        let file = file.ok_or(NOT_FOUND)?;
        let mut catalog = file.read()?;
        let removed = catalog.remove(&profile_id).ok_or(NOT_FOUND)?;
        file.write(&catalog)?;
        settings.revoke_active_credentials(&profile_id);
        let credential_cleanup_pending = retire_credential(Some(removed), None, store);
        Ok(MutationResult {
            catalog: catalog.into(),
            credential_cleanup_pending,
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn bind_model_profile<R: Runtime>(
    app: AppHandle<R>,
    purpose: Purpose,
    profile_id: Option<String>,
) -> Result<CatalogView, &'static str> {
    ModelSettings::transaction(app, false, move |file, _| {
        let file = file.ok_or(NOT_FOUND)?;
        let mut catalog = file.read()?;
        if let Some(profile_id) = profile_id {
            catalog.find(&profile_id).ok_or(NOT_FOUND)?;
            catalog.bindings.insert(purpose, profile_id);
        } else {
            catalog.bindings.remove(&purpose);
        }
        file.write(&catalog)?;
        Ok(catalog.into())
    })
    .await
}
