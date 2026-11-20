//! Manage configured Streamable HTTP MCP endpoints and inspect their advertised capabilities.
mod catalog;
#[cfg(test)]
mod tests;
use std::{sync::Arc, time::Duration};

use serde::Serialize;
use tauri::{Manager, Runtime, WebviewWindow};
use uebox_credentials::{CredentialStore, Secret};

use catalog::{Catalog, Endpoint, MISSING, UNAVAILABLE};
use uebox_mcp_client::{ClientConfig, McpClient, McpError};

pub(crate) struct McpHost {
    operation: tauri::async_runtime::Mutex<()>,
    credentials: Arc<CredentialStore>,
}

impl Default for McpHost {
    fn default() -> Self {
        Self {
            operation: Default::default(),
            credentials: Arc::new(CredentialStore::native()),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpInspection {
    protocol_version: String,
    total_tools: usize,
    tool_names: Vec<String>,
    session_cleanup_pending: bool,
}

fn error_code(error: McpError) -> &'static str {
    match error {
        McpError::InvalidEndpoint => "mcp_invalid_endpoint",
        McpError::InvalidAuthorization => "credential_invalid_secret",
        McpError::Timeout => "mcp_timeout",
        McpError::Redirect => "mcp_redirect",
        McpError::HttpStatus(401 | 403) => "mcp_authorization_required",
        McpError::ProtocolVersionMismatch => "mcp_version",
        McpError::ToolsNotSupported => "mcp_no_tools",
        _ => "mcp_unavailable",
    }
}

#[tauri::command]
pub(crate) async fn inspect_mcp_endpoint<R: Runtime>(
    window: WebviewWindow<R>,
    endpoint: String,
) -> Result<McpInspection, &'static str> {
    require_main(&window)?;
    let state = window.state::<McpHost>();
    let _busy = state.operation.try_lock().map_err(|_| "mcp_busy")?;
    inspect(ClientConfig::new(endpoint)).await
}

async fn inspect(mut config: ClientConfig) -> Result<McpInspection, &'static str> {
    config.request_timeout = Duration::from_secs(10);
    let mut client = McpClient::connect(config).await.map_err(error_code)?;
    let tools = client.list_tools().await;
    let session_cleanup_pending = client.close().await.is_err();
    let tools = tools.map_err(error_code)?;
    let total_tools = tools.len();
    let tool_names = tools
        .into_iter()
        .take(200)
        .map(|tool| {
            tool.name
                .chars()
                .filter(|character| !character.is_control())
                .collect()
        })
        .collect();
    Ok(McpInspection {
        protocol_version: client.protocol_version().to_owned(),
        total_tools,
        tool_names,
        session_cleanup_pending,
    })
}

fn require_main<R: Runtime>(window: &WebviewWindow<R>) -> Result<(), &'static str> {
    (window.label() == "main")
        .then_some(())
        .ok_or("mcp_unavailable")
}

async fn with_catalog<R, T>(
    window: &WebviewWindow<R>,
    create: bool,
    operation: impl FnOnce(Option<Catalog>, &CredentialStore) -> Result<T, &'static str>
        + Send
        + 'static,
) -> Result<T, &'static str>
where
    R: Runtime,
    T: Send + 'static,
{
    let root = window
        .path()
        .app_local_data_dir()
        .map_err(|_| UNAVAILABLE)?;
    let credentials = Arc::clone(&window.state::<McpHost>().credentials);
    tauri::async_runtime::spawn_blocking(move || {
        operation(Catalog::open(&root, create)?, &credentials)
    })
    .await
    .map_err(|_| UNAVAILABLE)?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogChange {
    endpoints: Vec<Endpoint>,
    credential_cleanup_pending: bool,
}

fn add_to_catalog(
    catalog: &Catalog,
    store: &CredentialStore,
    title: String,
    endpoint: String,
    bearer: Option<String>,
) -> Result<CatalogChange, &'static str> {
    let mut entries = catalog.read()?;
    let entry = Endpoint::new(&title, &endpoint, bearer.is_some())?;
    if entries
        .iter()
        .any(|item| item.url == entry.url || item.id == entry.id)
    {
        return Err("mcp_endpoint_duplicate");
    }
    let staged = if let Some(bearer) = bearer {
        let secret = Secret::new(bearer.into_bytes()).map_err(|error| error.code())?;
        if secret
            .expose()
            .iter()
            .any(|byte| !(0x21..=0x7e).contains(byte))
        {
            return Err("credential_invalid_secret");
        }
        let target = entry.credential()?;
        store.set(&target, &secret).map_err(|error| error.code())?;
        Some(target)
    } else {
        None
    };
    entries.push(entry);
    let endpoints = match catalog.write(entries) {
        Ok(value) => value,
        Err(error) => {
            if let Some(target) = staged {
                let _ = store.delete(&target);
            }
            return Err(error);
        }
    };
    Ok(CatalogChange {
        endpoints,
        credential_cleanup_pending: false,
    })
}

