use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;
use uebox_engine_unity::{BridgeError, BridgeOptions, Target, UnityClient};

#[cfg(windows)]
const PARENT_ROOT: &str = r"C:\tmp\one\..\other";
#[cfg(not(windows))]
const PARENT_ROOT: &str = "/tmp/one/../other";

fn project_a() -> &'static str {
    static PROJECT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PROJECT.get_or_init(|| fixture_path("Project A"))
}

fn project_other() -> &'static str {
    static PROJECT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PROJECT.get_or_init(|| fixture_path("Other"))
}

fn fixture_path(name: &str) -> String {
    std::fs::canonicalize(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
    .to_str()
    .unwrap()
    .to_owned()
}

fn listener() -> TcpListener {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    assert_ne!(listener.local_addr().unwrap().port(), 6400);
    listener
}

fn accept(listener: &TcpListener) -> TcpStream {
    let (stream, _) = listener.accept().unwrap();
    stream.set_nodelay(true).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
}

fn assert_closed(stream: &mut TcpStream) {
    let mut byte = [0];
    match stream.read(&mut byte) {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            ) => {}
        result => panic!("expected a closed connection, got {result:?}"),
    }
}

fn read_frame(stream: &mut TcpStream) -> Vec<u8> {
    let mut header = [0_u8; 8];
    stream.read_exact(&mut header).unwrap();
    let length = u64::from_be_bytes(header) as usize;
    assert!(length > 0 && length < 4096);
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).unwrap();
    payload
}

fn write_frame(stream: &mut TcpStream, payload: &str) {
    stream
        .write_all(&(payload.len() as u64).to_be_bytes())
        .unwrap();
    for chunk in payload.as_bytes().chunks(7) {
        stream.write_all(chunk).unwrap();
    }
}

fn assert_project_request(stream: &mut TcpStream) {
    let request: serde_json::Value = serde_json::from_slice(&read_frame(stream)).unwrap();
    assert_eq!(
        request,
        serde_json::json!({"type": "get_project_info", "params": {}})
    );
}

fn project_response(root: &str) -> String {
    serde_json::json!({
        "status": "success",
        "result": {"success": true, "message": "Retrieved project info.", "data": {
            "projectRoot": root, "projectName": "Project A", "unityVersion": "2022.3.62f3",
            "assetsPath": format!("{root}/Assets"), "platform": "StandaloneOSX",
            "renderPipeline": "BuiltIn"
        }}
    })
    .to_string()
}

#[test]
fn verifies_project_and_reads_active_scene_over_the_framed_socket() {
    let listener = listener();
    let address: SocketAddr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(b"WELCOME UNITY-").unwrap();
        stream.write_all(b"MCP 1 FRAMING=1\n").unwrap();
        assert_project_request(&mut stream);
        write_frame(&mut stream, &project_response(project_a()));
        assert_project_request(&mut stream);
        write_frame(&mut stream, &project_response(project_a()));
        let scene_request: serde_json::Value =
            serde_json::from_slice(&read_frame(&mut stream)).unwrap();
        assert_eq!(
            scene_request,
            serde_json::json!({"type": "manage_scene", "params": {"action": "get_active"}})
        );
        write_frame(
            &mut stream,
            r#"{"status":"success","result":{"success":true,"message":"Retrieved active scene information.","data":{"name":"Level 1","path":"Assets/Level 1.unity","buildIndex":-1,"isDirty":true,"isLoaded":true,"rootCount":3}}}"#,
        );
    });

    let target = Target::new(address, project_a()).unwrap();
    let mut client =
        UnityClient::connect_authorized(target.authorize_takeover(), BridgeOptions::default())
            .unwrap();
    assert_eq!(
        client.last_observed_project_info().project_root,
        project_a()
    );
    assert_eq!(
        client.last_observed_project_info().unity_version,
        "2022.3.62f3"
    );
    let scene = client.active_scene().unwrap();
    assert_eq!(scene.name, "Level 1");
    assert_eq!(scene.path, "Assets/Level 1.unity");
    assert_eq!(scene.build_index, -1);
    assert!(scene.is_dirty && scene.is_loaded);
    assert_eq!(scene.root_count, 3);
    server.join().unwrap();
}

fn connect(address: SocketAddr, path: &str, timeout_ms: u64) -> Result<UnityClient, BridgeError> {
    UnityClient::connect_authorized(
        Target::new(address, path)?.authorize_takeover(),
        BridgeOptions {
            io_timeout: Duration::from_millis(timeout_ms),
            max_frame_bytes: 1024,
        },
    )
}

