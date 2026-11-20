use serde_json::{json, Value};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};

use super::*;

#[test]
fn collection_lifecycle_and_legacy_import_preserve_independent_project_registrations() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("profile");
    let app = crate::configure_commands(mock_builder())
        .build(mock_context(noop_assets()))
        .unwrap();
    app.state::<ProjectRegistrations>()
        .set_test_data_dir(root.clone());
    let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let invoke = |command: &str, args: Value| {
        get_ipc_response(
            &window,
            tauri::webview::InvokeRequest {
                cmd: command.into(),
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
            },
        )
        .map(|body| body.deserialize::<Value>().unwrap())
    };
    assert_eq!(
        invoke("list_project_collections", json!({})).unwrap(),
        json!([])
    );
    assert!(!root.exists());
    let first =
        invoke("create_project_collection", json!({"name": "First"})).unwrap()[0]["id"].clone();
    let second =
        invoke("create_project_collection", json!({"name": "Second"})).unwrap()[1]["id"].clone();
    let ordered = invoke(
        "move_project_collection",
        json!({"collectionId": second, "direction": "up"}),
    )
    .unwrap();
    assert_eq!(ordered[0]["id"], second);
    assert_eq!(ordered[1]["id"], first);
    let renamed = invoke(
        "rename_project_collection",
        json!({"collectionId": second, "name": "Demo"}),
    )
    .unwrap();
    assert_eq!(renamed[0]["name"], "Demo");

    let project = fixture.path().join("offline-project");
    let path_family = if cfg!(windows) { "windows" } else { "posix" };
    let collection = json!({
        "format": "uebox-project-collections",
        "version": 1,
        "pathFamily": path_family,
        "collections": [{"id": "legacy-group", "name": null, "sortOrder": 99, "members": [{"engine": "unreal", "path": project}]}]
    });
    let import = || {
        invoke(
            "import_project_collections",
            json!({"manifest": collection.to_string()}),
        )
    };
    assert_eq!(
        import().unwrap_err(),
        "project_collections_import_missing_project"
    );
    assert_eq!(
        invoke("list_project_collections", json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let registration = json!({
        "format": "uebox-project-registration",
        "version": 1,
        "pathFamily": path_family,
        "projects": [{"engine": "unreal", "name": "Offline", "path": project}]
    });
    invoke(
        "import_registered_projects",
        json!({"manifest": registration.to_string()}),
    )
    .unwrap();
    let id = invoke("list_registered_projects", json!({})).unwrap()[0]["id"].clone();
    let report = import().unwrap();
    assert_eq!(report["inserted"], 1);
    assert_eq!(report["collections"][2]["projectIds"], json!([id]));
    assert_eq!(import().unwrap()["alreadyImported"], 1);

    let linked = invoke(
        "set_project_collection_membership",
        json!({"collectionId": first, "registrationId": id, "belongs": true}),
    )
    .unwrap();
    assert_eq!(linked[1]["projectIds"], json!([id]));
    let dissolved = invoke("remove_project_collection", json!({"collectionId": first})).unwrap();
    assert_eq!(dissolved.as_array().unwrap().len(), 2);
    assert_eq!(
        invoke("list_registered_projects", json!({})).unwrap()[0]["id"],
        id
    );
    invoke("remove_registered_project", json!({"id": id})).unwrap();
    assert_eq!(
        invoke("list_project_collections", json!({})).unwrap()[1]["projectIds"],
        json!([])
    );
    assert!(
        !project.exists(),
        "collection operations never create editor projects"
    );
}