fn remove_from_catalog(
    catalog: &Catalog,
    store: &CredentialStore,
    id: &str,
) -> Result<CatalogChange, &'static str> {
    let mut entries = catalog.read()?;
    let index = entries
        .iter()
        .position(|entry| entry.id == id)
        .ok_or(MISSING)?;
    let removed = entries.remove(index);
    let target = removed
        .authenticated
        .then(|| removed.credential())
        .transpose()?;
    let endpoints = catalog.write(entries)?;
    let credential_cleanup_pending = target.is_some_and(|target| store.delete(&target).is_err());
    Ok(CatalogChange {
        endpoints,
        credential_cleanup_pending,
    })
}

fn registered_client(
    catalog: Catalog,
    store: &CredentialStore,
    id: &str,
) -> Result<ClientConfig, &'static str> {
    let entry = catalog
        .read()?
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or(MISSING)?;
    let credentials = if entry.authenticated {
        Some(
            store
                .get(&entry.credential()?)
                .map_err(|error| error.code())?
                .ok_or("credential_unavailable")?,
        )
    } else {
        None
    };
    let config = ClientConfig::new(entry.url);
    if let Some(credentials) = credentials {
        config.with_bearer(credentials.expose()).map_err(error_code)
    } else {
        Ok(config)
    }
}

#[tauri::command]
pub(crate) async fn list_mcp_endpoints<R: Runtime>(
    window: WebviewWindow<R>,
) -> Result<Vec<Endpoint>, &'static str> {
    require_main(&window)?;
    with_catalog(&window, false, |catalog, _| {
        catalog
            .map(|file| file.read())
            .transpose()
            .map(Option::unwrap_or_default)
    })
    .await
}

#[tauri::command]
pub(crate) async fn save_mcp_endpoint<R: Runtime>(
    window: WebviewWindow<R>,
    title: String,
    endpoint: String,
    bearer: Option<String>,
) -> Result<CatalogChange, &'static str> {
    require_main(&window)?;
    let state = window.state::<McpHost>();
    let _busy = state.operation.try_lock().map_err(|_| "mcp_busy")?;
    with_catalog(&window, true, move |catalog, store| {
        add_to_catalog(&catalog.ok_or(UNAVAILABLE)?, store, title, endpoint, bearer)
    })
    .await
}

#[tauri::command]
pub(crate) async fn remove_mcp_endpoint<R: Runtime>(
    window: WebviewWindow<R>,
    endpoint_id: String,
) -> Result<CatalogChange, &'static str> {
    require_main(&window)?;
    let state = window.state::<McpHost>();
    let _busy = state.operation.try_lock().map_err(|_| "mcp_busy")?;
    with_catalog(&window, false, move |catalog, store| {
        remove_from_catalog(&catalog.ok_or(MISSING)?, store, &endpoint_id)
    })
    .await
}

#[tauri::command]
pub(crate) async fn inspect_saved_mcp_endpoint<R: Runtime>(
    window: WebviewWindow<R>,
    endpoint_id: String,
) -> Result<McpInspection, &'static str> {
    require_main(&window)?;
    let state = window.state::<McpHost>();
    let _busy = state.operation.try_lock().map_err(|_| "mcp_busy")?;
    let config = with_catalog(&window, false, move |catalog, store| {
        registered_client(catalog.ok_or(MISSING)?, store, &endpoint_id)
    })
    .await?;
    inspect(config).await
}
