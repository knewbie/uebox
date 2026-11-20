mod storage;
#[cfg(test)]
mod tests;
mod transfer;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};
use uebox_project_library::{EngineKind, ErrorKind, LibraryError, ProjectLibrary, ProjectRecord};

#[derive(Default)]
pub(crate) struct ProjectRegistrations {
    library: Mutex<Option<ProjectLibrary>>,
    #[cfg(test)]
    test_data_dir: std::sync::OnceLock<PathBuf>,
}

impl ProjectRegistrations {
    #[cfg(test)]
    pub(crate) fn set_test_data_dir(&self, path: PathBuf) {
        self.test_data_dir.set(path).unwrap();
    }

    fn app_data_dir<R: Runtime>(&self, app: &AppHandle<R>) -> Result<PathBuf, &'static str> {
        #[cfg(test)]
        if let Some(path) = self.test_data_dir.get() {
            return Ok(path.clone());
        }
        app.path()
            .app_local_data_dir()
            .map_err(|_| "project_library_unavailable")
    }
}

#[derive(Serialize)]
pub(crate) struct RegisteredProject {
    id: String,
    engine: &'static str,
    name: String,
    path: PathBuf,
}

impl From<ProjectRecord> for RegisteredProject {
    fn from(project: ProjectRecord) -> Self {
        Self {
            id: project.id.to_string(),
            engine: match project.engine {
                EngineKind::Unreal => "unreal",
                EngineKind::Unity => "unity",
                EngineKind::Godot => "godot",
            },
            name: project.name,
            path: project.path,
        }
    }
}

fn library_error(error: LibraryError) -> &'static str {
    match error.kind {
        ErrorKind::Unsupported | ErrorKind::Corrupt => "project_library_invalid",
        ErrorKind::Locked | ErrorKind::AlreadyExists => "project_library_locked",
        ErrorKind::Duplicate => "project_duplicate",
        ErrorKind::InvalidInput => "project_invalid",
        ErrorKind::Missing => "project_missing",
        ErrorKind::Io | ErrorKind::Other => "project_library_unavailable",
    }
}

async fn with_storage<R, T>(
    app: AppHandle<R>,
    create: bool,
    operation: impl FnOnce(Option<&Path>, &mut Option<ProjectLibrary>) -> Result<T, &'static str>
        + Send
        + 'static,
) -> Result<T, &'static str>
where
    R: Runtime,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let registrations = app.state::<ProjectRegistrations>();
        let mut library = registrations
            .library
            .lock()
            .map_err(|_| "project_library_unavailable")?;
        let data_dir = registrations.app_data_dir(&app)?;
        let storage = storage::lock_storage(&data_dir, create)?;
        let Some(storage) = storage else {
            return if library.is_none() {
                operation(None, &mut library)
            } else {
                Err("project_library_invalid")
            };
        };

        if library.is_none() {
            match ProjectLibrary::open_existing(storage.database()) {
                Ok(existing) => *library = Some(existing),
                Err(error) if error.kind == ErrorKind::Missing => {}
                Err(error) => return Err(library_error(error)),
            }
        }
        operation(Some(storage.database()), &mut library)
    })
    .await
    .map_err(|_| "project_library_unavailable")?
}

async fn with_library<R, T>(
    app: AppHandle<R>,
    create: bool,
    operation: impl FnOnce(Option<&mut ProjectLibrary>) -> Result<T, &'static str> + Send + 'static,
) -> Result<T, &'static str>
where
    R: Runtime,
    T: Send + 'static,
{
    with_storage(app, create, move |path, library| {
        if create && library.is_none() {
            let path = path.ok_or("project_library_unavailable")?;
            *library = Some(ProjectLibrary::create_new(path).map_err(library_error)?);
        }
        operation(library.as_mut())
    })
    .await
}

fn parse_engine(value: &str) -> Option<EngineKind> {
    match value {
        "unreal" => Some(EngineKind::Unreal),
        "unity" => Some(EngineKind::Unity),
        "godot" => Some(EngineKind::Godot),
        _ => None,
    }
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportedProjects {
    inserted: usize,
    already_registered: usize,
    duplicate_in_file: usize,
}

#[tauri::command]
pub(crate) async fn import_registered_projects<R: Runtime>(
    app: AppHandle<R>,
    manifest: String,
) -> Result<ImportedProjects, &'static str> {
    let records = transfer::parse(&manifest)?;
    if records.is_empty() {
        return Ok(ImportedProjects::default());
    }
    with_storage(app, true, move |path, library| {
        let summary = if let Some(existing) = library.as_mut() {
            existing.import_batch(&records).map_err(library_error)?
        } else {
            let path = path.ok_or("project_library_unavailable")?;
            let (created, summary) =
                ProjectLibrary::create_with_import(path, &records).map_err(library_error)?;
            *library = Some(created);
            summary
        };
        Ok(ImportedProjects {
            inserted: summary.inserted,
            already_registered: summary.already_registered,
            duplicate_in_file: summary.duplicate_in_file,
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn list_registered_projects<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Vec<RegisteredProject>, &'static str> {
    with_library(app, false, |library| {
        let Some(library) = library else {
            return Ok(Vec::new());
        };
        let records = library.list().map_err(library_error)?;
        Ok(records.into_iter().map(RegisteredProject::from).collect())
    })
    .await
}

#[tauri::command]
pub(crate) async fn register_project<R: Runtime>(
    app: AppHandle<R>,
    engine: String,
    name: String,
    path: String,
) -> Result<RegisteredProject, &'static str> {
    let engine = parse_engine(&engine).ok_or("project_invalid")?;
    if name.trim().is_empty()
        || name.contains('\0')
        || path.contains('\0')
        || !Path::new(&path).is_absolute()
    {
        return Err("project_invalid");
    }
    with_library(app, true, move |library| {
        let library = library.ok_or("project_library_unavailable")?;
        library
            .register(engine, &name, path)
            .map(RegisteredProject::from)
            .map_err(library_error)
    })
    .await
}

#[tauri::command]
pub(crate) async fn remove_registered_project<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> Result<bool, &'static str> {
    let id = id
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0 && value.to_string() == id)
        .ok_or("project_invalid")?;
    with_library(app, false, move |library| match library {
        Some(library) => library.remove(id).map_err(library_error),
        None => Ok(false),
    })
    .await
}
