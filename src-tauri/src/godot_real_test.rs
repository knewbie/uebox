//! Opt-in Tauri command integration against a real headless Godot and a temporary project.
use super::*;
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tauri::test::MockRuntime;
use tauri::WebviewWindow;

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

#[cfg(unix)]
fn alive(pid: &str) -> bool {
    Command::new("/bin/kill")
        .args(["-0", pid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn read_model_request(stream: &mut TcpStream) -> (String, String) {
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let mut data = Vec::new();
    let (header_end, length) = loop {
        let mut buffer = [0_u8; 4096];
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0);
        data.extend_from_slice(&buffer[..count]);
        if let Some(position) = data.windows(4).position(|part| part == b"\r\n\r\n") {
            let header_end = position + 4;
            let header = String::from_utf8(data[..header_end].to_vec()).unwrap();
            let length = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .expect("fixture expects a Content-Length");
            break (header_end, length);
        }
    };
    while data.len() < header_end + length {
        let mut buffer = [0_u8; 4096];
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0);
        data.extend_from_slice(&buffer[..count]);
    }
    (
        String::from_utf8(data[..header_end].to_vec()).unwrap(),
        String::from_utf8(data[header_end..header_end + length].to_vec()).unwrap(),
    )
}

fn respond_model(stream: &mut TcpStream, event: Value) {
    let body = format!("data: {event}\n\ndata: [DONE]\n\n");
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
}

struct Fixture {
    root: PathBuf,
    project: PathBuf,
    additional_projects: Vec<PathBuf>,
    sessions: GodotSessions,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // If a test failed before release, these are only handles created by this fixture.
        let handles: Vec<_> = self
            .sessions
            .sessions
            .lock()
            .unwrap()
            .drain()
            .map(|(_, entry)| entry.handle)
            .collect();
        for handle in handles {
            if let Some(session) = handle.lock().unwrap().take() {
                let mut child = session.into_editor_process();
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        for project in std::iter::once(&self.project).chain(&self.additional_projects) {
            // After release the test-only project plugin exits its own editor on this marker.
            let _ = fs::write(project.join("fixture-stop"), "stop");
            #[cfg(unix)]
            if let Ok(pid) = fs::read_to_string(project.join("fixture-pid")) {
                let pid = pid.trim();
                let deadline = Instant::now() + Duration::from_secs(8);
                while alive(pid) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(50));
                }
                if alive(pid) {
                    // Only the process carrying this test's unique project path can be stopped.
                    let args = Command::new("/bin/ps")
                        .args(["-p", pid, "-o", "command="])
                        .output()
                        .ok()
                        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
                        .unwrap_or_default();
                    if args.contains(project.to_str().unwrap()) {
                        let _ = Command::new("/bin/kill").args(["-TERM", pid]).status();
                    }
                }
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn real_godot_commands_query_the_selected_temporary_editor_and_release_without_killing_it() {
    let executable = std::env::var_os("UEBOX_TEST_GODOT_BIN")
        .expect("set UEBOX_TEST_GODOT_BIN to a real native Godot executable");
    let root =
        std::env::temp_dir().join(format!("uebox-tauri-godot-fixture-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let project = root.join("selected project");
    let home = root.join("private-home");
    fs::create_dir(&project).unwrap();
    for dir in ["appdata", "localappdata", "config", "data", "cache", "temp"] {
        fs::create_dir_all(home.join(dir)).unwrap();
    }
    let sessions = GodotSessions {
        test_home: Some(home),
        ..Default::default()
    };
    let mut fixture = Fixture {
        root,
        project,
        additional_projects: Vec::new(),
        sessions: sessions.clone(),
    };
    let addon = fixture.project.join("addons/uebox_godot");
    fs::create_dir_all(&addon).unwrap();
    let source_addon =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../integrations/godot/addons/uebox_godot");
    for file in ["plugin.cfg", "plugin.gd"] {
        fs::copy(source_addon.join(file), addon.join(file)).unwrap();
    }
    let probe = fixture.project.join("addons/fixture_probe");
    fs::create_dir_all(&probe).unwrap();
    fs::write(probe.join("plugin.cfg"), "[plugin]\nname=\"Fixture probe\"\ndescription=\"Temporary test only\"\nauthor=\"test\"\nversion=\"1\"\nscript=\"plugin.gd\"\n").unwrap();
    fs::write(
        probe.join("plugin.gd"),
        r#"@tool
extends EditorPlugin
var reported := false
func _enter_tree() -> void:
    set_process(true)
    call_deferred("_open_fixture")
func _open_fixture() -> void:
    EditorInterface.open_scene_from_path("res://main.tscn")
func _process(_delta: float) -> void:
    if FileAccess.file_exists("res://fixture-stop"):
        get_tree().quit()
        return
    var root := EditorInterface.get_edited_scene_root()
    if not reported and root != null and root.name == "World":
        var file := FileAccess.open("res://fixture-pid", FileAccess.WRITE)
        file.store_string(str(OS.get_process_id()))
        file.close()
        reported = true
"#,
    )
    .unwrap();
    let project_contents = "config_version=5\n[application]\nconfig/name=\"Tauri fixture project\"\n[editor_plugins]\nenabled=PackedStringArray(\"res://addons/uebox_godot/plugin.cfg\", \"res://addons/fixture_probe/plugin.cfg\")\n";
    let scene_contents = "[gd_scene format=3]\n\n[node name=\"World\" type=\"Node2D\"]\n\n[node name=\"Player\" type=\"Node2D\" parent=\".\"]\n";
    fs::write(fixture.project.join("project.godot"), project_contents).unwrap();
    fs::write(fixture.project.join("main.tscn"), scene_contents).unwrap();
    let second_project = fixture.root.join("second project");
    fs::create_dir(&second_project).unwrap();
    fixture.additional_projects.push(second_project.clone());
    for directory in ["uebox_godot", "fixture_probe"] {
        let destination = second_project.join("addons").join(directory);
        fs::create_dir_all(&destination).unwrap();
        for file in ["plugin.cfg", "plugin.gd"] {
            fs::copy(
                fixture.project.join("addons").join(directory).join(file),
                destination.join(file),
            )
            .unwrap();
        }
    }
    let second_contents = project_contents.replace("Tauri fixture project", "Tauri fixture second");
    fs::write(second_project.join("project.godot"), &second_contents).unwrap();
    fs::write(second_project.join("main.tscn"), scene_contents).unwrap();

    let app = tauri::test::mock_builder()
        .manage(sessions.clone())
        .invoke_handler(tauri::generate_handler![
            super::launch_godot,
            super::connect_godot,
            super::query_godot_project,
            super::query_godot_scene,
            super::status_godot_session,
            super::release_godot_session,
            crate::godot_agent::create_godot_agent,
            crate::godot_agent::start_godot_agent,
            crate::godot_agent::poll_godot_agent,
            crate::godot_agent::cancel_godot_agent,
            crate::godot_agent::close_godot_agent,
            crate::godot_agent::list_godot_name_activity,
            crate::godot_agent::answer_godot_name_approval
        ])
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let launched = invoke(
        &window,
        "launch_godot",
        json!({
            "executable":executable.to_str().unwrap(), "project":fixture.project.to_str().unwrap()
        }),
    )
    .unwrap();
    assert_eq!(launched.as_object().unwrap().len(), 2);
    assert_eq!(launched["state"], "starting");
    let id = launched["sessionId"].as_str().unwrap().to_owned();
    let handle = lookup(&sessions.sessions, &id, window.label()).unwrap();
    assert_ne!(id, handle.lock().unwrap().as_ref().unwrap().instance_id());
    drop(handle);
    let second = invoke(
        &window,
        "launch_godot",
        json!({
            "executable":executable.to_str().unwrap(), "project":second_project.to_str().unwrap()
        }),
    )
    .unwrap();
    let second_id = second["sessionId"].as_str().unwrap().to_owned();
    assert_ne!(id, second_id);

    let deadline = Instant::now() + Duration::from_secs(30);
    let pid = loop {
        if let Ok(pid) = fs::read_to_string(fixture.project.join("fixture-pid")) {
            if !pid.trim().is_empty() {
                break pid.trim().to_owned();
            }
        }
        assert!(
            Instant::now() < deadline,
            "fixture editor did not open its scene"
        );
        thread::sleep(Duration::from_millis(50));
    };
    let second_pid = loop {
        if let Ok(pid) = fs::read_to_string(second_project.join("fixture-pid")) {
            if !pid.trim().is_empty() {
                break pid.trim().to_owned();
            }
        }
        assert!(
            Instant::now() < deadline,
            "second fixture editor did not open its scene"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert_ne!(
        pid, second_pid,
        "each fixture must be a distinct editor process"
    );
    let body = json!({"sessionId":id});
    let second_body = json!({"sessionId":second_id});
    let project = invoke(&window, "connect_godot", body.clone()).unwrap();
    assert_eq!(project.as_object().unwrap().len(), 2);
    assert_eq!(project["name"], "Tauri fixture project");
    assert!(project["godotVersion"].as_str().unwrap().starts_with("4."));
    assert_eq!(
        invoke(&window, "connect_godot", second_body.clone()).unwrap()["name"],
        "Tauri fixture second"
    );
    assert_eq!(
        invoke(&window, "query_godot_project", body.clone()).unwrap(),
        project
    );
    let scene = invoke(&window, "query_godot_scene", body.clone()).unwrap();
    assert_eq!(scene.as_object().unwrap().len(), 3);
    assert_eq!(scene["nodes"][0].as_object().unwrap().len(), 3);
    assert_eq!(scene["scenePath"], "res://main.tscn");
    assert_eq!(scene["nodes"][0]["name"], "World");
    assert_eq!(scene["nodes"][1]["name"], "Player");
    assert_eq!(scene["truncated"], false);
    assert!(scene.get("token").is_none());

    // The model is a local fixture; the selected editor and project are real and temporary.
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let created = invoke(&window, "create_godot_agent", json!({
        "sessionId":id, "endpoint":endpoint, "model":"fixture-model", "credential":"fixture-secret"
    })).unwrap();
    let agent_id = created["agentId"].as_str().unwrap().to_owned();
    thread::sleep(Duration::from_millis(100));
    assert!(
        listener.accept().is_err(),
        "configuring must not request a model"
    );
    let model = thread::spawn(move || {
        let mut requests = Vec::new();
        for step in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(10))
                    }
                    _ => panic!("fixture model did not receive its request"),
                }
            };
            requests.push(read_model_request(&mut stream));
            let event = if step == 0 {
                json!({"choices":[{"index":0,"delta":{"tool_calls":[
                    {"index":0,"id":"project-call","type":"function","function":{"name":"godot_project_info","arguments":"{}"}},
                    {"index":1,"id":"scene-call","type":"function","function":{"name":"godot_scene_tree","arguments":"{}"}}
                ]},"finish_reason":"tool_calls"}]})
            } else {
                json!({"choices":[{"index":0,"delta":{"content":"Real editor fixture answer"},"finish_reason":"stop"}]})
            };
            respond_model(&mut stream, event);
        }
        requests
    });
    let run = invoke(
        &window,
        "start_godot_agent",
        json!({"agentId":agent_id,"text":"Read project and scene"}),
    )
    .unwrap();
    let run_id = run["runId"].as_u64().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut answer = String::new();
    loop {
        let update = invoke(
            &window,
            "poll_godot_agent",
            json!({"agentId":agent_id,"runId":run_id}),
        )
        .unwrap();
        for fragment in update["fragments"].as_array().unwrap() {
            answer.push_str(fragment.as_str().unwrap());
        }
        if update["phase"] == "completed" {
            break;
        }
        assert!(
            update["phase"] != "failed",
            "local model/real editor run failed: {}",
            update["error"]
        );
        assert!(
            Instant::now() < deadline,
            "local model/real editor run timed out"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(answer, "Real editor fixture answer");
    let requests = model.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|(headers, body)| headers
        .to_ascii_lowercase()
        .contains("authorization: bearer fixture-secret")
        && !body.contains("fixture-secret")));
    assert!(requests[1].1.contains("Tauri fixture project"));
    assert!(requests[1].1.contains("World"));
    assert!(requests[1].1.contains("Player"));
    assert!(!requests[1].1.contains("Tauri fixture second"));
    assert!(!requests[1].1.contains(fixture.project.to_str().unwrap()));
    let handle = lookup(&sessions.sessions, &id, window.label()).unwrap();
    let guard = handle.lock().unwrap();
    assert!(requests
        .iter()
        .all(|(_, body)| !guard.as_ref().unwrap().contains_private_bridge_values(body)));
    drop(guard);

    assert!(!requests[0].1.contains("godot_rename_scene_node"));
    invoke(&window, "close_godot_agent", json!({"agentId":agent_id})).unwrap();
    let other = tauri::WebviewWindowBuilder::new(&app, "other", Default::default())
        .build()
        .unwrap();
    assert!(invoke(&other, "query_godot_scene", body.clone()).is_err());

    let rename_model = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    rename_model.set_nonblocking(true).unwrap();
    let rename_endpoint = format!(
        "http://{}/v1/chat/completions",
        rename_model.local_addr().unwrap()
    );
    let config = json!({
        "sessionId":id,"endpoint":rename_endpoint,"model":"fixture-rename","allowNodeRename":true
    });
    assert!(invoke(&other, "create_godot_agent", config.clone()).is_err());
    let renamed_agent = invoke(&window, "create_godot_agent", config).unwrap();
    let renamed_agent_id = renamed_agent["agentId"].as_str().unwrap();
    let rename_requests = thread::spawn(move || {
        let mut requests = Vec::new();
        for step in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut stream = loop {
                match rename_model.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(10));
                    }
                    _ => panic!("rename model request did not arrive"),
                }
            };
            requests.push(read_model_request(&mut stream).1);
            let event = if step == 0 {
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                    "index":0,"id":"rename-call","type":"function","function":{
                        "name":"godot_rename_scene_node",
                        "arguments":json!({"node_path":"Player","name":"ApprovedPlayer"}).to_string()
                    }
                }]},"finish_reason":"tool_calls"}]})
            } else {
                json!({"choices":[{"index":0,"delta":{"content":"Rename confirmed"},"finish_reason":"stop"}]})
            };
            respond_model(&mut stream, event);
        }
        requests
    });
    let rename_run = invoke(
        &window,
        "start_godot_agent",
        json!({"agentId":renamed_agent_id,"text":"Rename Player"}),
    )
    .unwrap();
    let poll_rename = json!({"agentId":renamed_agent_id,"runId":rename_run["runId"]});
    let activity_input = json!({"agentId":renamed_agent_id});
    let deadline = Instant::now() + Duration::from_secs(12);
    let approval = loop {
        let status = invoke(&window, "poll_godot_agent", poll_rename.clone()).unwrap();
        assert_ne!(status["phase"], "failed", "{status}");
        if status["phase"] == "awaiting_approval" {
            let activity =
                invoke(&window, "list_godot_name_activity", activity_input.clone()).unwrap();
            if let Some(proposal) = activity["approvals"]
                .as_array()
                .and_then(|items| items.first())
            {
                break proposal.clone();
            }
        }
        assert!(Instant::now() < deadline, "Godot approval not offered");
        thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(approval["oldName"], "Player");
    assert_eq!(approval["newName"], "ApprovedPlayer");
    assert_eq!(approval["nodePath"], "Player");
    assert_eq!(approval["scenePath"], "res://main.tscn");
    assert_eq!(
        invoke(&other, "list_godot_name_activity", activity_input.clone()).unwrap()["approvals"],
        json!([])
    );
    let answer = json!({"approvalId": approval["approvalId"], "approved":true});
    assert!(invoke(&other, "answer_godot_name_approval", answer.clone()).is_err());
    assert_eq!(
        invoke(&window, "query_godot_scene", body.clone()).unwrap()["nodes"][1]["name"],
        "Player"
    );
    invoke(&window, "answer_godot_name_approval", answer.clone()).unwrap();
    assert!(invoke(&window, "answer_godot_name_approval", answer).is_err());
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let status = invoke(&window, "poll_godot_agent", poll_rename.clone()).unwrap();
        assert_ne!(status["phase"], "failed", "{status}");
        if status["phase"] == "completed" {
            break;
        }
        assert!(Instant::now() < deadline, "Godot rename run did not finish");
        thread::sleep(Duration::from_millis(20));
    }
    let receipt =
        invoke(&window, "list_godot_name_activity", activity_input).unwrap()["receipts"][0].clone();
    assert_eq!(receipt["status"], "applied");
    assert_eq!(receipt["actualName"], "ApprovedPlayer");
    assert_eq!(
        invoke(&window, "query_godot_scene", body.clone()).unwrap()["nodes"][1]["name"],
        "ApprovedPlayer"
    );
    let requests = rename_requests.join().unwrap();
    assert!(requests[0].contains("godot_rename_scene_node"));
    assert!(requests[1].contains("ApprovedPlayer"));
    assert!(!requests[1].contains(fixture.project.to_str().unwrap()));
    let handle = lookup(&sessions.sessions, &id, window.label()).unwrap();
    let guard = handle.lock().unwrap();
    assert!(requests
        .iter()
        .all(|body| !guard.as_ref().unwrap().contains_private_bridge_values(body)));
    drop(guard);
    assert_eq!(
        fs::read_to_string(fixture.project.join("main.tscn")).unwrap(),
        scene_contents
    );

    let status = invoke(&window, "status_godot_session", body.clone()).unwrap();
    assert_eq!(status.as_object().unwrap().len(), 2);
    assert_eq!(status["state"], "ready");
    invoke(&window, "release_godot_session", body.clone()).unwrap();
    assert!(invoke(
        &window,
        "start_godot_agent",
        json!({"agentId":renamed_agent_id,"text":"must fail"})
    )
    .is_err());
    assert!(invoke(&window, "query_godot_project", body)
        .unwrap_err()
        .to_string()
        .contains("Unknown Godot session"));
    assert_eq!(
        invoke(&window, "query_godot_project", second_body.clone()).unwrap()["name"],
        "Tauri fixture second"
    );
    assert_eq!(
        invoke(&window, "query_godot_scene", second_body.clone()).unwrap()["scenePath"],
        "res://main.tscn"
    );
    invoke(&window, "release_godot_session", second_body).unwrap();
    #[cfg(unix)]
    assert!(
        alive(&pid) && alive(&second_pid),
        "release must leave both fixture editors running"
    );
    assert_eq!(
        fs::read_to_string(fixture.project.join("project.godot")).unwrap(),
        project_contents
    );
    assert_eq!(
        fs::read_to_string(fixture.project.join("main.tscn")).unwrap(),
        scene_contents
    );
    assert_eq!(
        fs::read_to_string(second_project.join("project.godot")).unwrap(),
        second_contents
    );
    fs::write(fixture.project.join("fixture-stop"), "stop").unwrap();
    fs::write(second_project.join("fixture-stop"), "stop").unwrap();
    #[cfg(unix)]
    {
        let deadline = Instant::now() + Duration::from_secs(8);
        while (alive(&pid) || alive(&second_pid)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !alive(&pid) && !alive(&second_pid),
            "a test-owned editor did not close"
        );
    }
}
