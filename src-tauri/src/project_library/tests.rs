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

fn import_manifest(projects: Value) -> Value {
    json!({
        "format": "uebox-project-registration",
        "version": 1,
        "pathFamily": if cfg!(windows) { "windows" } else { "posix" },
        "projects": projects
    })
}

#[test]
fn import_preserves_offline_projects_and_merges_duplicates_atomically() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("preview-data");
    let offline = fixture.path().join("offline-project");
    let window = window(&root);
    let manifest = import_manifest(json!([
        {"engine":"unreal","name":"Original","path":offline},
        {"engine":"unreal","name":"Alias","path":offline},
        {"engine":"godot","name":"Another engine","path":offline}
    ]));
    let parameters = json!({"manifest":manifest.to_string()});
    assert_eq!(
        invoke(&window, "import_registered_projects", parameters.clone()),
        Ok(json!({"inserted":2,"alreadyRegistered":0,"duplicateInFile":1}))
    );
    let before = invoke(&window, "list_registered_projects", json!({})).unwrap();
    assert_eq!(before[0]["name"], "Original");
    assert_eq!(before[1]["name"], "Another engine");
    assert_eq!(
        invoke(&window, "import_registered_projects", parameters),
        Ok(json!({"inserted":0,"alreadyRegistered":2,"duplicateInFile":1}))
    );
    assert_eq!(
        invoke(&window, "list_registered_projects", json!({})),
        Ok(before)
    );
    assert!(!offline.exists());
}

#[test]
fn rejects_invalid_manifests_before_creating_storage() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("preview-data");
    let window = window(&root);
    let mut unsupported = import_manifest(json!([]));
    unsupported["version"] = json!(2);
    let mut wrong_path_family = import_manifest(json!([]));
    wrong_path_family["pathFamily"] = json!(if cfg!(windows) { "posix" } else { "windows" });
    let bad_record = import_manifest(json!([
        {"engine":"unreal","name":"Keep","path":fixture.path()},
        {"engine":"unreal","name":"Invalid","path":"relative"}
    ]));
    for (manifest, code) in [
        (unsupported, "project_import_version"),
        (wrong_path_family, "project_import_platform"),
        (bad_record, "project_import_invalid"),
    ] {
        assert_eq!(
            invoke(
                &window,
                "import_registered_projects",
                json!({"manifest":manifest.to_string()})
            ),
            Err(json!(code))
        );
        assert!(!root.exists());
    }
    assert_eq!(
        invoke(
            &window,
            "import_registered_projects",
            json!({"manifest":" ".repeat(1_048_577)}),
        ),
        Err(json!("project_import_size"))
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

#[test]
fn metadata_and_cover_commands_keep_engine_registrations_separate() {
    const IMAGE: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("preview-data");
    let project = fixture.path().join("shared-project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("project.godot"), "untouched").unwrap();
    let first = window(&root);
    let unreal = invoke(
        &first,
        "register_project",
        json!({"engine":"unreal","name":"UE","path":project}),
    )
    .unwrap();
    let godot = invoke(
        &first,
        "register_project",
        json!({"engine":"godot","name":"Godot","path":project}),
    )
    .unwrap();

    let fields = json!({"description":"Godot fixture","note":"Review scene","isPinned":true,"sortOrder":1.5});
    assert_eq!(
        invoke(
            &first,
            "save_project_properties",
            json!({
                "project":godot["id"], "fields": fields
            })
        )
        .unwrap(),
        fields
    );
    assert_eq!(
        invoke(
            &first,
            "save_project_cover",
            json!({
                "project":godot["id"], "encoded":"YmFk"
            })
        )
        .unwrap_err(),
        "project_cover_invalid"
    );
    let saved = invoke(
        &first,
        "save_project_cover",
        json!({
            "project":godot["id"], "encoded":IMAGE
        }),
    )
    .unwrap();
    assert_eq!(saved["mime"], "image/png");
    assert_eq!(saved["data"], IMAGE);
    let projects = invoke(&first, "list_registered_projects", json!({})).unwrap();
    assert_eq!(projects[0]["id"], godot["id"]);
    assert_eq!(projects[0]["pinned"], true);
    assert_eq!(projects[1]["id"], unreal["id"]);
    assert!(projects[1].get("pinned").is_none());
    assert_eq!(
        invoke(
            &first,
            "read_project_cover",
            json!({"project":unreal["id"]})
        )
        .unwrap(),
        Value::Null
    );
    assert_eq!(
        invoke(
            &first,
            "get_project_properties",
            json!({"project":unreal["id"]})
        )
        .unwrap()["fields"]["description"],
        Value::Null
    );
    drop(first);

    let reopened = window(&root);
    let updated = invoke(
        &reopened,
        "get_project_properties",
        json!({"project":godot["id"]}),
    )
    .unwrap();
    assert_eq!(updated["fields"], fields);
    assert_eq!(updated["hasCover"], true);
    assert_eq!(
        invoke(
            &reopened,
            "read_project_cover",
            json!({"project":godot["id"]})
        )
        .unwrap(),
        saved
    );
    assert_eq!(
        invoke(
            &reopened,
            "remove_project_cover",
            json!({"project":godot["id"]})
        )
        .unwrap(),
        true
    );
    assert_eq!(
        invoke(
            &reopened,
            "read_project_cover",
            json!({"project":godot["id"]})
        )
        .unwrap(),
        Value::Null
    );
    assert_eq!(
        fs::read_to_string(project.join("project.godot")).unwrap(),
        "untouched"
    );
}
