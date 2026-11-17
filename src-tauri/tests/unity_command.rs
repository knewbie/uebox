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
