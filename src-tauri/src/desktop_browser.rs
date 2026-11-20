//! A dedicated browser window with an explicit navigation origin.
use tauri::{
    ipc::Invoke, webview::NewWindowResponse, Manager, Runtime, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
use url::Url;

pub(crate) const BROWSER_LABEL: &str = "uebox-isolated-browser";
const UNAVAILABLE: &str = "browser_unavailable";

pub(crate) fn restrict_invocation<R: Runtime>(
    next: impl Fn(Invoke<R>) -> bool + Send + Sync + 'static,
) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static {
    move |message| {
        if message.message.webview().label() == BROWSER_LABEL {
            message.resolver.reject("browser_ipc_unavailable");
            return true;
        }
        next(message)
    }
}

fn approved_url(value: &str) -> Result<Url, &'static str> {
    const INVALID: &str = "browser_invalid_url";
    if value.len() > 2048 || value.chars().any(char::is_control) {
        return Err(INVALID);
    }
    let url = Url::parse(value).map_err(|_| INVALID)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(INVALID);
    }
    Ok(url)
}

#[tauri::command]
pub(crate) async fn open_isolated_browser<R: Runtime>(
    window: WebviewWindow<R>,
    address: String,
) -> Result<(), &'static str> {
    if window.label() != "main" {
        return Err(UNAVAILABLE);
    }
    let url = approved_url(&address)?;
    let origin = url.origin();
    let app = window.app_handle();
    if app.get_webview_window(BROWSER_LABEL).is_some() {
        return Err("browser_already_open");
    }
    let title = url.host_str().ok_or(UNAVAILABLE)?.to_owned();
    WebviewWindowBuilder::new(app, BROWSER_LABEL, WebviewUrl::External(url))
        .title(title)
        .inner_size(1040., 740.)
        .min_inner_size(400., 360.)
        .incognito(true)
        .devtools(false)
        .on_navigation(move |next| next.origin() == origin && approved_url(next.as_str()).is_ok())
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .on_download(|_, _| false)
        .build()
        .map(|_| ())
        .map_err(|_| UNAVAILABLE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_requires_the_same_https_origin_with_no_embedded_credentials() {
        let start = approved_url("https://fixture.example/path").unwrap();
        let next = approved_url("https://fixture.example:443/next?q=tool").unwrap();
        assert_eq!(start.origin(), next.origin());
        let different = approved_url("https://fixture.example:444/path").unwrap();
        assert_ne!(start.origin(), different.origin());
        for forbidden in [
            "http://fixture.example/",
            "tauri://localhost/",
            "file:///etc/passwd",
            "javascript:example()",
            "https://user:pass@fixture.example/",
        ] {
            assert_eq!(approved_url(forbidden), Err("browser_invalid_url"));
        }
    }
}

#[cfg(test)]
mod ipc_tests {
    use super::*;
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};

    #[test]
    fn isolated_browser_label_has_no_access_to_application_commands() {
        let app = crate::configure_commands(mock_builder())
            .build(mock_context(noop_assets()))
            .unwrap();
        let browser = WebviewWindowBuilder::new(&app, BROWSER_LABEL, WebviewUrl::default())
            .build()
            .unwrap();
        let response = get_ipc_response(
            &browser,
            tauri::webview::InvokeRequest {
                cmd: "host_status".into(),
                callback: CallbackFn(0),
                error: CallbackFn(1),
                url: if cfg!(windows) {
                    "http://tauri.localhost"
                } else {
                    "tauri://localhost"
                }
                .parse()
                .unwrap(),
                body: InvokeBody::Json(serde_json::json!({})),
                headers: Default::default(),
                invoke_key: INVOKE_KEY.into(),
            },
        );
        assert!(matches!(response, Err(error) if error == "browser_ipc_unavailable"));
    }
}