#[cfg(unix)]
#[test]
fn selected_symlink_accepts_canonical_editor_path_on_each_read() {
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp = TempDir(
        std::env::temp_dir().join(format!("uebox-unity-alias-{}-{unique}", std::process::id())),
    );
    std::fs::create_dir(&temp.0).unwrap();
    let project = temp.0.join("Actual Project");
    let alias = temp.0.join("Project Alias");
    let other_alias = temp.0.join("Other Alias");
    std::fs::create_dir(&project).unwrap();
    let project = std::fs::canonicalize(project).unwrap();
    symlink(&project, &alias).unwrap();
    symlink(&project, &other_alias).unwrap();
    let alias = alias.to_str().unwrap().to_owned();
    let other_alias = other_alias.to_str().unwrap().to_owned();
    let reported = project.to_str().unwrap().to_owned();

    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        for _ in 0..3 {
            assert_project_request(&mut stream);
            write_frame(&mut stream, &project_response(&reported));
        }
        let request: serde_json::Value = serde_json::from_slice(&read_frame(&mut stream)).unwrap();
        assert_eq!(request["type"], "manage_scene");
        assert_eq!(request["params"]["action"], "get_active");
        write_frame(&mut stream, &serde_json::json!({"status":"success","result":{"success":true,"data":{"name":"Alias scene","path":"","buildIndex":-1,"isDirty":false,"isLoaded":true,"rootCount":0}}}).to_string());
        assert_project_request(&mut stream);
        write_frame(&mut stream, &project_response(&other_alias));
        assert_closed(&mut stream);
    });
    let mut client = connect(address, &alias, 1000).unwrap();
    assert_eq!(client.target().expected_project_root, alias);
    assert_eq!(
        client.project_info().unwrap().project_root,
        project.to_str().unwrap()
    );
    assert_eq!(client.active_scene().unwrap().name, "Alias scene");
    assert!(matches!(
        client.project_info(),
        Err(BridgeError::ProjectMismatch { .. })
    ));
    assert!(matches!(
        client.project_info(),
        Err(BridgeError::Disconnected)
    ));
    server.join().unwrap();
}

#[test]
fn connect_without_takeover_authorization_never_opens_a_socket() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let mut old_client = TcpStream::connect(address).unwrap();
    old_client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (mut old_editor_side, _) = listener.accept().unwrap();
    let target = Target::new(address, project_a()).unwrap();
    let options = BridgeOptions {
        io_timeout: Duration::from_millis(100),
        max_frame_bytes: 1024,
    };
    let result = UnityClient::connect(target, options);
    assert!(matches!(result, Err(BridgeError::AuthorizationRequired)));
    listener.set_nonblocking(true).unwrap();
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "an unauthorized connection reached the editor"
    );
    old_editor_side.write_all(b"still connected").unwrap();
    let mut old_reply = [0; 15];
    old_client.read_exact(&mut old_reply).unwrap();
    assert_eq!(&old_reply, b"still connected");
}

#[test]
fn authorized_connection_rejects_missing_project_before_opening_a_socket() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let missing = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/no-such-unity-project");
    assert!(!missing.exists());
    let target = Target::new(address, missing.to_str().unwrap()).unwrap();
    let options = BridgeOptions {
        io_timeout: Duration::from_millis(100),
        max_frame_bytes: 1024,
    };
    let result = UnityClient::connect_authorized(target.authorize_takeover(), options);
    assert!(matches!(result, Err(BridgeError::InvalidTarget(_))));
    listener.set_nonblocking(true).unwrap();
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn authorized_connection_rejects_project_file_before_opening_a_socket() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let file =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/Project A/.gitkeep");
    let target = Target::new(address, file.to_str().unwrap()).unwrap();
    let options = BridgeOptions {
        io_timeout: Duration::from_millis(100),
        max_frame_bytes: 1024,
    };
    let result = UnityClient::connect_authorized(target.authorize_takeover(), options);
    assert!(matches!(result, Err(BridgeError::InvalidTarget(_))));
    listener.set_nonblocking(true).unwrap();
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn explicit_authorization_allows_takeover_of_the_selected_connection() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let mut old_client = TcpStream::connect(address).unwrap();
    old_client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (old_editor_side, _) = listener.accept().unwrap();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        old_editor_side.shutdown(std::net::Shutdown::Both).unwrap();
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        write_frame(&mut stream, &project_response(project_a()));
    });
    let approved = Target::new(address, project_a())
        .unwrap()
        .authorize_takeover();
    let _client = UnityClient::connect_authorized(approved, BridgeOptions::default()).unwrap();
    assert_closed(&mut old_client);
    server.join().unwrap();
}

