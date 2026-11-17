//! Test-only line transport into the real Tauri command handler.
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use tauri::{test::MockRuntime, WebviewWindow};

fn invoke(window: &WebviewWindow<MockRuntime>, command: &str, args: Value) -> Result<Value, Value> {
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
        body: tauri::ipc::InvokeBody::Json(args),
        headers: Default::default(),
        invoke_key: tauri::test::INVOKE_KEY.to_string(),
    };
    tauri::test::get_ipc_response(window, request).map(|body| body.deserialize().unwrap())
}
fn main() {
    let app = uebox_tauri_lib::configure_commands(tauri::test::mock_builder())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let other = tauri::WebviewWindowBuilder::new(&app, "other", Default::default())
        .build()
        .unwrap();
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{}", json!({"ready":true})).unwrap();
    stdout.flush().unwrap();
    for line in io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = message["id"].as_u64().unwrap();
        let command = message["command"].as_str().unwrap();
        let args = message.get("args").cloned().unwrap_or_else(|| json!({}));
        let result = match command {
            "fixture_other_start" => invoke(&other, "start_godot_agent", args),
            "fixture_other_poll" => invoke(&other, "poll_godot_agent", args),
            "fixture_other_cancel" => invoke(&other, "cancel_godot_agent", args),
            "fixture_other_close" => invoke(&other, "close_godot_agent", args),
            "launch_godot"
            | "connect_godot"
            | "query_godot_project"
            | "query_godot_scene"
            | "status_godot_session"
            | "release_godot_session"
            | "create_godot_agent"
            | "start_godot_agent"
            | "poll_godot_agent"
            | "cancel_godot_agent"
            | "close_godot_agent" => invoke(&window, command, args),
            _ => Err(Value::String("unsupported test command".into())),
        };
        let response = match result {
            Ok(value) => json!({"id":id,"ok":true,"value":value}),
            Err(error) => json!({"id":id,"ok":false,"error":error}),
        };
        writeln!(stdout, "{response}").unwrap();
        stdout.flush().unwrap();
    }
}
