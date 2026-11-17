#![cfg(feature = "godot")]

use serde_json::{json, Value};
use tauri::{test::MockRuntime, WebviewWindow};

fn invoke(window: &WebviewWindow<MockRuntime>, command: &str, body: Value) -> Result<Value, Value> {
    let request = tauri::webview::InvokeRequest {
        cmd: command.into(),
        callback: tauri::ipc::CallbackFn(0),
        error: tauri::ipc::CallbackFn(1),
        url: "tauri://localhost".parse().unwrap(),
        body: tauri::ipc::InvokeBody::Json(body),
        headers: Default::default(),
        invoke_key: tauri::test::INVOKE_KEY.to_string(),
    };
    tauri::test::get_ipc_response(window, request).map(|body| body.deserialize().unwrap())
}

#[test]
fn creating_an_agent_requires_an_existing_connected_godot_session_without_echoing_secrets() {
    let app = uebox_tauri_lib::configure_commands(tauri::test::mock_builder())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let secret = "fixture-secret-never-echo";
    let error = invoke(&window, "create_godot_agent", json!({
        "sessionId":"not-connected", "endpoint":"http://127.0.0.1:9/fixture?secret=fixture-secret-never-echo",
        "model":"fixture", "credential":secret
    })).expect_err("unknown target cannot create an agent").to_string();
    assert!(error.contains("target_unavailable"), "{error}");
    assert!(!error.contains(secret));
    for (command, body) in [
        (
            "start_godot_agent",
            json!({"agentId":"unknown", "text":"hello"}),
        ),
        ("poll_godot_agent", json!({"agentId":"unknown", "runId":1})),
        (
            "cancel_godot_agent",
            json!({"agentId":"unknown", "runId":1}),
        ),
        ("close_godot_agent", json!({"agentId":"unknown"})),
    ] {
        let error = invoke(&window, command, body)
            .expect_err("unknown agent must fail")
            .to_string();
        assert!(error.contains("agent_unavailable"), "{command}: {error}");
    }
}
