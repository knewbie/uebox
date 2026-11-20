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
fn host_reports_which_engine_modules_are_compiled() {
    let status = invoke(&window(), "host_status", json!({})).unwrap();
    assert_eq!(status["engineModules"]["unity"], cfg!(feature = "unity"));
    assert_eq!(status["engineModules"]["unreal"], cfg!(feature = "unreal"));
    assert_eq!(status["engineModules"]["godot"], cfg!(feature = "godot"));
    assert!(status["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| { item["id"] == "engines" && item["available"] == false }));
}

#[test]
fn godot_commands_are_registered_only_when_their_module_is_compiled() {
    let window = window();
    for (command, body) in [
        (
            "launch_godot",
            json!({"executable":"/missing","project":"/missing"}),
        ),
        ("connect_godot", json!({"sessionId":"missing"})),
        ("query_godot_project", json!({"sessionId":"missing"})),
        ("query_godot_scene", json!({"sessionId":"missing"})),
        ("status_godot_session", json!({"sessionId":"missing"})),
        ("release_godot_session", json!({"sessionId":"missing"})),
    ] {
        let error = invoke(&window, command, body)
            .expect_err("no Godot session can succeed")
            .to_string();
        if cfg!(feature = "godot") {
            assert!(!error.contains("Command "), "{command}: {error}");
            assert!(
                error.contains("InvalidProject") || error.contains("Unknown Godot session"),
                "{command}: {error}"
            );
        } else {
            assert!(error.contains("not found"), "{command}: {error}");
        }
    }
}

#[test]
fn godot_agent_commands_are_absent_from_other_engine_builds() {
    let error = invoke(
        &window(),
        "create_godot_agent",
        json!({"sessionId":"missing","endpoint":"http://127.0.0.1:9/v1/chat/completions","model":"fixture","credential":null}),
    )
    .expect_err("an unknown target must never create an agent")
    .to_string();
    if cfg!(feature = "godot") {
        assert!(error.contains("target_unavailable"), "{error}");
    } else {
        assert!(error.contains("not found"), "{error}");
    }
}

#[test]
fn unity_command_is_registered_only_when_its_module_is_compiled() {
    let error = invoke(
        &window(),
        "inspect_unity",
        json!({"projectRoot":"/fixture","port":1,"allowClientTakeover":false}),
    )
    .expect_err("without authorization the command cannot connect")
    .to_string();
    if cfg!(feature = "unity") {
        assert!(
            error.contains("takeover requires explicit authorization"),
            "{error}"
        );
    } else {
        assert!(error.contains("not found"), "{error}");
    }
}

#[test]
fn unreal_commands_are_registered_only_when_their_module_is_compiled() {
    let window = window();
    for (command, body) in [
        ("start_unreal_listener", json!({"port":0})),
        ("list_unreal_instances", json!({})),
        ("query_unreal_project", json!({"connectionId":""})),
        ("stop_unreal_listener", json!({})),
    ] {
        let error = invoke(&window, command, body)
            .expect_err("no listener can succeed")
            .to_string();
        if cfg!(feature = "unreal") {
            assert!(!error.contains("not found"), "{command}: {error}");
            assert!(
                error.contains("required") || error.contains("not running"),
                "{command}: {error}"
            );
        } else {
            assert!(error.contains("not found"), "{command}: {error}");
        }
    }
}

#[test]
fn single_engine_dependency_graph_excludes_other_engines() {
    let cases: [(&str, &str, &[&str]); 3] = [
        (
            "unity",
            "uebox-engine-unity ",
            &[
                "uebox-engine-unreal ",
                "uebox-godot-session ",
                "uebox-engine-godot ",
                "uebox-agent-core ",
                "uebox-agent-godot-tools ",
            ],
        ),
        (
            "unreal",
            "uebox-engine-unreal ",
            &[
                "uebox-engine-unity ",
                "uebox-godot-session ",
                "uebox-engine-godot ",
                "uebox-agent-core ",
                "uebox-agent-godot-tools ",
            ],
        ),
        (
            "godot",
            "uebox-godot-session ",
            &["uebox-engine-unity ", "uebox-engine-unreal "],
        ),
    ];
    for (feature, included, excluded) in cases {
        let output = std::process::Command::new(env!("CARGO"))
            .args([
                "tree",
                "--offline",
                "--locked",
                "--no-default-features",
                "--features",
                feature,
                "--edges",
                "normal,build,dev",
                "--prefix",
                "none",
                "--manifest-path",
            ])
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .output()
            .expect("cargo tree must run");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let tree = String::from_utf8(output.stdout).unwrap();
        assert!(
            tree.lines().any(|line| line.starts_with(included)),
            "{tree}"
        );
        if feature == "godot" {
            assert!(
                tree.lines()
                    .any(|line| line.starts_with("uebox-engine-godot ")),
                "{tree}"
            );
        }
        for &excluded in excluded {
            assert!(
                !tree.lines().any(|line| line.starts_with(excluded)),
                "{tree}"
            );
        }
    }
}
