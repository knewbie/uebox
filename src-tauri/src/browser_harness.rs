//! Standalone Task28 command tests; does not launch a native browser.
mod browser;
mod desktop_browser;

pub fn configure_commands<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .manage(browser::BrowserHost::default())
        .invoke_handler(desktop_browser::restrict_invocation(
            tauri::generate_handler![
                desktop_browser::open_isolated_browser,
                browser::open_browser_window,
                browser::isolated_browser_status,
                browser::control_isolated_browser
            ],
        ))
}
