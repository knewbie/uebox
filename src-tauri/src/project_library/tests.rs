use std::fs;
use std::path::Path;

use serde_json::{json, Value};
use tauri::{test::MockRuntime, Manager, WebviewWindow};

use super::ProjectRegistrations;

fn window(root: &Path) -> WebviewWindow<MockRuntime> {
    let app = crate::configure_commands(tauri::test::mock_builder())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.state::<ProjectRegistrations>()
        .set_test_data_dir(root.to_path_buf());
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
fn commands_register_all_engines_reopen_and_remove_only_the_registration() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("preview-data");
    let project = fixture.path().join("工程");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("project.godot"), "untouched").unwrap();

    let first = window(&root);
    let host = invoke(&first, "host_status", json!({})).unwrap();
    assert!(host["capabilities"]
        .as_array()
        .unwrap()
        .contains(&json!({"id": "projectRegistration", "available": true})));
    assert_eq!(
        invoke(&first, "list_registered_projects", json!({})),
        Ok(json!([]))
    );
    assert!(
        !root.exists(),
        "looking at an empty library must not create one"
    );

    let mut records = Vec::new();
    for engine in ["unreal", "unity", "godot"] {
        let parameters = json!({"engine": engine, "name": "测试工程", "path": project});
        let record = invoke(&first, "register_project", parameters.clone()).unwrap();
        assert!(record["id"].as_str().unwrap().parse::<i64>().unwrap() > 0);
        assert_eq!(record["engine"], engine);
        assert_eq!(record["name"], "测试工程");
        assert_eq!(record["path"], project.to_str().unwrap());
        assert_eq!(
            invoke(&first, "register_project", parameters),
            Err(json!("project_duplicate"))
        );
        records.push(record);
    }
    drop(first);

    let second = window(&root);
    assert_eq!(
        invoke(&second, "list_registered_projects", json!({})),
        Ok(json!(records))
    );
    let removed = records.remove(1);
    assert_eq!(
        invoke(
            &second,
            "remove_registered_project",
            json!({"id": removed["id"]})
        ),
        Ok(json!(true))
    );
    assert_eq!(
        invoke(
            &second,
            "remove_registered_project",
            json!({"id": removed["id"]})
        ),
        Ok(json!(false))
    );
    assert_eq!(
        invoke(&second, "list_registered_projects", json!({})),
        Ok(json!(records))
    );
    assert_eq!(
        fs::read_to_string(project.join("project.godot")).unwrap(),
        "untouched"
    );
}

#[test]
fn invalid_command_parameters_leave_storage_uninitialized() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("preview-data");
    let window = window(&root);
    for parameters in [
        json!({"engine": "other", "name": "fixture", "path": fixture.path()}),
        json!({"engine": "unity", "name": "  ", "path": fixture.path()}),
        json!({"engine": "godot", "name": "fixture", "path": "relative"}),
    ] {
        assert_eq!(
            invoke(&window, "register_project", parameters),
            Err(json!("project_invalid"))
        );
    }
    assert_eq!(
        invoke(&window, "remove_registered_project", json!({"id": "-1"})),
        Err(json!("project_invalid"))
    );
    assert!(!root.exists());
}

#[test]
fn commands_preserve_incompatible_data_and_rollback_journals() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("preview-data");
    let database = root.join("project-library/library.sqlite");
    let journal = database.with_file_name("library.sqlite-journal");
    let registration = json!({"engine": "godot", "name": "fixture", "path": fixture.path()});
    let first = window(&root);
    invoke(&first, "register_project", registration.clone()).unwrap();
    drop(first);
    let before = fs::read(&database).unwrap();

    let mut journal_options = fs::OpenOptions::new();
    journal_options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut journal_options, 0o600);
    use std::io::Write;
    journal_options
        .open(&journal)
        .unwrap()
        .write_all(b"fixture")
        .unwrap();

    let second = window(&root);
    assert_eq!(
        invoke(&second, "list_registered_projects", json!({})),
        Err(json!("project_library_recovery_required"))
    );
    assert_eq!(
        invoke(&second, "register_project", registration.clone()),
        Err(json!("project_library_recovery_required"))
    );
    assert_eq!(fs::read(&database).unwrap(), before);
    assert_eq!(fs::read(&journal).unwrap(), b"fixture");
    fs::remove_file(&journal).unwrap();

    // Change only the application identifier in the fixed SQLite header.
    let mut foreign = before;
    foreign[68..72].copy_from_slice(&0u32.to_be_bytes());
    fs::write(&database, &foreign).unwrap();
    assert_eq!(
        invoke(&second, "register_project", registration),
        Err(json!("project_library_invalid"))
    );
    assert_eq!(fs::read(&database).unwrap(), foreign);
}
