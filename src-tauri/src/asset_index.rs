//! Offline asset-index archives stored independently of engine projects and original files.
use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};
use uebox_asset_index::{Archive, Asset, AssetIndex, Error, Folder, ImportReport, Vault};

use crate::profile_storage::{acquire_lock, checked_file_exists, private_directory, ProfileError};

#[derive(Default)]
pub(crate) struct AssetIndexHost(tauri::async_runtime::Mutex<()>);

const INVALID: &str = "asset_index_invalid";
const UNAVAILABLE: &str = "asset_index_unavailable";

fn storage_error(error: ProfileError) -> &'static str {
    match error {
        ProfileError::Invalid => INVALID,
        ProfileError::Locked => "asset_index_busy",
        ProfileError::Unavailable => UNAVAILABLE,
    }
}

fn index_error(error: Error) -> &'static str {
    match error {
        Error::Invalid => INVALID,
        Error::Limit => "asset_index_limit",
        Error::Version => "asset_index_version",
        Error::Conflict => "asset_index_conflict",
        Error::RecoveryRequired => "asset_index_recovery_required",
        Error::Missing | Error::Unavailable => UNAVAILABLE,
    }
}

async fn with_index<R, T>(
    app: AppHandle<R>,
    create: bool,
    operation: impl FnOnce(Option<AssetIndex>) -> Result<T, &'static str> + Send + 'static,
) -> Result<T, &'static str>
where
    R: Runtime,
    T: Send + 'static,
{
    let root = app.path().app_local_data_dir().map_err(|_| UNAVAILABLE)?;
    let state = app.state::<AssetIndexHost>();
    let _operation = state.0.lock().await;
    tauri::async_runtime::spawn_blocking(move || {
        let Some(directory) =
            private_directory(&root, "asset-index", create).map_err(storage_error)?
        else {
            return operation(None);
        };
        let _lock = acquire_lock(&directory.join("index.lock")).map_err(storage_error)?;
        let database = directory.join("index.sqlite");
        checked_file_exists(&database).map_err(storage_error)?;
        let index = match AssetIndex::open_existing(&database) {
            Ok(index) => Some(index),
            Err(Error::Missing) if create => {
                Some(AssetIndex::create(&database).map_err(index_error)?)
            }
            Err(Error::Missing) => None,
            Err(error) => return Err(index_error(error)),
        };
        operation(index)
    })
    .await
    .map_err(|_| UNAVAILABLE)?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page<T> {
    items: Vec<T>,
    next_id: Option<String>,
}

fn page_size(limit: u32) -> Result<u32, &'static str> {
    if (1..=99).contains(&limit) {
        Ok(limit + 1)
    } else {
        Err(INVALID)
    }
}

fn page<T>(mut items: Vec<T>, size: usize, identity: impl FnOnce(&T) -> &str) -> Page<T> {
    let next_id = if items.len() > size {
        items.truncate(size);
        items.last().map(|last| identity(last).to_owned())
    } else {
        None
    };
    Page { items, next_id }
}

#[tauri::command]
pub(crate) async fn import_asset_index<R: Runtime>(
    app: AppHandle<R>,
    document: String,
) -> Result<ImportReport, &'static str> {
    // Parsing is potentially significant for a 16 MiB archive. Do it off the UI thread.
    let archive = tauri::async_runtime::spawn_blocking(move || Archive::parse(&document))
        .await
        .map_err(|_| UNAVAILABLE)?
        .map_err(index_error)?;
    with_index(app, true, move |index| {
        index
            .ok_or(UNAVAILABLE)?
            .import(archive)
            .map_err(index_error)
    })
    .await
}

#[tauri::command]
pub(crate) async fn list_asset_vaults<R: Runtime>(
    app: AppHandle<R>,
    after_id: Option<String>,
    limit: u32,
) -> Result<Page<Vault>, &'static str> {
    let count = page_size(limit)?;
    with_index(app, false, move |index| {
        let result = match index {
            Some(index) => index
                .vaults(after_id.as_deref(), count)
                .map_err(index_error)?,
            None => Vec::new(),
        };
        Ok(page(result, limit as usize, |vault| &vault.identity))
    })
    .await
}

#[tauri::command]
pub(crate) async fn list_asset_folders<R: Runtime>(
    app: AppHandle<R>,
    vault_id: String,
    folder_id: Option<String>,
    after_id: Option<String>,
    limit: u32,
) -> Result<Page<Folder>, &'static str> {
    let count = page_size(limit)?;
    with_index(app, false, move |index| {
        let result = match index {
            Some(index) => index
                .folders(&vault_id, folder_id.as_deref(), after_id.as_deref(), count)
                .map_err(index_error)?,
            None => Vec::new(),
        };
        Ok(page(result, limit as usize, |folder| &folder.identity))
    })
    .await
}

#[tauri::command]
pub(crate) async fn list_asset_items<R: Runtime>(
    app: AppHandle<R>,
    vault_id: String,
    folder_id: Option<String>,
    after_id: Option<String>,
    limit: u32,
    search: Option<String>,
) -> Result<Page<Asset>, &'static str> {
    let count = page_size(limit)?;
    with_index(app, false, move |index| {
        let result = match index {
            Some(index) => index
                .assets(
                    &vault_id,
                    folder_id.as_deref(),
                    after_id.as_deref(),
                    count,
                    search.as_deref(),
                )
                .map_err(index_error)?,
            None => Vec::new(),
        };
        Ok(page(result, limit as usize, |asset| &asset.identity))
    })
    .await
}
