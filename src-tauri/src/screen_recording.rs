//! Explicit, main-window-only recording. Parent integration registers these three
//! commands and links native/macos/capture.m; no managed state is required.
mod files;
mod native;
mod state;
#[cfg(test)]
mod tests;

use files::Workspace;
use serde::Serialize;
use state::{authorize, check_start, parse_token, Machine, Snapshot, UNAVAILABLE};
use std::{
    ffi::CString,
    sync::{Mutex, MutexGuard, OnceLock},
};
use tauri::{Manager, Runtime, WebviewWindow};
use tauri_plugin_opener::OpenerExt;

const DIRECTORY: &str = "screen-recordings";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScreenRecordingStatus {
    supported: bool,
    token: Option<String>,
    phase: &'static str,
    audio: bool,
    file_name: Option<String>,
    error: Option<&'static str>,
    cleanup_pending: bool,
}

impl ScreenRecordingStatus {
    fn from_snapshot(snapshot: Snapshot, supported: bool) -> Self {
        Self {
            supported,
            token: snapshot.token.map(|token| token.to_string()),
            phase: snapshot.phase.as_str(),
            audio: snapshot.audio,
            file_name: snapshot.file_name,
            error: snapshot.error,
            cleanup_pending: snapshot.cleanup_pending,
        }
    }
}

fn machine() -> Result<MutexGuard<'static, Machine>, &'static str> {
    static MACHINE: OnceLock<Mutex<Machine>> = OnceLock::new();
    MACHINE
        .get_or_init(|| Mutex::new(Machine::default()))
        .lock()
        .map_err(|_| UNAVAILABLE)
}

#[tauri::command]
pub(crate) async fn screen_recording_status<R: Runtime>(
    window: WebviewWindow<R>,
) -> Result<ScreenRecordingStatus, &'static str> {
    authorize(window.label())?;
    let state = machine()?;
    Ok(ScreenRecordingStatus::from_snapshot(
        state.snapshot(),
        native::available(),
    ))
}

#[tauri::command]
pub(crate) async fn start_screen_recording<R: Runtime>(
    window: WebviewWindow<R>,
    opt_in: bool,
    audio: bool,
) -> Result<ScreenRecordingStatus, &'static str> {
    authorize(window.label())?;
    let mut state = machine()?;
    let supported = native::available();
    check_start(window.label(), opt_in, supported, state.busy())?;
    let app_local = window
        .app_handle()
        .path()
        .app_local_data_dir()
        .map_err(|_| UNAVAILABLE)?;
    let root = crate::profile_storage::private_directory(&app_local, DIRECTORY, true)
        .map_err(|_| UNAVAILABLE)?
        .ok_or(UNAVAILABLE)?;
    let mut nonce = [0; 16];
    getrandom::getrandom(&mut nonce).map_err(|_| UNAVAILABLE)?;
    let work = Workspace::prepare(&root, nonce)?;
    // The native ABI copies the UTF-8 path before returning. Never lossy-convert
    // a filesystem path or accept a destination/name from the frontend.
    let path = CString::new(work.path().to_str().ok_or(UNAVAILABLE)?).map_err(|_| UNAVAILABLE)?;
    let token = state.begin(work, audio)?;
    // Hold the same lock through both FFI dispatches: stop cannot enqueue before
    // start. capture.m always dispatches callbacks asynchronously on main_queue.
    native::start(&path, audio, token, recording_event);
    Ok(ScreenRecordingStatus::from_snapshot(
        state.snapshot(),
        supported,
    ))
}

#[tauri::command]
pub(crate) async fn stop_screen_recording<R: Runtime>(
    window: WebviewWindow<R>,
    token: String,
) -> Result<ScreenRecordingStatus, &'static str> {
    authorize(window.label())?;
    let token = parse_token(&token)?;
    let mut state = machine()?;
    if state.stop(token)? {
        native::stop(token);
    }
    Ok(ScreenRecordingStatus::from_snapshot(
        state.snapshot(),
        native::available(),
    ))
}

#[tauri::command]
pub(crate) async fn reveal_screen_recording<R: Runtime>(
    window: WebviewWindow<R>,
    token: String,
) -> Result<(), &'static str> {
    authorize(window.label())?;
    let token = parse_token(&token)?;
    let snapshot = machine()?.snapshot();
    if snapshot.token != Some(token) {
        return Err(UNAVAILABLE);
    }
    let file = snapshot.file_name.ok_or(UNAVAILABLE)?;
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let root = app.path().app_local_data_dir().map_err(|_| UNAVAILABLE)?;
        let path = root.join(DIRECTORY).join(file);
        if !crate::profile_storage::checked_file_exists(&path).map_err(|_| UNAVAILABLE)? {
            return Err(UNAVAILABLE);
        }
        app.opener()
            .reveal_item_in_dir(path)
            .map_err(|_| UNAVAILABLE)
    })
    .await
    .map_err(|_| UNAVAILABLE)?
}

#[cfg(target_os = "macos")]
pub(crate) fn stop_for_shutdown() -> bool {
    let Ok(mut state) = machine() else {
        return false;
    };
    if !state.busy() {
        return false;
    }
    if let Some(token) = state.snapshot().token {
        if state.stop(token) == Ok(true) {
            native::stop(token);
        }
    }
    true
}

extern "C" fn recording_event(token: u64, event: std::ffi::c_int) {
    let _ = std::panic::catch_unwind(|| {
        if matches!(event, 1 | 2) {
            handle_event(token, event);
        } else {
            // Publication syncs the finished video. Keep it off the native UI thread.
            tauri::async_runtime::spawn_blocking(move || handle_event(token, event));
        }
    });
}

fn handle_event(token: u64, event: std::ffi::c_int) {
    if let Ok(mut state) = machine() {
        state.event(token, event);
    }
}
