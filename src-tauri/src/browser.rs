//! Task28 manual browser controls. Keep desktop_browser::restrict_invocation
//! around the complete application handler; this window must never receive IPC.
pub(crate) mod download;
mod model;
mod native;
#[cfg(test)]
mod tests;

pub(crate) use model::BrowserHost;
use model::{approved_url, Session, INVALID_SESSION, UNAVAILABLE};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{
    webview::{DownloadEvent, NewWindowResponse, PageLoadEvent},
    Manager, Runtime, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

const LABEL: &str = crate::desktop_browser::BROWSER_LABEL;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Action {
    Back,
    Forward,
    Reload,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserStatus {
    session_id: Option<String>,
    open: bool,
    opening: bool,
    address: Option<String>,
    origin: Option<String>,
    loading: bool,
    can_go_back: bool,
    can_go_forward: bool,
    history_supported: bool,
    navigation_blocked: bool,
    download_blocked: bool,
    downloads_enabled: bool,
}

impl BrowserStatus {
    fn closed() -> Self {
        Self {
            session_id: None,
            open: false,
            opening: false,
            address: None,
            origin: None,
            loading: false,
            can_go_back: false,
            can_go_forward: false,
            history_supported: native::supported(),
            navigation_blocked: false,
            download_blocked: false,
            downloads_enabled: false,
        }
    }

    fn opening(session: &Session, open: bool) -> Self {
        Self {
            session_id: Some(session.id.to_string()),
            open,
            opening: true,
            origin: Some(session.initial.origin().ascii_serialization()),
            loading: true,
            ..Self::closed()
        }
    }
}

fn authorize<R: Runtime>(window: &WebviewWindow<R>) -> Result<(), &'static str> {
    if window.label() == "main" {
        Ok(())
    } else {
        Err(UNAVAILABLE)
    }
}

async fn snapshot<R: Runtime>(
    browser: &WebviewWindow<R>,
    session: Arc<Session>,
    action: Option<Action>,
) -> Result<BrowserStatus, &'static str> {
    let before = browser.url().map_err(|_| UNAVAILABLE)?;
    if before.as_str() == "about:blank" && !session.flags()?.page_started && action.is_none() {
        return Ok(BrowserStatus::opening(&session, true));
    }
    // A native address is still checked against the immutable session policy.
    if !session.accepts(&before) {
        return Err(UNAVAILABLE);
    }
    let history = native::operate(browser, session.clone(), action).await?;
    if !session.is_live() {
        return Err(INVALID_SESSION);
    }
    let address = browser.url().map_err(|_| UNAVAILABLE)?;
    if !session.accepts(&address) {
        return Err(UNAVAILABLE);
    }
    let flags = session.flags()?;
    Ok(BrowserStatus {
        session_id: Some(session.id.to_string()),
        open: true,
        opening: flags.opening,
        address: Some(address.to_string()),
        origin: Some(session.initial.origin().ascii_serialization()),
        loading: history.loading.unwrap_or(flags.loading),
        can_go_back: history.back,
        can_go_forward: history.forward,
        history_supported: history.supported,
        navigation_blocked: flags.navigation_blocked,
        download_blocked: flags.download_blocked,
        downloads_enabled: true,
    })
}

#[tauri::command]
pub(crate) async fn isolated_browser_status<R: Runtime>(
    window: WebviewWindow<R>,
    host: State<'_, BrowserHost>,
) -> Result<BrowserStatus, &'static str> {
    authorize(&window)?;
    let Some(session) = host.current()? else {
        return Ok(BrowserStatus::closed());
    };
    if let Some(browser) = window.app_handle().get_webview_window(LABEL) {
        return snapshot(&browser, session, None).await;
    }
    if session.flags()?.opening {
        return Ok(BrowserStatus::opening(&session, false));
    }
    host.clear(session.id)?;
    Ok(BrowserStatus::closed())
}

#[tauri::command]
pub(crate) async fn open_browser_window<R: Runtime>(
    window: WebviewWindow<R>,
    host: State<'_, BrowserHost>,
    address: String,
) -> Result<BrowserStatus, &'static str> {
    authorize(&window)?;
    let url = approved_url(&address)?;
    let app = window.app_handle();
    if app.get_webview_window(LABEL).is_some() {
        return Err("browser_already_open");
    }
    if let Some(previous) = host.current()? {
        if !previous.flags()?.opening {
            host.clear(previous.id)?;
        }
    }
    let session = host.reserve(url)?;
    let browser = match build_window(app, &session) {
        Ok(browser) => browser,
        Err(error) => {
            host.clear(session.id)?;
            return Err(error);
        }
    };
    session.opened();
    let owner = app.clone();
    let id = session.id;
    browser.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            let _ = owner.state::<BrowserHost>().clear(id);
        }
    });
    // Creation accepts a request; it does not prove the external page has loaded.
    Ok(BrowserStatus::opening(&session, true))
}

fn build_window<R: Runtime>(
    app: &tauri::AppHandle<R>,
    session: &Arc<Session>,
) -> Result<WebviewWindow<R>, &'static str> {
    let navigation = session.clone();
    let loading = session.clone();
    let download = session.clone();
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::External(session.initial.clone()))
        .title(session.initial.host_str().ok_or(UNAVAILABLE)?)
        .inner_size(1040., 740.)
        .min_inner_size(400., 360.)
        .incognito(true)
        .devtools(false)
        .on_navigation(move |url| navigation.navigation(url))
        .on_page_load(move |_, payload| {
            loading.page_load(
                payload.url(),
                matches!(payload.event(), PageLoadEvent::Started),
            )
        })
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .on_download(move |_, event| {
            if matches!(event, DownloadEvent::Requested { .. }) {
                download.block_download();
            }
            false
        })
        .build()
        .map_err(|_| UNAVAILABLE)
}

#[tauri::command]
pub(crate) async fn control_isolated_browser<R: Runtime>(
    window: WebviewWindow<R>,
    host: State<'_, BrowserHost>,
    session_id: String,
    action: Action,
) -> Result<BrowserStatus, &'static str> {
    authorize(&window)?;
    let session = host.matching(&session_id)?;
    let browser = window
        .app_handle()
        .get_webview_window(LABEL)
        .ok_or(INVALID_SESSION)?;
    snapshot(&browser, session, Some(action)).await
}