#[test]
fn ping_uses_the_existing_framed_connection_without_reconnecting() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        write_frame(&mut stream, &project_response(project_a()));
        assert_eq!(read_frame(&mut stream), b"ping");
        write_frame(
            &mut stream,
            r#"{"status":"success","result":{"message":"pong"}}"#,
        );
    });
    let mut client = connect(address, project_a(), 1000).unwrap();
    client.ping().unwrap();
    server.join().unwrap();
}

#[test]
fn invalid_ping_response_closes_the_existing_connection() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        write_frame(&mut stream, &project_response(project_a()));
        assert_eq!(read_frame(&mut stream), b"ping");
        write_frame(
            &mut stream,
            r#"{"status":"success","result":{"message":"not pong"}}"#,
        );
        assert_closed(&mut stream);
    });
    let mut client = connect(address, project_a(), 1000).unwrap();
    assert!(matches!(
        client.ping(),
        Err(BridgeError::InvalidResponse(_))
    ));
    assert!(matches!(client.ping(), Err(BridgeError::Disconnected)));
    server.join().unwrap();
}

#[test]
fn disconnected_client_does_not_reattach_to_a_replacement_on_the_same_port() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        write_frame(&mut stream, &project_response(project_a()));
        assert_project_request(&mut stream);
        stream.shutdown(std::net::Shutdown::Both).unwrap();
        listener
    });
    let mut client = connect(address, project_a(), 1000).unwrap();
    assert!(matches!(
        client.project_info(),
        Err(BridgeError::Disconnected)
    ));
    let listener = server.join().unwrap();
    listener.set_nonblocking(true).unwrap();
    assert!(matches!(
        client.project_info(),
        Err(BridgeError::Disconnected)
    ));
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn first_eof_after_a_connected_request_is_reported_as_disconnected() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        write_frame(&mut stream, &project_response(project_a()));
        assert_project_request(&mut stream);
        stream.shutdown(std::net::Shutdown::Both).unwrap();
    });
    let mut client = connect(address, project_a(), 1000).unwrap();
    assert!(matches!(
        client.project_info(),
        Err(BridgeError::Disconnected)
    ));
    assert_eq!(
        client.last_observed_project_info().project_root,
        project_a()
    );
    assert!(matches!(
        client.project_info(),
        Err(BridgeError::Disconnected)
    ));
    server.join().unwrap();
}

#[test]
fn rejects_nonlocal_or_ambiguous_targets_without_connecting() {
    assert!(matches!(
        Target::new("192.0.2.1:1234".parse().unwrap(), project_a()),
        Err(BridgeError::InvalidTarget(_))
    ));
    assert!(matches!(
        Target::new("127.0.0.1:0".parse().unwrap(), project_a()),
        Err(BridgeError::InvalidTarget(_))
    ));
    assert!(matches!(
        Target::new("127.0.0.1:1234".parse().unwrap(), "relative/project"),
        Err(BridgeError::InvalidTarget(_))
    ));
    assert!(matches!(
        Target::new("127.0.0.1:1234".parse().unwrap(), PARENT_ROOT),
        Err(BridgeError::InvalidTarget(_))
    ));
}

#[test]
fn rejects_wrong_project_before_any_scene_query() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        write_frame(&mut stream, &project_response(project_other()));
        assert_closed(&mut stream);
    });
    assert!(matches!(
        connect(address, project_a(), 1000),
        Err(BridgeError::ProjectMismatch { .. })
    ));
    server.join().unwrap();
}

#[test]
fn rechecks_project_identity_before_scene_read() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        write_frame(&mut stream, &project_response(project_a()));
        assert_project_request(&mut stream);
        write_frame(&mut stream, &project_response(project_other()));
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        assert_closed(&mut stream);
    });
    let mut client = connect(address, project_a(), 1000).unwrap();
    assert!(matches!(
        client.active_scene(),
        Err(BridgeError::ProjectMismatch { .. })
    ));
    assert!(matches!(
        client.active_scene(),
        Err(BridgeError::Disconnected)
    ));
    server.join().unwrap();
}

#[test]
fn rejects_bad_and_oversized_welcome() {
    for greeting in [b"WELCOME OTHER 1 FRAMING=1\n".to_vec(), vec![b'x'; 129]] {
        let listener = listener();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut stream = accept(&listener);
            let _ = stream.write_all(&greeting);
        });
        assert!(matches!(
            connect(address, project_a(), 1000),
            Err(BridgeError::InvalidHandshake)
        ));
        server.join().unwrap();
    }
}

