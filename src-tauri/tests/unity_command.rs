#![cfg(feature = "unity")]

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::test::MockRuntime;
use tauri::WebviewWindow;

fn project() -> &'static str {
    static PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PATH.get_or_init(|| std::env::temp_dir().to_str().unwrap().to_owned())
}
fn other_project() -> &'static str {
    static PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        std::env::temp_dir()
            .parent()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    })
}
fn window() -> WebviewWindow<MockRuntime> {
    let app = uebox_tauri_lib::configure_commands(tauri::test::mock_builder())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap()
}

fn invoke_command(
    window: &WebviewWindow<MockRuntime>,
    command: &str,
    body: Value,
) -> Result<Value, Value> {
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
    tauri::test::get_ipc_response(window, request).map(|body| body.deserialize::<Value>().unwrap())
}

fn invoke(window: &WebviewWindow<MockRuntime>, body: Value) -> Result<Value, Value> {
    invoke_command(window, "inspect_unity", body)
}

fn listener() -> TcpListener {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    assert_ne!(listener.local_addr().unwrap().port(), 6400);
    listener
}

fn accept(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "fixture did not receive a connection"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("fixture accept failed: {error}"),
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Value {
    let mut header = [0_u8; 8];
    stream.read_exact(&mut header).unwrap();
    let size = u64::from_be_bytes(header) as usize;
    assert!((1..4096).contains(&size));
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn send_response(stream: &mut TcpStream, payload: Value) {
    let bytes = serde_json::to_vec(&payload).unwrap();
    stream
        .write_all(&(bytes.len() as u64).to_be_bytes())
        .unwrap();
    stream.write_all(&bytes).unwrap();
}

fn project_response(root: &str, version: &str) -> Value {
    json!({"status":"success", "result":{"success":true, "data": {
        "projectRoot":root, "projectName":"Fixture project", "unityVersion":version,
        "assetsPath":format!("{root}/Assets"), "platform":"Fixture platform",
        "renderPipeline":"Fixture pipeline"
    }}})
}

#[test]
fn takeover_authorization_is_required_before_connecting() {
    let socket = listener();
    let port = socket.local_addr().unwrap().port();
    let error = invoke(
        &window(),
        json!({"projectRoot":project(),"port":port,"allowClientTakeover":false}),
    )
    .expect_err("unauthorized takeover must fail");
    assert!(
        error.to_string().to_lowercase().contains("takeover"),
        "{error}"
    );
    socket.set_nonblocking(true).unwrap();
    assert!(
        matches!(socket.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn denied_takeover_does_not_disturb_a_preexisting_fixture_client() {
    let socket = listener();
    let port = socket.local_addr().unwrap().port();
    let mut existing_client = TcpStream::connect(socket.local_addr().unwrap()).unwrap();
    existing_client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let (mut existing_server, _) = socket.accept().unwrap();

    let error = invoke(
        &window(),
        json!({"projectRoot":project(),"port":port,"allowClientTakeover":false}),
    )
    .expect_err("a denied takeover must not connect");
    assert!(error.to_string().contains("takeover"));
    socket.set_nonblocking(true).unwrap();
    assert!(
        matches!(socket.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    existing_server.write_all(b"still connected").unwrap();
    let mut received = [0; 15];
    existing_client.read_exact(&mut received).unwrap();
    assert_eq!(&received, b"still connected");
}

#[test]
fn omitted_confirmation_and_zero_port_are_rejected() {
    let socket = listener();
    let port = socket.local_addr().unwrap().port();
    for arguments in [
        json!({"projectRoot":project(),"port":port}),
        json!({"projectRoot":project(),"port":port,"confirmed":true}),
        json!({"projectRoot":project(),"port":port,"allowClientTakeover":null}),
    ] {
        let error = invoke(&window(), arguments).expect_err("takeover must default to denial");
        assert!(error.to_string().contains("takeover"), "{error}");
    }
    assert!(invoke(
        &window(),
        json!({"projectRoot":project(),"port":0,"allowClientTakeover":true})
    )
    .is_err());
    socket.set_nonblocking(true).unwrap();
    assert!(
        matches!(socket.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn reused_port_and_project_require_a_fresh_authorization_and_may_be_a_different_editor() {
    let socket = listener();
    let port = socket.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        for scene_name in ["Editor A scene", "Editor B scene"] {
            let mut stream = accept(&socket);
            stream
                .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
                .unwrap();
            for _ in 0..2 {
                assert_eq!(
                    read_request(&mut stream),
                    json!({"type":"get_project_info","params":{}})
                );
                send_response(&mut stream, project_response(project(), "fixture"));
            }
            assert_eq!(
                read_request(&mut stream),
                json!({"type":"manage_scene","params":{"action":"get_active"}})
            );
            send_response(
                &mut stream,
                json!({"status":"success","result":{"success":true,"data":{
                    "name":scene_name,"path":"","buildIndex":-1,"isDirty":false,"isLoaded":true,"rootCount":0
                }}}),
            );
        }
    });
    let window = window();
    let first = invoke(
        &window,
        json!({"projectRoot":project(),"port":port,"allowClientTakeover":true}),
    )
    .unwrap();
    let denied = invoke(&window, json!({"projectRoot":project(),"port":port}));
    assert!(denied
        .expect_err("each attempt requires its own authorization")
        .to_string()
        .contains("takeover"));
    let second = invoke(
        &window,
        json!({"projectRoot":project(),"port":port,"allowClientTakeover":true}),
    )
    .unwrap();
    assert_eq!(first["scene"]["name"], "Editor A scene");
    assert_eq!(second["scene"]["name"], "Editor B scene");
    server.join().unwrap();
}

#[test]
fn invalid_project_path_is_rejected_before_connecting() {
    let socket = listener();
    let port = socket.local_addr().unwrap().port();
    let error = invoke(
        &window(),
        json!({"projectRoot":"relative/project","port":port,"allowClientTakeover":true}),
    )
    .expect_err("relative paths must fail");
    assert!(error.to_string().contains("absolute"), "{error}");
    socket.set_nonblocking(true).unwrap();
    assert!(
        matches!(socket.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn missing_project_directory_is_rejected_before_connecting() {
    let socket = listener();
    let port = socket.local_addr().unwrap().port();
    let missing =
        std::env::temp_dir().join(format!("uebox-missing-project-{}", std::process::id()));
    assert!(!missing.exists());
    let error = invoke(
        &window(),
        json!({"projectRoot":missing.to_str().unwrap(),"port":port,"allowClientTakeover":true}),
    )
    .expect_err("missing project directory must fail");
    assert!(
        error.to_string().contains("accessible directory"),
        "{error}"
    );
    socket.set_nonblocking(true).unwrap();
    assert!(
        matches!(socket.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn queries_selected_project_and_scene_through_registered_command() {
    let socket = listener();
    let port = socket.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let mut stream = accept(&socket);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_eq!(
            read_request(&mut stream),
            json!({"type":"get_project_info","params":{}})
        );
        send_response(&mut stream, project_response(project(), "old version"));
        assert_eq!(
            read_request(&mut stream),
            json!({"type":"get_project_info","params":{}})
        );
        send_response(&mut stream, project_response(project(), "2022.3.fixture"));
        assert_eq!(
            read_request(&mut stream),
            json!({"type":"manage_scene","params":{"action":"get_active"}})
        );
        send_response(
            &mut stream,
            json!({"status":"success","result":{"success":true,"data":{
                "name":"Fixture Scene","path":"Assets/Fixture.unity","buildIndex":-1,
                "isDirty":true,"isLoaded":true,"rootCount":3
            }}}),
        );
    });
    let result = invoke(
        &window(),
        json!({"projectRoot":project(),"port":port,"allowClientTakeover":true}),
    )
    .expect("the fixture should answer the read-only query");
    assert_eq!(result["project"]["projectRoot"], project());
    assert_eq!(result["project"]["projectName"], "Fixture project");
    assert_eq!(result["project"]["unityVersion"], "2022.3.fixture");
    assert_eq!(result["scene"]["name"], "Fixture Scene");
    assert_eq!(result["scene"]["path"], "Assets/Fixture.unity");
    assert_eq!(result["scene"]["buildIndex"], -1);
    assert_eq!(result["scene"]["isDirty"], true);
    assert_eq!(result["scene"]["isLoaded"], true);
    assert_eq!(result["scene"]["rootCount"], 3);
    server.join().unwrap();
}

#[test]
fn wrong_project_is_an_error_before_any_scene_request() {
    let socket = listener();
    let port = socket.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let mut stream = accept(&socket);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_eq!(
            read_request(&mut stream),
            json!({"type":"get_project_info","params":{}})
        );
        send_response(&mut stream, project_response(other_project(), "fixture"));
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let error = invoke(
        &window(),
        json!({"projectRoot":project(),"port":port,"allowClientTakeover":true}),
    )
    .expect_err("wrong project must fail");
    assert!(error.to_string().contains("project mismatch"), "{error}");
    server.join().unwrap();
}

#[test]
fn authorized_wrong_project_can_already_have_displaced_the_existing_client() {
    let socket = listener();
    let port = socket.local_addr().unwrap().port();
    let mut existing_client = TcpStream::connect(socket.local_addr().unwrap()).unwrap();
    existing_client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (existing_server, _) = socket.accept().unwrap();
    let server = thread::spawn(move || {
        let mut new_client = accept(&socket);
        // Model the documented single-client server: it closes the old connection
        // as soon as it accepts the new one, before returning project information.
        existing_server.shutdown(std::net::Shutdown::Both).unwrap();
        new_client
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_eq!(
            read_request(&mut new_client),
            json!({"type":"get_project_info","params":{}})
        );
        send_response(
            &mut new_client,
            project_response(other_project(), "fixture"),
        );
    });
    let error = invoke(
        &window(),
        json!({"projectRoot":project(),"port":port,"allowClientTakeover":true}),
    )
    .expect_err("the mismatched project must still be rejected");
    assert!(error.to_string().contains("project mismatch"), "{error}");
    let mut byte = [0];
    assert_eq!(existing_client.read(&mut byte).unwrap(), 0);
    server.join().unwrap();
}

#[test]
fn project_is_checked_again_before_the_scene_query() {
    let socket = listener();
    let port = socket.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let mut stream = accept(&socket);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_eq!(
            read_request(&mut stream),
            json!({"type":"get_project_info","params":{}})
        );
        send_response(&mut stream, project_response(project(), "fixture"));
        assert_eq!(
            read_request(&mut stream),
            json!({"type":"get_project_info","params":{}})
        );
        send_response(&mut stream, project_response(other_project(), "fixture"));
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let error = invoke(
        &window(),
        json!({"projectRoot":project(),"port":port,"allowClientTakeover":true}),
    )
    .expect_err("changed project must stop the scene query");
    assert!(error.to_string().contains("project mismatch"), "{error}");
    server.join().unwrap();
}

#[test]
fn editor_errors_are_preserved() {
    let socket = listener();
    let port = socket.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let mut stream = accept(&socket);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_eq!(
            read_request(&mut stream),
            json!({"type":"get_project_info","params":{}})
        );
        send_response(
            &mut stream,
            json!({"status":"error","error":"resource disabled"}),
        );
    });
    let error = invoke(
        &window(),
        json!({"projectRoot":project(),"port":port,"allowClientTakeover":true}),
    )
    .expect_err("editor failure must remain a failure");
    assert!(error.to_string().contains("resource disabled"), "{error}");
    server.join().unwrap();
}

#[test]
fn scene_query_failure_does_not_return_a_partial_success() {
    let socket = listener();
    let port = socket.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let mut stream = accept(&socket);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        for _ in 0..2 {
            assert_eq!(
                read_request(&mut stream),
                json!({"type":"get_project_info","params":{}})
            );
            send_response(&mut stream, project_response(project(), "fixture"));
        }
        assert_eq!(
            read_request(&mut stream),
            json!({"type":"manage_scene","params":{"action":"get_active"}})
        );
        send_response(
            &mut stream,
            json!({"status":"error","error":"scene tool disabled"}),
        );
    });
    let error = invoke(
        &window(),
        json!({"projectRoot":project(),"port":port,"allowClientTakeover":true}),
    )
    .expect_err("a scene failure must not return project-only success");
    assert!(error.to_string().contains("scene tool disabled"), "{error}");
    server.join().unwrap();
}

#[test]
fn a_finished_failed_query_releases_the_port_for_a_new_confirmed_attempt() {
    let socket = listener();
    let port = socket.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        for error in ["first fixture error", "second fixture error"] {
            let mut stream = accept(&socket);
            stream
                .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
                .unwrap();
            assert_eq!(
                read_request(&mut stream),
                json!({"type":"get_project_info","params":{}})
            );
            send_response(&mut stream, json!({"status":"error","error":error}));
        }
    });
    let window = window();
    for expected in ["first fixture error", "second fixture error"] {
        let result = invoke(
            &window,
            json!({"projectRoot":project(),"port":port,"allowClientTakeover":true}),
        );
        assert!(result
            .expect_err("editor error must be returned")
            .to_string()
            .contains(expected));
    }
    server.join().unwrap();
}

#[test]
fn simultaneous_queries_cannot_open_two_connections_to_the_same_port() {
    let socket = listener();
    let port = socket.local_addr().unwrap().port();
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut stream = accept(&socket);
        accepted_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(
            matches!(socket.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_eq!(
            read_request(&mut stream),
            json!({"type":"get_project_info","params":{}})
        );
        send_response(
            &mut stream,
            json!({"status":"error","error":"fixture complete"}),
        );
    });
    let window = window();
    let first_window = window.clone();
    let first = thread::spawn(move || {
        invoke(
            &first_window,
            json!({"projectRoot":project(),"port":port,"allowClientTakeover":true}),
        )
    });
    accepted_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("first query should connect");
    let (status_tx, status_rx) = mpsc::channel();
    let status_window = window.clone();
    let status_thread = thread::spawn(move || {
        status_tx
            .send(invoke_command(&status_window, "host_status", json!({})))
            .unwrap();
    });
    let host_response = status_rx.recv_timeout(Duration::from_secs(1));
    let second = invoke(
        &window,
        json!({"projectRoot":project(),"port":port,"allowClientTakeover":true}),
    );
    release_tx.send(()).unwrap();
    assert_eq!(
        host_response.expect("host must stay responsive").unwrap()["backend"],
        "rust"
    );
    assert!(second
        .expect_err("duplicate query must fail")
        .to_string()
        .contains("already"));
    assert!(first.join().unwrap().is_err());
    status_thread.join().unwrap();
    server.join().unwrap();
}

#[test]
fn unity_agent_rejects_unapproved_or_unknown_targets_without_opening_a_socket() {
    let window = window();
    assert_eq!(
        invoke_command(
            &window,
            "attach_unity_agent_target",
            json!({"projectRoot":project(),"port":0,"allowClientTakeover":false}),
        ),
        Err(json!("takeover_required"))
    );
    assert_eq!(
        invoke_command(
            &window,
            "attach_unity_agent_target",
            json!({"projectRoot":project(),"port":0,"allowClientTakeover":true}),
        ),
        Err(json!("target_unavailable"))
    );
    assert_eq!(
        invoke_command(
            &window,
            "create_unity_agent",
            json!({"sessionId":"unknown","endpoint":"http://127.0.0.1:9/fixture","model":"fixture","credential":"synthetic"}),
        ),
        Err(json!("target_unavailable"))
    );
    assert_eq!(
        invoke_command(
            &window,
            "release_unity_agent_target",
            json!({"sessionId":"unknown"}),
        ),
        Err(json!("target_unavailable"))
    );
}

fn unity_agent_model_request(stream: &mut TcpStream) -> (String, Value) {
    use std::io::{BufRead, BufReader};
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    let mut size = None;
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            size = Some(value.trim().parse::<usize>().unwrap());
        }
        headers.push_str(&line);
    }
    let mut body = vec![0; size.unwrap()];
    reader.read_exact(&mut body).unwrap();
    (headers, serde_json::from_slice(&body).unwrap())
}

fn unity_agent_model_reply(stream: &mut TcpStream, delta: Value, finish: &str) {
    let event = json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}]});
    let body = format!("data: {event}\n\ndata: [DONE]\n\n");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}

