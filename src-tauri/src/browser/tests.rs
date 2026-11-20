use super::*;
use tauri::{
    ipc::{CallbackFn, InvokeBody},
    test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY},
};

#[test]
fn only_same_https_origin_is_allowed_including_scheme_and_port() {
    let host = BrowserHost::default();
    let session = host
        .reserve(approved_url("https://fixture.example/one").unwrap())
        .unwrap();
    assert!(session.navigation(&approved_url("https://fixture.example:443/two").unwrap()));
    for address in [
        "https://fixture.example:444/",
        "https://other.example/",
        "https://fixture.example.evil/",
    ] {
        assert!(!session.navigation(&approved_url(address).unwrap()));
    }
    for address in [
        "http://fixture.example/",
        "file:///tmp/fixture",
        "javascript:alert(1)",
        "tauri://localhost/",
        "https://user:secret@fixture.example/",
        "https://fixture.example/\u{0085}",
    ] {
        assert!(approved_url(address).is_err(), "{address}");
    }
    assert!(approved_url(&format!("https://fixture.example/{}", "x".repeat(2048))).is_err());
    assert!(session.flags().unwrap().navigation_blocked);
}

#[test]
fn stale_windows_cannot_clear_or_control_the_new_session() {
    let host = BrowserHost::default();
    let first = host
        .reserve(approved_url("https://fixture.example/one").unwrap())
        .unwrap();
    assert!(matches!(
        host.reserve(approved_url("https://fixture.example/two").unwrap()),
        Err("browser_already_open")
    ));
    host.clear(first.id).unwrap();
    let second = host
        .reserve(approved_url("https://fixture.example/two").unwrap())
        .unwrap();
    assert_ne!(first.id, second.id);
    host.clear(first.id).unwrap();
    assert_eq!(host.current().unwrap().unwrap().id, second.id);
    assert!(matches!(
        host.matching(&first.id.to_string()),
        Err("browser_invalid_session")
    ));
    first.block_download();
    assert!(!second.flags().unwrap().download_blocked);
    for invalid in ["", "0", "01", "+2", " 2", "18446744073709551616"] {
        assert!(matches!(
            host.matching(invalid),
            Err("browser_invalid_session")
        ));
    }
}

#[test]
fn late_page_completion_cannot_mark_a_new_navigation_as_finished() {
    let host = BrowserHost::default();
    let one = approved_url("https://fixture.example/one").unwrap();
    let two = approved_url("https://fixture.example/two").unwrap();
    let session = host.reserve(one.clone()).unwrap();
    session.page_load(&one, true);
    session.page_load(&two, true);
    session.page_load(&one, false);
    assert!(session.flags().unwrap().loading);
    session.page_load(&two, false);
    assert!(!session.flags().unwrap().loading);
    session.page_load(&approved_url("https://other.example/").unwrap(), false);
    assert!(!session.flags().unwrap().loading);
}

