//! Desktop window and opt-in tray behavior.
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;
use tauri::{
    menu::{Menu, MenuEvent, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, Runtime, Window, WindowEvent,
};

const MAIN_WINDOW: &str = "main";
const TRAY: &str = "uebox-desktop-tray";
const SHOW: &str = "desktop-show";
const QUIT: &str = "desktop-quit";
const UNAVAILABLE: &str = "desktop_tray_unavailable";

#[derive(Default)]
pub(crate) struct DesktopState {
    tray_active: AtomicBool,
}

#[derive(Deserialize)]
pub(crate) enum DesktopLanguage {
    #[serde(rename = "en-US")]
    English,
    #[serde(rename = "zh-CN")]
    Chinese,
}

impl DesktopLanguage {
    fn show_label(&self) -> &'static str {
        match self {
            Self::English => "Show Unreal Box",
            Self::Chinese => "显示虚幻盒子",
        }
    }

    fn quit_label(&self) -> &'static str {
        match self {
            Self::English => "Quit Unreal Box",
            Self::Chinese => "退出虚幻盒子",
        }
    }
}

pub(crate) fn reveal<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn menu<R: Runtime>(
    app: &AppHandle<R>,
    language: DesktopLanguage,
) -> Result<Menu<R>, tauri::Error> {
    let show = MenuItem::with_id(app, SHOW, language.show_label(), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, QUIT, language.quit_label(), true, None::<&str>)?;
    Menu::with_items(app, &[&show, &quit])
}

// The application only hides its primary window after the user enables the
// tray. Other windows, including isolated browser windows, retain normal close behavior.
pub(crate) fn window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    if window.label() != MAIN_WINDOW {
        return;
    }
    let Some(state) = window.try_state::<DesktopState>() else {
        return;
    };
    if let WindowEvent::CloseRequested { api, .. } = event {
        if state.tray_active.load(Ordering::Acquire) && window.hide().is_ok() {
            api.prevent_close();
        }
    }
}

pub(crate) fn menu_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    let state = app.state::<DesktopState>();
    if !state.tray_active.load(Ordering::Acquire) {
        return;
    }
    match event.id.as_ref() {
        SHOW => reveal(app),
        QUIT => {
            state.tray_active.store(false, Ordering::Release);
            app.exit(0);
        }
        _ => {}
    }
}

#[tauri::command]
pub(crate) fn desktop_tray<R: Runtime>(app: AppHandle<R>) -> bool {
    app.state::<DesktopState>()
        .tray_active
        .load(Ordering::Acquire)
}

#[tauri::command]
pub(crate) async fn set_desktop_tray<R: Runtime>(
    app: AppHandle<R>,
    enabled: bool,
    language: DesktopLanguage,
) -> Result<bool, &'static str> {
    let (reply, mut result) = tauri::async_runtime::channel(1);
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let _ = reply.try_send(apply_tray(&handle, enabled, language));
    })
    .map_err(|_| UNAVAILABLE)?;
    result.recv().await.ok_or(UNAVAILABLE)?
}

fn apply_tray<R: Runtime>(
    app: &AppHandle<R>,
    enabled: bool,
    language: DesktopLanguage,
) -> Result<bool, &'static str> {
    let state = app.state::<DesktopState>();
    if enabled == state.tray_active.load(Ordering::Acquire) {
        return Ok(enabled);
    }
    if !enabled {
        state.tray_active.store(false, Ordering::Release);
        app.remove_tray_by_id(TRAY);
        reveal(app);
        return Ok(false);
    }

    let menu = menu(app, language).map_err(|_| UNAVAILABLE)?;
    let icon = app.default_window_icon().ok_or(UNAVAILABLE)?.clone();
    TrayIconBuilder::with_id(TRAY)
        .icon(icon)
        .menu(&menu)
        .build(app)
        .map_err(|_| UNAVAILABLE)?;
    state.tray_active.store(true, Ordering::Release);
    Ok(true)
}