#[test]
#[ignore = "requires a disposable Unity project and dedicated Coplay bridge port"]
fn unity_agent_queries_a_real_disposable_editor() {
    let project = std::path::PathBuf::from(std::env::var_os("UEBOX_TEST_UNITY_PROJECT").unwrap())
        .canonicalize()
        .unwrap();
    let temporary = std::env::temp_dir().canonicalize().unwrap();
    assert!(project.starts_with(temporary));
    assert!(project.join("Assets/UEBoxProbe.unity").is_file());
    let port: u16 = std::env::var("UEBOX_TEST_UNITY_PORT")
        .unwrap()
        .parse()
        .unwrap();
    assert!(port > 0 && port != 6400);

    let window = window();
    let target = invoke_command(
        &window,
        "attach_unity_agent_target",
        json!({"projectRoot":project,"port":port,"allowClientTakeover":true}),
    )
    .unwrap();
    let model = listener();
    let agent = invoke_command(
        &window,
        "create_unity_agent",
        json!({"sessionId":target["sessionId"],
            "endpoint":format!("http://{}/v1/chat/completions", model.local_addr().unwrap()),
            "model":"fixture","credential":null}),
    )
    .unwrap();
    let expected_root = project.to_str().unwrap().to_owned();
    let provider = thread::spawn(move || {
        let mut stream = accept(&model);
        let (_, request) = unity_agent_model_request(&mut stream);
        assert_eq!(request["tools"].as_array().unwrap().len(), 2);
        unity_agent_model_reply(
            &mut stream,
            json!({"tool_calls":[
                {"index":0,"id":"project","type":"function","function":{"name":"unity_project_info","arguments":"{}"}},
                {"index":1,"id":"scene","type":"function","function":{"name":"unity_active_scene","arguments":"{}"}}
            ]}),
            "tool_calls",
        );
        let mut stream = accept(&model);
        let (_, request) = unity_agent_model_request(&mut stream);
        let receipts: Vec<Value> = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "tool")
            .map(|message| serde_json::from_str(message["content"].as_str().unwrap()).unwrap())
            .collect();
        assert_eq!(receipts.len(), 2);
        assert!(receipts[0]["data"]["unity_version"]
            .as_str()
            .is_some_and(|version| !version.is_empty()));
        assert_eq!(receipts[1]["data"]["name"], "UEBoxProbe");
        assert_eq!(receipts[1]["data"]["path"], "Assets/UEBoxProbe.unity");
        assert!(!request.to_string().contains(&expected_root));
        unity_agent_model_reply(
            &mut stream,
            json!({"content":"Read live Unity scene"}),
            "stop",
        );
    });
    let run = invoke_command(
        &window,
        "start_unity_agent",
        json!({"agentId":agent["agentId"],"text":"Read the editor project and scene"}),
    )
    .unwrap();
    assert_eq!(
        unity_agent_poll_until(&window, &run, "completed"),
        "Read live Unity scene"
    );
    invoke_command(&window, "release_unity_agent_target", target).unwrap();
    provider.join().unwrap();
}