#[test]
fn ipc_rejects_every_browser_command_from_untrusted_windows_without_opening_a_page() {
    let app = crate::configure_commands(mock_builder())
        .build(mock_context(noop_assets()))
        .unwrap();
    let request = |cmd: &str, body: serde_json::Value| tauri::webview::InvokeRequest {
        cmd: cmd.into(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url: if cfg!(windows) {
            "http://tauri.localhost"
        } else {
            "tauri://localhost"
        }
        .parse()
        .unwrap(),
        body: InvokeBody::Json(body),
        headers: Default::default(),
        invoke_key: INVOKE_KEY.into(),
    };
    for label in ["uebox-isolated-browser", "secondary"] {
        let window = WebviewWindowBuilder::new(&app, label, WebviewUrl::default())
            .build()
            .unwrap();
        for (cmd, args) in [
            ("isolated_browser_status", serde_json::json!({})),
            (
                "open_browser_window",
                serde_json::json!({"address":"https://fixture.example/"}),
            ),
            (
                "control_isolated_browser",
                serde_json::json!({"sessionId":"1","action":"reload"}),
            ),
        ] {
            let error = get_ipc_response(&window, request(cmd, args.clone())).unwrap_err();
            assert_eq!(
                error,
                if label == "uebox-isolated-browser" {
                    "browser_ipc_unavailable"
                } else {
                    "browser_unavailable"
                }
            );
            let mut remote = request(cmd, args);
            remote.url = "https://fixture.example/".parse().unwrap();
            assert!(get_ipc_response(&window, remote).is_err());
        }
    }
    assert!(app.state::<BrowserHost>().current().unwrap().is_none());
}

#[test]
fn main_status_is_inert_and_invalid_urls_and_sessions_have_no_side_effects() {
    let app = crate::configure_commands(mock_builder())
        .build(mock_context(noop_assets()))
        .unwrap();
    let main = WebviewWindowBuilder::new(&app, "main", WebviewUrl::default())
        .build()
        .unwrap();
    let request = |cmd: &str, args: serde_json::Value| tauri::webview::InvokeRequest {
        cmd: cmd.into(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url: if cfg!(windows) {
            "http://tauri.localhost"
        } else {
            "tauri://localhost"
        }
        .parse()
        .unwrap(),
        body: InvokeBody::Json(args),
        headers: Default::default(),
        invoke_key: INVOKE_KEY.into(),
    };
    let response = get_ipc_response(
        &main,
        request("isolated_browser_status", serde_json::json!({})),
    )
    .unwrap();
    let status: serde_json::Value = response.deserialize().unwrap();
    assert_eq!(status["open"], false);
    assert_eq!(status["sessionId"], serde_json::Value::Null);
    assert_eq!(status["downloadsEnabled"], false);
    for address in [
        "file:///tmp/fixture",
        "javascript:alert(1)",
        "https://user:secret@fixture.example/",
    ] {
        assert_eq!(
            get_ipc_response(
                &main,
                request(
                    "open_browser_window",
                    serde_json::json!({"address":address})
                )
            )
            .unwrap_err(),
            "browser_invalid_url"
        );
    }
    assert_eq!(
        get_ipc_response(
            &main,
            request(
                "control_isolated_browser",
                serde_json::json!({"sessionId":"1","action":"reload"})
            )
        )
        .unwrap_err(),
        "browser_invalid_session"
    );
    assert!(app.get_webview_window("uebox-isolated-browser").is_none());
    for action in ["eval", "navigate", "download", "javascript:alert(1)"] {
        assert!(serde_json::from_value::<Action>(serde_json::json!(action)).is_err());
    }
}

#[test]
fn opening_reports_pending_instead_of_reading_a_page_before_native_navigation_starts() {
    let app = crate::configure_commands(mock_builder())
        .build(mock_context(noop_assets()))
        .unwrap();
    let main = WebviewWindowBuilder::new(&app, "main", WebviewUrl::default())
        .build()
        .unwrap();
    let request = tauri::webview::InvokeRequest {
        cmd: "open_browser_window".into(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url: if cfg!(windows) {
            "http://tauri.localhost"
        } else {
            "tauri://localhost"
        }
        .parse()
        .unwrap(),
        body: InvokeBody::Json(serde_json::json!({"address":"https://fixture.example/one"})),
        headers: Default::default(),
        invoke_key: INVOKE_KEY.into(),
    };
    let response = get_ipc_response(&main, request).unwrap();
    let status: serde_json::Value = response.deserialize().unwrap();
    assert_eq!(status["sessionId"], "1");
    assert_eq!(status["open"], true);
    assert_eq!(status["opening"], true);
    assert_eq!(status["address"], serde_json::Value::Null);
    assert_eq!(status["origin"], "https://fixture.example");
    assert_eq!(status["downloadsEnabled"], false);
    assert!(app.get_webview_window("uebox-isolated-browser").is_some());
}
