//! Open a registered project folder through the operating system.
use tauri::{AppHandle, Runtime};
use tauri_plugin_opener::OpenerExt;

use crate::project_library::with_library;

#[tauri::command]
pub(crate) async fn open_registered_project_folder<R: Runtime>(
    app: AppHandle<R>,
    project_id: String,
) -> Result<(), &'static str> {
    let id = project_id
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0 && id.to_string() == project_id)
        .ok_or("project_invalid")?;

    let path = with_library(app.clone(), false, move |library| {
        library
            .ok_or("project_missing")?
            .list()
            .map_err(|_| "project_library_unavailable")?
            .into_iter()
            .find(|record| record.id == id)
            .map(|record| record.path)
            .ok_or("project_missing")
    })
    .await?;
    tauri::async_runtime::spawn_blocking(move || {
        let path = path.canonicalize().map_err(|_| "project_offline")?;
        if !path.is_dir() {
            return Err("project_offline");
        }
        let directory = path
            .into_os_string()
            .into_string()
            .map_err(|_| "project_invalid")?;
        app.opener()
            .open_path(directory, None::<&str>)
            .map_err(|_| "project_folder_unavailable")
    })
    .await
    .map_err(|_| "project_folder_unavailable")?
}
