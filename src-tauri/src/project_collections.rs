//! Project collections are an engine-neutral view over the Rust registrations.
#[cfg(test)]
mod tests;
pub(crate) mod transfer;

use std::path::Path;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};
use uebox_project_collections::{Collection, CollectionError, ProjectCollections};
use uebox_project_library::ProjectLibrary;

use crate::{
    profile_storage::{acquire_lock, checked_file_exists, private_directory, ProfileError},
    project_library::{with_library, ProjectRegistrations},
};

const INVALID: &str = "project_collections_invalid";
const LOCKED: &str = "project_collections_locked";
const UNAVAILABLE: &str = "project_collections_unavailable";
const NOT_FOUND: &str = "project_collection_not_found";

fn storage_error(error: ProfileError) -> &'static str {
    match error {
        ProfileError::Invalid => INVALID,
        ProfileError::Locked => LOCKED,
        ProfileError::Unavailable => UNAVAILABLE,
    }
}

fn collection_error(error: CollectionError) -> &'static str {
    match error {
        CollectionError::InvalidInput | CollectionError::Corrupt | CollectionError::Unsupported => {
            INVALID
        }
        CollectionError::CollectionMissing => NOT_FOUND,
        CollectionError::ProjectMissing => "project_missing",
        CollectionError::Locked => LOCKED,
        CollectionError::RecoveryRequired => "project_collections_recovery_required",
        CollectionError::Duplicate | CollectionError::Conflict => "project_collections_conflict",
        _ => UNAVAILABLE,
    }
}

fn with_collection_file<T>(
    root: &Path,
    create: bool,
    operation: impl FnOnce(Option<&mut ProjectCollections>) -> Result<T, &'static str>,
) -> Result<T, &'static str> {
    let Some(directory) =
        private_directory(root, "project-collections", create).map_err(storage_error)?
    else {
        return operation(None);
    };
    let _lock = acquire_lock(&directory.join("collections.lock")).map_err(storage_error)?;
    let target = directory.join("collections.sqlite");
    checked_file_exists(&target).map_err(storage_error)?;
    let mut collections = match ProjectCollections::open_existing(&target) {
        Ok(existing) => Some(existing),
        Err(CollectionError::Missing) if create => {
            Some(ProjectCollections::create_new(&target).map_err(collection_error)?)
        }
        Err(CollectionError::Missing) => None,
        Err(error) => return Err(collection_error(error)),
    };
    operation(collections.as_mut())
}

async fn with_collections<R, T>(
    app: AppHandle<R>,
    create: bool,
    operation: impl FnOnce(Option<(&mut ProjectCollections, &ProjectLibrary)>) -> Result<T, &'static str>
        + Send
        + 'static,
) -> Result<T, &'static str>
where
    R: Runtime,
    T: Send + 'static,
{
    let root = app.state::<ProjectRegistrations>().app_data_dir(&app)?;
    with_library(app, create, move |library| {
        with_collection_file(&root, create, move |collections| {
            match (collections, library) {
                (Some(collections), Some(library)) => operation(Some((collections, library))),
                (Some(_), None) => Err(UNAVAILABLE),
                (None, _) => operation(None),
            }
        })
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CollectionView {
    id: String,
    name: Option<String>,
    project_ids: Vec<String>,
}

impl From<Collection> for CollectionView {
    fn from(collection: Collection) -> Self {
        Self {
            id: collection.id,
            name: collection.name,
            project_ids: collection
                .project_ids
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
        }
    }
}

fn view(
    projects: &ProjectLibrary,
    collections: &ProjectCollections,
) -> Result<Vec<CollectionView>, &'static str> {
    collections
        .list_collections(projects)
        .map(|entries| entries.into_iter().map(Into::into).collect())
        .map_err(collection_error)
}

fn project_id(value: &str) -> Result<i64, &'static str> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0 && id.to_string() == value)
        .ok_or("project_invalid")
}

#[tauri::command]
pub(crate) async fn list_project_collections<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Vec<CollectionView>, &'static str> {
    with_collections(app, false, |data| match data {
        Some((collections, projects)) => view(projects, collections),
        None => Ok(Vec::new()),
    })
    .await
}

