#![cfg(feature = "godot")]

use serde_json::{json, Value};
use tauri::test::MockRuntime;
use tauri::WebviewWindow;

fn window() -> WebviewWindow<MockRuntime> {
    let app = uebox_tauri_lib::configure_commands(tauri::test::mock_builder())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap()
}

fn invoke(window: &WebviewWindow<MockRuntime>, command: &str, body: Value) -> Result<Value, Value> {
    let request = tauri::webview::InvokeRequest {
        cmd: command.into(),
        callback: tauri::ipc::CallbackFn(0),
        error: tauri::ipc::CallbackFn(1),
        url: if cfg!(windows) {
            "http://tauri.localhost"
        } else {
            "tauri://localhost"
        }
        .parse()
        .unwrap(),
        body: tauri::ipc::InvokeBody::Json(body),
        headers: Default::default(),
        invoke_key: tauri::test::INVOKE_KEY.to_string(),
    };
    tauri::test::get_ipc_response(window, request).map(|body| body.deserialize().unwrap())
}

#[test]
fn godot_launch_needs_no_separate_confirmation_parameter() {
    let window = window();
    let error = invoke(
        &window,
        "launch_godot",
        json!({"executable":"/missing","project":"/missing"}),
    )
    .expect_err("the selected project must be validated")
    .to_string();
    assert!(error.contains("InvalidProject"), "{error}");
}

#[test]
fn every_godot_operation_rejects_an_unknown_session_id() {
    let window = window();
    for command in [
        "connect_godot",
        "query_godot_project",
        "query_godot_scene",
        "status_godot_session",
        "release_godot_session",
    ] {
        let error = invoke(&window, command, json!({"sessionId":"not-a-session"}))
            .expect_err("unknown session must fail")
            .to_string();
        assert!(
            error.contains("Unknown Godot session"),
            "{command}: {error}"
        );
    }
}

#[test]
fn godot_launch_rejects_invalid_selected_executable() {
    let root = std::env::temp_dir().join(format!("uebox-godot-invalid-exe-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    std::fs::write(root.join("project.godot"), "config_version=5\n").unwrap();
    let error = invoke(
        &window(),
        "launch_godot",
        json!({"executable":root.join("missing-godot"),"project":root}),
    )
    .expect_err("an invalid executable must fail")
    .to_string();
    assert!(error.contains("InvalidExecutable"), "{error}");
}