fn unity_agent_answer_ping(stream: &mut TcpStream) {
    let mut frame = [0; 12];
    stream.read_exact(&mut frame).unwrap();
    assert_eq!(&frame, b"\0\0\0\0\0\0\0\x04ping");
    send_response(
        stream,
        json!({"status":"success","result":{"message":"pong"}}),
    );
}

fn unity_agent_poll_until(
    window: &WebviewWindow<MockRuntime>,
    args: &Value,
    phase: &str,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut text = String::new();
    loop {
        let update = invoke_command(window, "poll_unity_agent", args.clone()).unwrap();
        for fragment in update["fragments"].as_array().unwrap() {
            text.push_str(fragment.as_str().unwrap());
        }
        if update["phase"] == phase {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "last Unity Agent update: {update}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn unity_agent_reuses_one_socket_and_holds_the_port_until_a_cancelled_read_drains() {
    use tauri::Manager;
    const SECRET: &str = "synthetic-unity-agent-bearer";
    let project = tempfile::tempdir().unwrap();
    let root = project.path().canonicalize().unwrap();
    let root = root.to_str().unwrap();
    let socket = listener();
    let port = socket.local_addr().unwrap().port();
    let editor_root = root.to_owned();
    let (reading_tx, reading_rx) = mpsc::channel();
    let (finish_read_tx, finish_read_rx) = mpsc::channel();
    let editor = thread::spawn(move || {
        let mut stream = accept(&socket);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_eq!(read_request(&mut stream)["type"], "get_project_info");
        send_response(
            &mut stream,
            project_response(&editor_root, "fixture-version"),
        );
        unity_agent_answer_ping(&mut stream);
        assert_eq!(read_request(&mut stream)["type"], "get_project_info");
        send_response(
            &mut stream,
            project_response(&editor_root, "fixture-version"),
        );
        unity_agent_answer_ping(&mut stream);
        assert_eq!(read_request(&mut stream)["type"], "get_project_info");
        reading_tx.send(()).unwrap();
        finish_read_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        send_response(
            &mut stream,
            project_response(&editor_root, "fixture-version"),
        );
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        assert!(
            matches!(socket.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        socket
    });
    let window = window();
    let other = tauri::WebviewWindowBuilder::new(window.app_handle(), "other", Default::default())
        .build()
        .unwrap();
    let attach_args = json!({"projectRoot":root,"port":port,"allowClientTakeover":true});
    let attached =
        invoke_command(&window, "attach_unity_agent_target", attach_args.clone()).unwrap();
    let target_args = json!({"sessionId":attached["sessionId"]});
    assert_eq!(attached.as_object().unwrap().len(), 1);
    assert_eq!(
        invoke_command(&other, "release_unity_agent_target", target_args.clone()),
        Err(json!("target_unavailable"))
    );
    assert_eq!(
        invoke_command(&window, "attach_unity_agent_target", attach_args.clone()),
        Err(json!("target_busy"))
    );
    assert!(invoke(&window, attach_args.clone())
        .unwrap_err()
        .to_string()
        .contains("already"));

    let model = listener();
    model.set_nonblocking(true).unwrap();
    let config = json!({
        "sessionId":attached["sessionId"],
        "endpoint":format!("http://{}/v1/chat/completions", model.local_addr().unwrap()),
        "model":"fixture",
        "credential":SECRET
    });
    assert_eq!(
        invoke_command(&other, "create_unity_agent", config.clone()),
        Err(json!("target_unavailable"))
    );
    let created = invoke_command(&window, "create_unity_agent", config).unwrap();
    assert!(matches!(model.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    let model_root = root.to_owned();
    let model_worker = thread::spawn(move || {
        for index in 0..3 {
            let mut stream = accept(&model);
            let (headers, request) = unity_agent_model_request(&mut stream);
            assert!(headers
                .to_ascii_lowercase()
                .contains(&format!("authorization: bearer {SECRET}")));
            assert!(!request.to_string().contains(SECRET));
            if index == 1 {
                let receipt = request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|message| message["role"] == "tool")
                    .unwrap();
                let receipt: Value =
                    serde_json::from_str(receipt["content"].as_str().unwrap()).unwrap();
                assert!(receipt["data"].get("project_root").is_none());
                assert!(receipt["data"].get("assets_path").is_none());
                assert!(!receipt.to_string().contains(&model_root));
                assert_eq!(receipt["data"]["unity_version"], "fixture-version");
                unity_agent_model_reply(&mut stream, json!({"content":"Fixture answer"}), "stop");
            } else {
                unity_agent_model_reply(
                    &mut stream,
                    json!({"tool_calls":[{
                        "index":0, "id":format!("call-{index}"), "type":"function",
                        "function":{"name":"unity_project_info","arguments":"{}"}
                    }]}),
                    "tool_calls",
                );
            }
        }
        model
    });
    let send = json!({"agentId":created["agentId"],"text":"Read selected project"});
    let first = invoke_command(&window, "start_unity_agent", send.clone()).unwrap();
    assert_eq!(
        unity_agent_poll_until(&window, &first, "completed"),
        "Fixture answer"
    );
    let second = invoke_command(&window, "start_unity_agent", send.clone()).unwrap();
    reading_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(
        invoke_command(&other, "cancel_unity_agent", second.clone()),
        Err(json!("agent_unavailable"))
    );
    invoke_command(&window, "cancel_unity_agent", second.clone()).unwrap();
    assert!(unity_agent_poll_until(&window, &second, "draining").is_empty());
    assert_eq!(
        invoke_command(&window, "start_unity_agent", send.clone()),
        Err(json!("agent_busy"))
    );

    let release_window = window.clone();
    let command_args = target_args.clone();
    let (closed_tx, closed_rx) = mpsc::channel();
    let release = thread::spawn(move || {
        closed_tx
            .send(invoke_command(
                &release_window,
                "release_unity_agent_target",
                command_args,
            ))
            .unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let update = invoke_command(&window, "poll_unity_agent", second.clone()).unwrap();
        if update["error"] == "target_unavailable" {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    assert!(closed_rx.try_recv().is_err());
    assert_eq!(
        invoke_command(&window, "attach_unity_agent_target", attach_args.clone()),
        Err(json!("target_busy"))
    );
    finish_read_tx.send(()).unwrap();
    closed_rx
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap();
    release.join().unwrap();
    assert!(unity_agent_poll_until(&window, &second, "failed").is_empty());
    assert_eq!(
        invoke_command(&window, "start_unity_agent", send),
        Err(json!("agent_unavailable"))
    );
    let model = model_worker.join().unwrap();
    assert!(matches!(model.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    let socket = editor.join().unwrap();
    let editor_root_for_replacement = root.to_owned();
    let verification = thread::spawn(move || {
        let mut stream = accept(&socket);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_eq!(read_request(&mut stream)["type"], "get_project_info");
        send_response(
            &mut stream,
            project_response(&editor_root_for_replacement, "replacement"),
        );
    });
    let replacement = invoke_command(&window, "attach_unity_agent_target", attach_args).unwrap();
    assert_ne!(replacement["sessionId"], attached["sessionId"]);
    verification.join().unwrap();
    let agent = invoke_command(
        &window,
        "create_unity_agent",
        json!({
            "sessionId":replacement["sessionId"],"model":"fixture",
            "endpoint":format!("http://{}/v1/chat/completions", model.local_addr().unwrap())
        }),
    )
    .unwrap();
    let send = json!({"agentId":agent["agentId"],"text":"Do not contact the model after EOF"});
    assert_eq!(
        invoke_command(&window, "start_unity_agent", send.clone()),
        Err(json!("target_unavailable"))
    );
    assert_eq!(
        invoke_command(&window, "start_unity_agent", send),
        Err(json!("agent_unavailable"))
    );
    assert!(matches!(model.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
}

#[test]
#[cfg(feature = "unreal")]
fn pending_unity_query_does_not_block_unreal_listener_lifecycle() {
    let unity_socket = listener();
    let unity_port = unity_socket.local_addr().unwrap().port();
    let temporary = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let unreal_port = temporary.local_addr().unwrap().port();
    assert_ne!(unreal_port, 17860);
    assert_ne!(unreal_port, 6400);
    drop(temporary);

    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut stream = accept(&unity_socket);
        accepted_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_eq!(
            read_request(&mut stream),
            json!({"type":"get_project_info","params":{}})
        );
        send_response(
            &mut stream,
            json!({"status":"error","error":"fixture complete"}),
        );
    });
    let window = window();
    let query_window = window.clone();
    let query = thread::spawn(move || {
        invoke(
            &query_window,
            json!({"projectRoot":project(),"port":unity_port,"allowClientTakeover":true}),
        )
    });
    accepted_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("Unity query must connect");
    let started = invoke_command(
        &window,
        "start_unreal_listener",
        json!({"port":unreal_port}),
    )
    .unwrap();
    assert_eq!(started["port"], unreal_port);
    assert_eq!(
        invoke_command(&window, "list_unreal_instances", json!({})).unwrap(),
        json!([])
    );
    invoke_command(&window, "stop_unreal_listener", json!({})).unwrap();
    TcpListener::bind((Ipv4Addr::LOCALHOST, unreal_port)).expect("Unreal port must be released");
    release_tx.send(()).unwrap();
    assert!(query
        .join()
        .unwrap()
        .expect_err("fixture returns an error")
        .to_string()
        .contains("fixture complete"));
    server.join().unwrap();
}

#[test]
#[ignore = "requires the disposable Unity rename fixture and dedicated Coplay bridge port"]
fn unity_agent_approves_rename_in_a_real_disposable_editor() {
    let project = std::path::PathBuf::from(std::env::var_os("UEBOX_TEST_UNITY_PROJECT").unwrap())
        .canonicalize()
        .unwrap();
    let temporary = project.starts_with(std::env::temp_dir().canonicalize().unwrap());
    #[cfg(unix)]
    let temporary = temporary
        || std::path::Path::new("/tmp")
            .canonicalize()
            .is_ok_and(|root| project.starts_with(root));
    assert!(temporary);
    let fixture_scene = project.join("Assets/UEBoxRenameProbe.unity");
    assert!(fixture_scene.is_file());
    let original_archive = std::fs::read(&fixture_scene).unwrap();
    let port: u16 = std::env::var("UEBOX_TEST_UNITY_PORT")
        .unwrap()
        .parse()
        .unwrap();
    assert!(port > 0 && port != 6400);

    let window = window();
    use tauri::Manager;
    let other = tauri::WebviewWindowBuilder::new(
        window.app_handle(),
        "approval-outsider",
        Default::default(),
    )
    .build()
    .unwrap();
    let target = invoke_command(
        &window,
        "attach_unity_agent_target",
        json!({"projectRoot":project,"port":port,"allowClientTakeover":true}),
    )
    .unwrap();
    let model = listener();
    let agent = invoke_command(
        &window,
        "create_unity_agent",
        json!({"sessionId":target["sessionId"],
            "endpoint":format!("http://{}/v1/chat/completions", model.local_addr().unwrap()),
            "model":"fixture","credential":null,"allowGameObjectRename":true}),
    )
    .unwrap();
    let provider = thread::spawn(move || {
        let mut stream = accept(&model);
        let (_, request) = unity_agent_model_request(&mut stream);
        let tools = request["tools"].as_array().unwrap();
        assert!(tools
            .iter()
            .any(|tool| tool["function"]["name"] == "unity_rename_selected_game_object"));
        unity_agent_model_reply(
            &mut stream,
            json!({"tool_calls":[
                {"index":0,"id":"name","type":"function","function":{
                    "name":"unity_rename_selected_game_object",
                    "arguments":r#"{"name":"Approved Fixture Target"}"#
                }}
            ]}),
            "tool_calls",
        );
        let mut stream = accept(&model);
        let (_, request) = unity_agent_model_request(&mut stream);
        let receipt = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "tool")
            .expect("The approved editor result reaches the model");
        assert!(receipt["content"]
            .as_str()
            .unwrap()
            .contains("Approved Fixture Target"));
        unity_agent_model_reply(
            &mut stream,
            json!({"content":"Editor read-back confirmed"}),
            "stop",
        );
    });

    let run = invoke_command(
        &window,
        "start_unity_agent",
        json!({"agentId":agent["agentId"],"text":"Rename the selected fixture object"}),
    )
    .unwrap();
    let activity_args = json!({"agentId":agent["agentId"]});
    let approval_id = {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let response =
                invoke_command(&window, "list_unity_name_activity", activity_args.clone()).unwrap();
            if let Some(approval) = response["approvals"].as_array().unwrap().first() {
                assert_eq!(approval["oldName"], "Original Fixture Target");
                assert_eq!(approval["newName"], "Approved Fixture Target");
                break approval["approvalId"].clone();
            }
            assert!(
                Instant::now() < deadline,
                "No approval arrived from the real editor"
            );
            thread::sleep(Duration::from_millis(40));
        }
    };
    assert_eq!(
        invoke_command(&window, "poll_unity_agent", run.clone()).unwrap()["phase"],
        "awaiting_approval"
    );
    assert!(
        invoke_command(&other, "list_unity_name_activity", activity_args.clone()).unwrap()
            ["approvals"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let answer = json!({"approvalId":approval_id,"approved":true});
    assert_eq!(
        invoke_command(&other, "answer_unity_name_approval", answer.clone()),
        Err(json!("approval_unavailable"))
    );
    invoke_command(&window, "answer_unity_name_approval", answer.clone()).unwrap();
    assert_eq!(
        invoke_command(&window, "answer_unity_name_approval", answer),
        Err(json!("approval_unavailable"))
    );
    assert_eq!(
        unity_agent_poll_until(&window, &run, "completed"),
        "Editor read-back confirmed"
    );
    let activity = invoke_command(&window, "list_unity_name_activity", activity_args).unwrap();
    assert_eq!(activity["receipts"][0]["status"], "applied");
    assert_eq!(
        activity["receipts"][0]["actualName"],
        "Approved Fixture Target"
    );
    assert_eq!(std::fs::read(fixture_scene).unwrap(), original_archive);
    invoke_command(&window, "release_unity_agent_target", target).unwrap();
    provider.join().unwrap();
}
