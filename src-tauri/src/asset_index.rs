//! Offline asset-index archives stored independently of engine projects and original files.
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_opener::OpenerExt;
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

fn resolve_original(root: &Path, relative: &Path) -> Result<PathBuf, &'static str> {
    const OUTSIDE: &str = "asset_original_outside";
    const MISSING: &str = "asset_original_offline";
    if !root.is_absolute()
        || relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(OUTSIDE);
    }
    #[cfg(windows)]
    {
        use std::path::Prefix;
        if !matches!(root.components().next(), Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        {
            return Err(OUTSIDE);
        }
    }
    let root = root.canonicalize().map_err(|_| MISSING)?;
    let original = root.join(relative).canonicalize().map_err(|_| MISSING)?;
    if !original.starts_with(&root) {
        return Err(OUTSIDE);
    }
    if !original.is_file() {
        return Err(MISSING);
    }
    Ok(original)
}

#[tauri::command]
pub(crate) async fn reveal_asset_original<R: Runtime>(
    app: AppHandle<R>,
    vault_id: String,
    asset_id: String,
) -> Result<(), &'static str> {
    let location = with_index(app.clone(), false, move |index| {
        index
            .ok_or("asset_original_offline")?
            .location(&vault_id, &asset_id)
            .map_err(index_error)?
            .ok_or("asset_original_offline")
    })
    .await?;
    tauri::async_runtime::spawn_blocking(move || {
        let original = resolve_original(Path::new(&location.root), Path::new(&location.relative))?;
        app.opener()
            .reveal_item_in_dir(original)
            .map_err(|_| "asset_original_unavailable")
    })
    .await
    .map_err(|_| "asset_original_unavailable")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn originals_are_scoped_to_files_inside_the_registered_library() {
        let fixture = tempfile::tempdir().unwrap();
        let library = fixture.path().join("existing");
        std::fs::create_dir(&library).unwrap();
        std::fs::write(library.join("asset.bin"), b"fixture").unwrap();
        let outside = fixture.path().join("outside.bin");
        std::fs::write(&outside, b"not in this library").unwrap();
        assert_eq!(
            resolve_original(&library, Path::new("asset.bin")).unwrap(),
            library.join("asset.bin").canonicalize().unwrap()
        );
        assert_eq!(
            resolve_original(&library, Path::new("../outside.bin")).unwrap_err(),
            "asset_original_outside"
        );
        assert_eq!(
            resolve_original(&library, Path::new("absent.bin")).unwrap_err(),
            "asset_original_offline"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, library.join("link")).unwrap();
            assert_eq!(
                resolve_original(&library, Path::new("link")).unwrap_err(),
                "asset_original_outside"
            );
        }
    }
}