#[test]
fn rejects_zero_or_oversized_frames_before_reading_payload() {
    for size in [0_u64, 1025, u64::MAX] {
        let listener = listener();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut stream = accept(&listener);
            stream
                .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
                .unwrap();
            assert_project_request(&mut stream);
            stream.write_all(&size.to_be_bytes()).unwrap();
        });
        assert!(
            matches!(connect(address, project_a(), 1000), Err(BridgeError::FrameSize { size: actual, max: 1024 }) if actual == size)
        );
        server.join().unwrap();
    }
}

#[test]
fn times_out_on_a_partial_handshake_and_on_a_partial_frame() {
    for handshake in [true, false] {
        let listener = listener();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut stream = accept(&listener);
            if handshake {
                stream.write_all(b"WELCOME ").unwrap();
            } else {
                stream
                    .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
                    .unwrap();
                assert_project_request(&mut stream);
                stream.write_all(&10_u64.to_be_bytes()).unwrap();
                stream.write_all(b"{").unwrap();
            }
            thread::sleep(Duration::from_millis(180));
        });
        assert!(matches!(
            connect(address, project_a(), 80),
            Err(BridgeError::Timeout(_))
        ));
        server.join().unwrap();
    }
}

#[test]
fn timeout_is_a_whole_frame_deadline_even_if_bytes_keep_arriving() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        stream.write_all(&20_u64.to_be_bytes()).unwrap();
        for _ in 0..20 {
            if stream.write_all(b" ").is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
    });
    assert!(matches!(
        connect(address, project_a(), 100),
        Err(BridgeError::Timeout(_))
    ));
    server.join().unwrap();
}

#[test]
fn distinguishes_remote_errors_malformed_responses_and_disconnects() {
    for (wire, remote) in [
        (r#"{"status":"error","error":"resource disabled"}"#, true),
        (
            r#"{"status":"success","result":{"success":false,"error":"resource disabled"}}"#,
            true,
        ),
        (
            r#"{"status":"success","result":{"success":true,"data":{}}}"#,
            false,
        ),
        ("not-json", false),
    ] {
        let listener = listener();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut stream = accept(&listener);
            stream
                .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
                .unwrap();
            assert_project_request(&mut stream);
            write_frame(&mut stream, wire);
        });
        let result = connect(address, project_a(), 1000);
        if remote {
            assert!(
                matches!(result, Err(BridgeError::Remote(message)) if message.contains("resource disabled"))
            );
        } else {
            assert!(matches!(result, Err(BridgeError::InvalidResponse(_))));
        }
        server.join().unwrap();
    }

    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        stream.write_all(&20_u64.to_be_bytes()).unwrap();
        stream.write_all(b"{\"status\"").unwrap();
    });
    assert!(matches!(
        connect(address, project_a(), 1000),
        Err(BridgeError::Disconnected)
    ));
    server.join().unwrap();
}

#[test]
fn separates_explicit_endpoints_even_for_the_same_project() {
    let mut servers = Vec::new();
    let mut addresses = Vec::new();
    for scene_name in ["First editor", "Second editor"] {
        let listener = listener();
        addresses.push(listener.local_addr().unwrap());
        servers.push(thread::spawn(move || {
            let mut stream = accept(&listener);
            stream.write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n").unwrap();
            for _ in 0..2 {
                assert_project_request(&mut stream);
                write_frame(&mut stream, &project_response(project_a()));
            }
            let request: serde_json::Value = serde_json::from_slice(&read_frame(&mut stream)).unwrap();
            assert_eq!(request["params"]["action"], "get_active");
            write_frame(&mut stream, &serde_json::json!({"status":"success","result":{"success":true,"data":{"name":scene_name,"path":"","buildIndex":-1,"isDirty":false,"isLoaded":true,"rootCount":0}}}).to_string());
        }));
    }
    let mut first = connect(addresses[0], project_a(), 1000).unwrap();
    let mut second = connect(addresses[1], project_a(), 1000).unwrap();
    assert_eq!(second.active_scene().unwrap().name, "Second editor");
    assert_eq!(first.active_scene().unwrap().name, "First editor");
    for server in servers {
        server.join().unwrap();
    }
}

