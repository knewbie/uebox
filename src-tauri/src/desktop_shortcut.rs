//! An explicitly configured, process-local shortcut for the main desktop window.
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime, WebviewWindow};
use tauri_plugin_global_shortcut::{Code, GlobalShortcut, Modifiers, Shortcut, ShortcutState};

const UNAVAILABLE: &str = "desktop_shortcut_unavailable";
const OCCUPIED: &str = "desktop_shortcut_occupied";

#[derive(Default)]
pub(crate) struct ShortcutPreference(Mutex<Option<WindowShortcut>>);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WindowShortcut {
    control: bool,
    option: bool,
    shift: bool,
    command: bool,
    key: ShortcutKey,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum ShortcutKey {
    Backquote,
    Space,
    KeyA,
    KeyB,
    KeyC,
    KeyD,
    KeyE,
    KeyF,
    KeyG,
    KeyH,
    KeyI,
    KeyJ,
    KeyK,
    KeyL,
    KeyM,
    KeyN,
    KeyO,
    KeyP,
    KeyQ,
    KeyR,
    KeyS,
    KeyT,
    KeyU,
    KeyV,
    KeyW,
    KeyX,
    KeyY,
    KeyZ,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
}

impl WindowShortcut {
    fn native(self) -> Result<Shortcut, &'static str> {
        if !(self.control || self.option || self.command) {
            return Err("desktop_shortcut_invalid");
        }
        let mut modifiers = Modifiers::empty();
        modifiers.set(Modifiers::CONTROL, self.control);
        modifiers.set(Modifiers::ALT, self.option);
        modifiers.set(Modifiers::SHIFT, self.shift);
        modifiers.set(Modifiers::SUPER, self.command);
        // ShortcutKey is the closed set of keyboard codes accepted by the settings UI.
        let key = format!("{:?}", self.key)
            .parse::<Code>()
            .map_err(|_| "desktop_shortcut_invalid")?;
        Ok(Shortcut::new(Some(modifiers), key))
    }
}

fn toggle_main<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or_default() && window.is_focused().unwrap_or_default() {
        let _ = window.hide();
    } else {
        crate::desktop::reveal(app);
    }
}

#[tauri::command]
pub(crate) fn desktop_window_shortcut<R: Runtime>(
    window: WebviewWindow<R>,
) -> Result<Option<WindowShortcut>, &'static str> {
    if window.label() != "main" {
        return Err(UNAVAILABLE);
    }
    let state = window.state::<ShortcutPreference>();
    let choice = state.0.lock().map_err(|_| UNAVAILABLE)?;
    Ok(*choice)
}

#[tauri::command]
pub(crate) async fn set_desktop_window_shortcut<R: Runtime>(
    window: WebviewWindow<R>,
    shortcut: Option<WindowShortcut>,
) -> Result<Option<WindowShortcut>, &'static str> {
    if window.label() != "main" {
        return Err(UNAVAILABLE);
    }
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let global = app.try_state::<GlobalShortcut<R>>().ok_or(UNAVAILABLE)?;
        let state = app.state::<ShortcutPreference>();
        let mut choice = state.0.lock().map_err(|_| UNAVAILABLE)?;
        if *choice == shortcut {
            return Ok(*choice);
        }
        let previous = choice.map(WindowShortcut::native).transpose()?;
        let next = shortcut.map(WindowShortcut::native).transpose()?;
        if let Some(next) = next {
            global
                .on_shortcut(next, |app, _, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_main(app);
                    }
                })
                .map_err(|_| OCCUPIED)?;
        }
        if let Some(previous) = previous {
            if global.unregister(previous).is_err() {
                if let Some(next) = next {
                    let _ = global.unregister(next);
                }
                return Err(UNAVAILABLE);
            }
        }
        *choice = shortcut;
        Ok(*choice)
    })
    .await
    .map_err(|_| UNAVAILABLE)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmodified_and_shift_only_keys_cannot_be_registered_globally() {
        let mut choice = WindowShortcut {
            control: false,
            option: false,
            shift: false,
            command: false,
            key: ShortcutKey::Backquote,
        };
        assert!(choice.native().is_err());
        choice.shift = true;
        assert!(choice.native().is_err());
        choice.option = true;
        assert!(choice.native().is_ok());
    }
}
