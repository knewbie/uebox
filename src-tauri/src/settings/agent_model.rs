use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

use tauri::{AppHandle, Manager, Runtime};
use uebox_agent_core::{Error, OpenAiProvider, ProviderConfig};

use super::{
    credential_ref, read, Authorization, CatalogFile, ModelSettings, Purpose, MISSING_CREDENTIAL,
    NOT_FOUND,
};

pub(crate) type CredentialSlot = Arc<Mutex<Option<String>>>;
type WeakCredentialSlot = Weak<Mutex<Option<String>>>;

pub(crate) struct AgentModel {
    pub provider: OpenAiProvider,
    pub credential: CredentialSlot,
}

impl AgentModel {
    pub(crate) fn direct(
        endpoint: String,
        model: String,
        credential: Option<String>,
    ) -> Result<Self, Error> {
        Self::new(
            endpoint,
            model,
            Arc::new(Mutex::new(credential.filter(|value| !value.is_empty()))),
        )
    }

    fn new(endpoint: String, model: String, credential: CredentialSlot) -> Result<Self, Error> {
        let token = credential.clone();
        let required = token
            .lock()
            .map_err(|_| Error::CredentialUnavailable)?
            .is_some();
        let provider = OpenAiProvider::new(
            ProviderConfig::new(endpoint, model)
                .with_max_output_tokens(1024)
                .with_max_request_bytes(1024 * 1024)
                .with_token_provider(move || {
                    let value = token
                        .lock()
                        .map_err(|_| Error::CredentialUnavailable)?
                        .clone();
                    if required && value.is_none() {
                        Err(Error::CredentialUnavailable)
                    } else {
                        Ok(value)
                    }
                }),
        )?;
        Ok(Self {
            provider,
            credential,
        })
    }
}

#[derive(Default)]
pub(super) struct ActiveCredentials(Mutex<HashMap<String, Vec<WeakCredentialSlot>>>);

impl ActiveCredentials {
    fn lease(&self, profile: &str, credential: Option<String>) -> CredentialSlot {
        let slot = Arc::new(Mutex::new(credential));
        let mut active = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active.retain(|_, slots| {
            slots.retain(|slot| slot.strong_count() > 0);
            !slots.is_empty()
        });
        active
            .entry(profile.to_owned())
            .or_default()
            .push(Arc::downgrade(&slot));
        slot
    }

    pub(super) fn revoke(&self, profile: &str) {
        let slots = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(profile);
        for slot in slots
            .into_iter()
            .flatten()
            .filter_map(|slot| slot.upgrade())
        {
            slot.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
        }
    }
}

pub(crate) enum AgentModelSource {
    Direct {
        endpoint: String,
        model: String,
        credential: Option<String>,
    },
    Saved {
        profile_id: Option<String>,
    },
}

impl AgentModelSource {
    #[cfg(any(feature = "unreal", feature = "unity"))]
    pub(crate) async fn resolve<R: Runtime>(
        self,
        app: AppHandle<R>,
        map_error: fn(Error) -> &'static str,
    ) -> Result<AgentModel, &'static str> {
        match self {
            Self::Direct {
                endpoint,
                model,
                credential,
            } => AgentModel::direct(endpoint, model, credential).map_err(map_error),
            Self::Saved { profile_id } => {
                tauri::async_runtime::spawn_blocking(move || resolve_saved_model(&app, profile_id))
                    .await
                    .map_err(|_| super::UNAVAILABLE)?
            }
        }
    }

    #[cfg(feature = "godot")]
    pub(crate) fn resolve_in_worker<R: Runtime>(
        self,
        app: &AppHandle<R>,
        map_error: fn(Error) -> &'static str,
    ) -> Result<AgentModel, &'static str> {
        match self {
            Self::Direct {
                endpoint,
                model,
                credential,
            } => AgentModel::direct(endpoint, model, credential).map_err(map_error),
            Self::Saved { profile_id } => resolve_saved_model(app, profile_id),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
pub(crate) async fn resolve_cli_model<R: Runtime>(
    app: AppHandle<R>,
    profile_id: String,
) -> Result<AgentModel, &'static str> {
    tauri::async_runtime::spawn_blocking(move || {
        resolve_configured_model(&app, Some(profile_id), true)
    })
    .await
    .map_err(|_| super::UNAVAILABLE)?
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
pub(crate) async fn validate_cli_model<R: Runtime>(
    app: AppHandle<R>,
    profile_id: String,
) -> Result<(), &'static str> {
    tauri::async_runtime::spawn_blocking(move || {
        let settings = app.state::<ModelSettings>();
        let root = settings.data_dir(&app)?;
        let file = CatalogFile::open(&root, false)?;
        let catalog = read(file.as_ref())?;
        with_agent_binding(&catalog, &profile_id)
    })
    .await
    .map_err(|_| super::UNAVAILABLE)?
}

fn with_agent_binding(catalog: &super::Catalog, id: &str) -> Result<(), &'static str> {
    if catalog
        .bindings
        .get(&Purpose::Agent)
        .is_some_and(|saved| saved == id)
        && catalog.find(id).is_some()
    {
        Ok(())
    } else {
        Err(NOT_FOUND)
    }
}

pub(super) fn resolve_saved_model<R: Runtime>(
    app: &AppHandle<R>,
    selected_profile: Option<String>,
) -> Result<AgentModel, &'static str> {
    resolve_configured_model(app, selected_profile, false)
}

fn resolve_configured_model<R: Runtime>(
    app: &AppHandle<R>,
    selected_profile: Option<String>,
    require_agent_binding: bool,
) -> Result<AgentModel, &'static str> {
    let settings = app.state::<ModelSettings>();
    let root = settings.data_dir(app)?;
    let file = CatalogFile::open(&root, false)?;
    let credentials = &settings.credentials;
    let leases = &settings.active_credentials;
    let catalog = read(file.as_ref())?;
    let id = selected_profile
        .as_deref()
        .or_else(|| catalog.bindings.get(&Purpose::Agent).map(String::as_str))
        .or_else(|| catalog.bindings.get(&Purpose::Chat).map(String::as_str))
        .ok_or(NOT_FOUND)?;
    if require_agent_binding {
        with_agent_binding(&catalog, id)?;
    }
    let profile = catalog.find(id).ok_or(NOT_FOUND)?;
    let bearer = match &profile.authorization {
        Authorization::None => None,
        Authorization::Reauthorize => return Err("credential_reauthorization_required"),
        Authorization::Environment { name } => {
            Some(std::env::var(name).map_err(|_| MISSING_CREDENTIAL)?)
        }
        Authorization::Keychain { reference } => {
            let entry = credential_ref(reference)?;
            let secret = credentials
                .get(&entry)
                .map_err(|error| error.code())?
                .ok_or(MISSING_CREDENTIAL)?;
            Some(
                std::str::from_utf8(secret.expose())
                    .map_err(|_| "credential_invalid_secret")?
                    .to_owned(),
            )
        }
    };
    if bearer.as_ref().is_some_and(|value| {
        value.is_empty() || value.bytes().any(|byte| !(0x21..=0x7e).contains(&byte))
    }) {
        return Err("credential_invalid_secret");
    }
    let credential = leases.lease(id, bearer);
    AgentModel::new(profile.endpoint.clone(), profile.model.clone(), credential)
        .map_err(|_| "invalid_model_configuration")
}
