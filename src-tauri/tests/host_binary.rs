use std::process::{Command, Output, Stdio};

fn run_host_status() -> (u32, Output) {
    let binary =
        option_env!("CARGO_BIN_EXE_uebox-tauri").unwrap_or("src-tauri/target/debug/uebox-tauri");
    let child = Command::new(binary)
        .arg("--host-status-json")
        .stdout(Stdio::piped())
        .spawn();
    assert!(child.is_ok(), "the Rust host diagnostic must be executable");
    let child = child.unwrap();
    let process_id = child.id();
    (process_id, child.wait_with_output().unwrap())
}

#[test]
fn host_status_reports_the_running_rust_process_and_platform() {
    let (process_id, output) = run_host_status();
    assert!(output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["processId"], process_id);
    assert_eq!(status["os"], std::env::consts::OS);
    assert_eq!(status["architecture"], std::env::consts::ARCH);
    assert_eq!(status["backend"], "rust");
}

#[test]
fn host_status_does_not_claim_unmigrated_features_are_available() {
    let (_, output) = run_host_status();
    assert!(output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["migration"], "in_progress");
    let capabilities = status["capabilities"].as_array().unwrap();
    for capability in ["engines", "agent", "libraries", "settings"] {
        assert!(
            capabilities
                .iter()
                .any(|item| item["id"] == capability && item["available"] == false),
            "{capability} must be reported as unavailable"
        );
    }
}

#[test]
fn opt_in_trace_reports_the_process_serving_host_status() {
    let binary =
        option_env!("CARGO_BIN_EXE_uebox-tauri").unwrap_or("src-tauri/target/debug/uebox-tauri");
    let child = Command::new(binary)
        .arg("--host-status-json")
        .env("UEBOX_TAURI_TRACE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let process_id = child.id();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let trace = String::from_utf8(output.stderr).unwrap();
    assert!(trace.contains(&format!("host_status process_id={process_id}")));
}

#[test]
fn registered_command_reports_the_host_and_rejects_unknown_commands() {
    let app = uebox_tauri_lib::configure_commands(tauri::test::mock_builder())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let request = |command: &str| tauri::webview::InvokeRequest {
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
        body: tauri::ipc::InvokeBody::default(),
        headers: Default::default(),
        invoke_key: tauri::test::INVOKE_KEY.to_string(),
    };

    let response = tauri::test::get_ipc_response(&window, request("host_status")).unwrap();
    let status: serde_json::Value = response.deserialize().unwrap();
    assert_eq!(status["processId"], std::process::id());
    assert_eq!(status["backend"], "rust");
    assert!(tauri::test::get_ipc_response(&window, request("arbitrary_ipc")).is_err());
}
