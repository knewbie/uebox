//! Fixed native history operations only. No JavaScript evaluation or page IPC.
use super::{
    model::{Session, INVALID_SESSION, UNAVAILABLE},
    Action,
};
use std::sync::Arc;
#[cfg(any(target_os = "macos", windows))]
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
use tauri::{Runtime, WebviewWindow};

#[derive(Default)]
pub(super) struct History {
    pub supported: bool,
    pub back: bool,
    pub forward: bool,
    pub loading: Option<bool>,
}

pub(super) const fn supported() -> bool {
    cfg!(any(target_os = "macos", windows))
}

pub(super) async fn operate<R: Runtime>(
    window: &WebviewWindow<R>,
    session: Arc<Session>,
    action: Option<Action>,
) -> Result<History, &'static str> {
    if !session.is_live() {
        return Err(INVALID_SESSION);
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        if matches!(action, Some(Action::Back | Action::Forward)) {
            return Err("browser_history_unavailable");
        }
        if matches!(action, Some(Action::Reload)) {
            window.reload().map_err(|_| UNAVAILABLE)?;
        }
        return Ok(History::default());
    }
    #[cfg(any(target_os = "macos", windows))]
    {
        const TIMEOUT: Duration = Duration::from_secs(5);
        let deadline = Instant::now() + TIMEOUT;
        let (sender, receiver) = mpsc::sync_channel(1);
        window
            .with_webview(move |view| {
                let result = if !session.is_live() {
                    Err(INVALID_SESSION)
                } else if Instant::now() >= deadline {
                    Err(UNAVAILABLE)
                } else {
                    platform(view, action)
                };
                let _ = sender.send(result);
            })
            .map_err(|_| UNAVAILABLE)?;
        // Never wait for the UI thread from the UI thread. No locks cross await.
        tauri::async_runtime::spawn_blocking(move || receiver.recv_timeout(TIMEOUT))
            .await
            .map_err(|_| UNAVAILABLE)?
            .map_err(|_| UNAVAILABLE)?
    }
}

#[cfg(target_os = "macos")]
fn platform(
    view: tauri::webview::PlatformWebview,
    action: Option<Action>,
) -> Result<History, &'static str> {
    use std::ffi::{c_char, c_void, CStr};
    #[link(name = "objc")]
    extern "C" {
        fn sel_registerName(name: *const c_char) -> *const c_void;
        fn objc_msgSend();
    }
    // WKWebView is owned by Tauri. with_webview guarantees these selectors run
    // on its main thread; the signatures exactly match the WebKit properties.
    unsafe fn boolean(view: *mut c_void, name: &CStr) -> bool {
        let send: unsafe extern "C" fn(*mut c_void, *const c_void) -> bool =
            std::mem::transmute(objc_msgSend as *const ());
        send(view, sel_registerName(name.as_ptr()))
    }
    unsafe fn navigate(view: *mut c_void, name: &CStr) {
        let send: unsafe extern "C" fn(*mut c_void, *const c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let _ = send(view, sel_registerName(name.as_ptr()));
    }
    let view = view.inner();
    if view.is_null() {
        return Err(UNAVAILABLE);
    }
    unsafe {
        match action {
            Some(Action::Back) if !boolean(view, c"canGoBack") => return Err("browser_no_history"),
            Some(Action::Forward) if !boolean(view, c"canGoForward") => {
                return Err("browser_no_history")
            }
            Some(Action::Back) => navigate(view, c"goBack"),
            Some(Action::Forward) => navigate(view, c"goForward"),
            Some(Action::Reload) => navigate(view, c"reload"),
            None => {}
        }
        Ok(History {
            supported: true,
            back: boolean(view, c"canGoBack"),
            forward: boolean(view, c"canGoForward"),
            loading: Some(boolean(view, c"isLoading")),
        })
    }
}

#[cfg(windows)]
fn platform(
    view: tauri::webview::PlatformWebview,
    action: Option<Action>,
) -> Result<History, &'static str> {
    unsafe {
        let core = view.controller().CoreWebView2().map_err(|_| UNAVAILABLE)?;
        let mut back = Default::default();
        let mut forward = Default::default();
        core.CanGoBack(&mut back).map_err(|_| UNAVAILABLE)?;
        core.CanGoForward(&mut forward).map_err(|_| UNAVAILABLE)?;
        match action {
            Some(Action::Back) if !back.as_bool() => return Err("browser_no_history"),
            Some(Action::Forward) if !forward.as_bool() => return Err("browser_no_history"),
            Some(Action::Back) => core.GoBack().map_err(|_| UNAVAILABLE)?,
            Some(Action::Forward) => core.GoForward().map_err(|_| UNAVAILABLE)?,
            Some(Action::Reload) => core.Reload().map_err(|_| UNAVAILABLE)?,
            None => {}
        }
        core.CanGoBack(&mut back).map_err(|_| UNAVAILABLE)?;
        core.CanGoForward(&mut forward).map_err(|_| UNAVAILABLE)?;
        Ok(History {
            supported: true,
            back: back.as_bool(),
            forward: forward.as_bool(),
            loading: None,
        })
    }
}