#[tauri::command]
pub(crate) async fn create_project_collection<R: Runtime>(
    app: AppHandle<R>,
    name: Option<String>,
) -> Result<Vec<CollectionView>, &'static str> {
    let name = name.unwrap_or_default();
    if name.len() > 1024 || name.chars().any(char::is_control) {
        return Err(INVALID);
    }
    with_collections(app, true, move |data| {
        let (collections, projects) = data.ok_or(UNAVAILABLE)?;
        let order = collections
            .list_collections(projects)
            .map_err(collection_error)?
            .last()
            .map_or(0, |collection| collection.sort_order.saturating_add(1));
        let mut random = [0_u8; 16];
        getrandom::getrandom(&mut random).map_err(|_| UNAVAILABLE)?;
        let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        collections
            .create_collection(&format!("native-{id}"), &name, order)
            .map_err(collection_error)?;
        view(projects, collections)
    })
    .await
}

#[tauri::command]
pub(crate) async fn rename_project_collection<R: Runtime>(
    app: AppHandle<R>,
    collection_id: String,
    name: Option<String>,
) -> Result<Vec<CollectionView>, &'static str> {
    with_collections(app, false, move |data| {
        let (collections, projects) = data.ok_or(NOT_FOUND)?;
        collections
            .rename_collection(&collection_id, name.as_deref().unwrap_or_default())
            .map_err(collection_error)?;
        view(projects, collections)
    })
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Direction {
    Up,
    Down,
}

#[tauri::command]
pub(crate) async fn move_project_collection<R: Runtime>(
    app: AppHandle<R>,
    collection_id: String,
    direction: Direction,
) -> Result<Vec<CollectionView>, &'static str> {
    with_collections(app, false, move |data| {
        let (collections, projects) = data.ok_or(NOT_FOUND)?;
        let mut entries = collections
            .list_collections(projects)
            .map_err(collection_error)?;
        let index = entries
            .iter()
            .position(|entry| entry.id == collection_id)
            .ok_or(NOT_FOUND)?;
        let neighbor = match direction {
            Direction::Up => index.checked_sub(1),
            Direction::Down => index.checked_add(1).filter(|index| *index < entries.len()),
        };
        if let Some(neighbor) = neighbor {
            entries.swap(index, neighbor);
            let updates: Vec<_> = entries
                .iter()
                .enumerate()
                .map(|(index, entry)| (entry.id.as_str(), index as i64))
                .collect();
            collections.set_order(&updates).map_err(collection_error)?;
        }
        view(projects, collections)
    })
    .await
}

#[tauri::command]
pub(crate) async fn set_project_collection_membership<R: Runtime>(
    app: AppHandle<R>,
    collection_id: String,
    registration_id: String,
    belongs: bool,
) -> Result<Vec<CollectionView>, &'static str> {
    let id = project_id(&registration_id)?;
    with_collections(app, false, move |data| {
        let (collections, projects) = data.ok_or(NOT_FOUND)?;
        if belongs {
            collections
                .bind_project(projects, &collection_id, id)
                .map_err(collection_error)?;
        } else {
            collections
                .unbind_project(Some(&collection_id), id)
                .map_err(collection_error)?;
        }
        view(projects, collections)
    })
    .await
}

#[tauri::command]
pub(crate) async fn remove_project_collection<R: Runtime>(
    app: AppHandle<R>,
    collection_id: String,
) -> Result<Vec<CollectionView>, &'static str> {
    with_collections(app, false, move |data| match data {
        Some((collections, projects)) => {
            collections
                .remove_collection(&collection_id)
                .map_err(collection_error)?;
            view(projects, collections)
        }
        None => Ok(Vec::new()),
    })
    .await
}
