use std::path::PathBuf;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};
use uebox_project_library::{CoverFormat, ProjectLibrary, ProjectMetadata};

use super::{library_error, with_library, ProjectRegistrations};

const MAX_COVER_BYTES: usize = 8 * 1024 * 1024;
const UNAVAILABLE: &str = "project_library_unavailable";
const PROJECT_MISSING: &str = "project_missing";
const COVER_INVALID: &str = "project_cover_invalid";

fn project_id(value: &str) -> Result<i64, &'static str> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0 && id.to_string() == value)
        .ok_or("project_invalid")
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Fields {
    note: Option<String>,
    description: Option<String>,
    is_pinned: bool,
    sort_order: f64,
}

impl From<ProjectMetadata> for Fields {
    fn from(value: ProjectMetadata) -> Self {
        Self {
            note: value.note,
            description: value.description,
            is_pinned: value.is_pinned,
            sort_order: value.sort_order,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Properties {
    fields: Fields,
    has_cover: bool,
}

fn directory(root: &std::path::Path, create: bool) -> Result<Option<PathBuf>, &'static str> {
    crate::profile_storage::private_directory(root, "project-covers", create)
        .map_err(|_| UNAVAILABLE)
}

fn require_registered(library: &ProjectLibrary, id: i64) -> Result<(), &'static str> {
    library
        .list()
        .map_err(library_error)?
        .iter()
        .any(|project| project.id == id)
        .then_some(())
        .ok_or(PROJECT_MISSING)
}

#[tauri::command]
pub(crate) async fn get_project_properties<R: Runtime>(
    app: AppHandle<R>,
    project: String,
) -> Result<Properties, &'static str> {
    let id = project_id(&project)?;
    with_library(app, false, move |library| {
        let library = library.ok_or(PROJECT_MISSING)?;
        require_registered(library, id)?;
        let fields = library
            .get_metadata(id)
            .map_err(library_error)?
            .map(Into::into)
            .unwrap_or(Fields {
                note: None,
                description: None,
                is_pinned: false,
                sort_order: 0.0,
            });
        Ok(Properties {
            fields,
            has_cover: library.get_cover(id).map_err(library_error)?.is_some(),
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn save_project_properties<R: Runtime>(
    app: AppHandle<R>,
    project: String,
    fields: Fields,
) -> Result<Fields, &'static str> {
    let id = project_id(&project)?;
    with_library(app, false, move |library| {
        library
            .ok_or(PROJECT_MISSING)?
            .set_metadata(
                id,
                fields.note.as_deref(),
                fields.description.as_deref(),
                fields.is_pinned,
                fields.sort_order,
            )
            .map(Into::into)
            .map_err(library_error)
    })
    .await
}

#[derive(Serialize)]
pub(crate) struct Cover {
    mime: &'static str,
    data: String,
}

impl Cover {
    fn from_bytes(format: CoverFormat, bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_COVER_BYTES {
            return Err(COVER_INVALID);
        }
        let mime = match format {
            CoverFormat::Png => "image/png",
            CoverFormat::Jpeg => "image/jpeg",
            CoverFormat::Webp => "image/webp",
        };
        Ok(Self {
            mime,
            data: STANDARD.encode(bytes),
        })
    }
}

#[tauri::command]
pub(crate) async fn read_project_cover<R: Runtime>(
    app: AppHandle<R>,
    project: String,
) -> Result<Option<Cover>, &'static str> {
    let id = project_id(&project)?;
    let root = app.state::<ProjectRegistrations>().app_data_dir(&app)?;
    with_library(app, false, move |library| {
        let library = library.ok_or(PROJECT_MISSING)?;
        let Some(record) = library.get_cover(id).map_err(library_error)? else {
            return Ok(None);
        };
        let Some(directory) = directory(&root, false)? else {
            return Ok(None);
        };
        library
            .read_cover(&directory, id)
            .map_err(library_error)?
            .map(|bytes| Cover::from_bytes(record.format, &bytes))
            .transpose()
    })
    .await
}

#[tauri::command]
pub(crate) async fn save_project_cover<R: Runtime>(
    app: AppHandle<R>,
    project: String,
    encoded: String,
) -> Result<Cover, &'static str> {
    let id = project_id(&project)?;
    if encoded.len() > MAX_COVER_BYTES.div_ceil(3) * 4 {
        return Err(COVER_INVALID);
    }
    let root = app.state::<ProjectRegistrations>().app_data_dir(&app)?;
    with_library(app, false, move |library| {
        let library = library.ok_or(PROJECT_MISSING)?;
        let bytes = STANDARD.decode(encoded).map_err(|_| COVER_INVALID)?;
        if bytes.is_empty() || bytes.len() > MAX_COVER_BYTES {
            return Err(COVER_INVALID);
        }
        require_registered(library, id)?;
        let directory = directory(&root, true)?.ok_or(UNAVAILABLE)?;
        let record =
            library
                .set_cover(&directory, id, None, &bytes)
                .map_err(|error| match error.kind {
                    uebox_project_library::ErrorKind::InvalidInput
                    | uebox_project_library::ErrorKind::Unsupported => COVER_INVALID,
                    _ => library_error(error),
                })?;
        Cover::from_bytes(record.format, &bytes)
    })
    .await
}

#[tauri::command]
pub(crate) async fn remove_project_cover<R: Runtime>(
    app: AppHandle<R>,
    project: String,
) -> Result<bool, &'static str> {
    let id = project_id(&project)?;
    let root = app.state::<ProjectRegistrations>().app_data_dir(&app)?;
    with_library(app, false, move |library| {
        let library = library.ok_or(PROJECT_MISSING)?;
        require_registered(library, id)?;
        let Some(directory) = directory(&root, false)? else {
            return Ok(false);
        };
        library.remove_cover(&directory, id).map_err(library_error)
    })
    .await
}