#[test]
fn project_query_refreshes_the_identity_from_the_editor() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        write_frame(&mut stream, &project_response(project_a()));
        assert_project_request(&mut stream);
        let updated = project_response(project_a()).replace("2022.3.62f3", "2023.2.1f1");
        write_frame(&mut stream, &updated);
    });
    let mut client = connect(address, project_a(), 1000).unwrap();
    assert_eq!(
        client.last_observed_project_info().unity_version,
        "2022.3.62f3"
    );
    assert_eq!(client.project_info().unwrap().unity_version, "2023.2.1f1");
    assert_eq!(
        client.last_observed_project_info().unity_version,
        "2023.2.1f1"
    );
    server.join().unwrap();
}

#[test]
fn scene_response_timeout_permanently_closes_the_connection() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        for _ in 0..2 {
            assert_project_request(&mut stream);
            write_frame(&mut stream, &project_response(project_a()));
        }
        let _: serde_json::Value = serde_json::from_slice(&read_frame(&mut stream)).unwrap();
        thread::sleep(Duration::from_millis(160));
        let _ = stream.write_all(&10_u64.to_be_bytes());
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        assert_closed(&mut stream);
    });
    let mut client = connect(address, project_a(), 80).unwrap();
    assert!(matches!(
        client.active_scene(),
        Err(BridgeError::Timeout(_))
    ));
    assert!(matches!(
        client.active_scene(),
        Err(BridgeError::Disconnected)
    ));
    server.join().unwrap();
}

#[test]
fn rejects_unbounded_options_before_opening_a_socket() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    for options in [
        BridgeOptions {
            io_timeout: Duration::ZERO,
            max_frame_bytes: 1024,
        },
        BridgeOptions {
            io_timeout: Duration::from_secs(31),
            max_frame_bytes: 1024,
        },
        BridgeOptions {
            io_timeout: Duration::from_secs(1),
            max_frame_bytes: 0,
        },
        BridgeOptions {
            io_timeout: Duration::from_secs(1),
            max_frame_bytes: 64 * 1024 * 1024 + 1,
        },
    ] {
        let target = Target::new(address, project_a()).unwrap();
        assert!(matches!(
            UnityClient::connect_authorized(target.authorize_takeover(), options),
            Err(BridgeError::InvalidOptions(_))
        ));
    }
    listener.set_nonblocking(true).unwrap();
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn guarded_rename_validates_readback_and_never_retries_after_a_lost_reply() {
    use serde_json::{json, Value};
    use uebox_engine_unity::GameObjectNameOutcome;

    for lose_reply in [false, true] {
        let listener = listener();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut stream = accept(&listener);
            stream
                .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
                .unwrap();
            for _ in 0..2 {
                assert_project_request(&mut stream);
                write_frame(&mut stream, &project_response(project_a()));
            }
            let command = "uebox_prepare_or_apply_gameobject_name";
            let prepare: Value = serde_json::from_slice(&read_frame(&mut stream)).unwrap();
            assert_eq!(
                prepare,
                json!({"type":command,"params":{"action":"prepare","newName":"New"}})
            );
            let ticket = "0123456789abcdef".repeat(4);
            let receipt = |data| {
                json!({"status":"success", "result":{"success":true,"data":data}}).to_string()
            };
            write_frame(
                &mut stream,
                &receipt(json!({
                    "preview":{"sceneHandle":12,"gameObjectInstanceId":-19,"oldName":"Old"},
                    "proposedName":"New","ticket":ticket,"expiresInSeconds":30.0
                })),
            );
            assert_project_request(&mut stream);
            write_frame(&mut stream, &project_response(project_a()));
            let apply: Value = serde_json::from_slice(&read_frame(&mut stream)).unwrap();
            assert_eq!(
                apply,
                json!({"type":command,"params":{"action":"apply","ticket":ticket}})
            );
            if !lose_reply {
                write_frame(
                    &mut stream,
                    &receipt(json!({
                        "status":"applied","sceneHandle":12,"gameObjectInstanceId":-19,
                        "oldName":"Old","newName":"New","saved":false
                    })),
                );
            }
        });

        let mut client = connect(address, project_a(), 500).unwrap();
        let proposal = client.prepare_selected_game_object_name("New").unwrap();
        assert_eq!(proposal.old_name(), "Old");
        assert_eq!(proposal.scene_handle(), 12);
        assert_eq!(proposal.game_object_instance_id(), -19);
        assert_eq!(
            client.apply_selected_game_object_name(proposal),
            if lose_reply {
                GameObjectNameOutcome::Unknown
            } else {
                GameObjectNameOutcome::Applied {
                    actual_name: "New".to_owned(),
                }
            }
        );
        if lose_reply {
            assert!(matches!(
                client.project_info(),
                Err(BridgeError::Disconnected)
            ));
        }
        server.join().unwrap();
    }
}
