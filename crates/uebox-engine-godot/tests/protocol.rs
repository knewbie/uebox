use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpListener},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use uebox_engine_godot::{GodotClient, Target};
#[path = "support/paths.rs"]
mod paths;

const INSTANCE: &str = "0123456789abcdef0123456789abcdef";
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

struct Project(PathBuf);
impl Project {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "uebox-godot-protocol-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("project.godot"), "config_version=5\n").unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn peer<F>(handler: F) -> (SocketAddr, thread::JoinHandle<()>)
where
    F: FnOnce(Value) -> Value + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let task = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(value) => break value,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("peer failed to accept: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request = serde_json::from_str(&line).unwrap();
        let response = handler(request);
        writeln!(stream, "{response}").unwrap();
    });
    (address, task)
}

fn client(address: SocketAddr, project: &Project, timeout: Duration) -> GodotClient {
    GodotClient::new(Target::new(address, &project.0, INSTANCE, TOKEN, timeout).unwrap())
}

#[test]
fn checks_only_the_exact_private_values_of_this_client() {
    let project = Project::new();
    let instance = "a".repeat(32);
    let token = "b".repeat(64);
    let client = GodotClient::new(
        Target::new(
            "127.0.0.1:1".parse().unwrap(),
            &project.0,
            &instance,
            &token,
            Duration::from_secs(1),
        )
        .unwrap(),
    );
    assert!(client.contains_private_bridge_values(&format!("prefix:{instance}:suffix")));
    assert!(client.contains_private_bridge_values(&format!("prefix:{token}:suffix")));
    assert!(!client.contains_private_bridge_values(&"c".repeat(64)));
    assert!(!client.contains_private_bridge_values(&token[..63]));
    assert!(!client.contains_private_bridge_values(&instance[..31]));
    assert!(!client.contains_private_bridge_values(""));
}

#[test]
fn project_query_sends_explicit_target_and_returns_editor_data() {
    let project = Project::new();
    let expected_path = paths::godot_wire_path(&project.0);
    let (address, task) = peer(move |request| {
        assert_eq!(request["version"], 1);
        assert_eq!(request["command"], "project.info");
        assert_eq!(request["token"], TOKEN);
        assert_eq!(request["target"]["instance_id"], INSTANCE);
        assert_eq!(request["target"]["project_path"], expected_path);
        let id = request["request_id"].as_str().unwrap();
        assert!(!id.is_empty());
        json!({"version": 1, "request_id": id,
            "identity": {"instance_id": INSTANCE, "project_path": expected_path},
            "result": {"name": "Actual project", "godot_version": "4.7.2"}})
    });
    let info = client(address, &project, Duration::from_secs(1))
        .project_info()
        .unwrap();
    assert_eq!(info.name, "Actual project");
    assert_eq!(info.godot_version, "4.7.2");
    task.join().unwrap();
}

#[test]
fn scene_query_returns_the_editors_current_tree_and_truncation_flag() {
    let project = Project::new();
    let path = paths::godot_wire_path(&project.0);
    let (address, task) = peer(move |request| {
        assert_eq!(request["command"], "scene.tree");
        json!({"version": 1, "request_id": request["request_id"],
            "identity": {"instance_id": INSTANCE, "project_path": path},
            "result": {"scene_path": "res://main.tscn", "truncated": false,
                "nodes": [{"path": ".", "name": "World", "class": "Node2D"},
                          {"path": "Player", "name": "Player", "class": "Sprite2D"}]}})
    });
    let scene = client(address, &project, Duration::from_secs(1))
        .scene_tree()
        .unwrap();
    assert_eq!(scene.scene_path.as_deref(), Some("res://main.tscn"));
    assert!(!scene.truncated);
    assert_eq!(scene.nodes.len(), 2);
    assert_eq!(
        (
            &*scene.nodes[1].path,
            &*scene.nodes[1].name,
            &*scene.nodes[1].class
        ),
        ("Player", "Player", "Sprite2D")
    );
    task.join().unwrap();
}

#[test]
fn target_rejects_non_loopback_invalid_credentials_and_invalid_project() {
    let project = Project::new();
    let timeout = Duration::from_secs(1);
    for address in [
        "0.0.0.0:1234",
        "192.0.2.10:1234",
        "127.0.0.2:1234",
        "[::1]:1234",
        "127.0.0.1:0",
    ] {
        assert!(
            matches!(
                Target::new(
                    address.parse().unwrap(),
                    &project.0,
                    INSTANCE,
                    TOKEN,
                    timeout
                ),
                Err(uebox_engine_godot::Error::InvalidTarget(_))
            ),
            "{address}"
        );
    }
    for (id, token) in [
        ("short", TOKEN),
        (INSTANCE, "short"),
        ("GGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGG", TOKEN),
    ] {
        assert!(matches!(
            Target::new(
                "127.0.0.1:1234".parse().unwrap(),
                &project.0,
                id,
                token,
                timeout
            ),
            Err(uebox_engine_godot::Error::InvalidTarget(_))
        ));
    }
    assert!(matches!(
        Target::new(
            "127.0.0.1:1234".parse().unwrap(),
            &project.0,
            INSTANCE,
            TOKEN,
            Duration::ZERO
        ),
        Err(uebox_engine_godot::Error::InvalidTarget(_))
    ));
    assert!(matches!(
        Target::new(
            "127.0.0.1:1234".parse().unwrap(),
            project.0.join("missing"),
            INSTANCE,
            TOKEN,
            timeout
        ),
        Err(uebox_engine_godot::Error::InvalidTarget(_))
    ));
}

#[cfg(unix)]
#[test]
fn rejects_a_real_project_directory_with_literal_backslash_in_its_name() {
    let project = Project::new();
    let literal = project.0.join(r"A\B");
    let nested = project.0.join("A").join("B");
    fs::create_dir_all(&literal).unwrap();
    fs::create_dir_all(&nested).unwrap();
    fs::write(literal.join("project.godot"), "config_version=5\n").unwrap();
    fs::write(nested.join("project.godot"), "config_version=5\n").unwrap();
    assert_ne!(
        fs::canonicalize(&literal).unwrap(),
        fs::canonicalize(&nested).unwrap()
    );
    let result = Target::new(
        "127.0.0.1:1234".parse().unwrap(),
        &literal,
        INSTANCE,
        TOKEN,
        Duration::from_secs(1),
    );
    assert!(
        matches!(result, Err(uebox_engine_godot::Error::InvalidTarget(_))),
        "ambiguous project must be rejected"
    );
}

#[cfg(unix)]
#[test]
fn rejects_normal_project_when_a_distinct_backslash_path_normalizes_to_it() {
    let project = Project::new();
    let literal = project.0.join(r"A\B");
    let nested = project.0.join("A").join("B");
    fs::create_dir_all(&literal).unwrap();
    fs::create_dir_all(&nested).unwrap();
    fs::write(literal.join("project.godot"), "config_version=5\n").unwrap();
    fs::write(nested.join("project.godot"), "config_version=5\n").unwrap();
    let address = "127.0.0.1:1234".parse().unwrap();
    let result = Target::new(address, &nested, INSTANCE, TOKEN, Duration::from_secs(1));
    assert!(
        matches!(result, Err(uebox_engine_godot::Error::InvalidTarget(_))),
        "normalized alias must be rejected"
    );
    fs::remove_dir_all(&literal).unwrap();
    assert!(Target::new(address, &nested, INSTANCE, TOKEN, Duration::from_secs(1)).is_ok());
}

#[cfg(unix)]
#[test]
fn rejects_a_target_if_an_ambiguous_alias_appears_after_selection() {
    let project = Project::new();
    let nested = project.0.join("A").join("B");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("project.godot"), "config_version=5\n").unwrap();
    let target = Target::new(
        "127.0.0.1:1234".parse().unwrap(),
        &nested,
        INSTANCE,
        TOKEN,
        Duration::from_secs(1),
    )
    .unwrap();
    fs::create_dir(project.0.join(r"A\B")).unwrap();
    let result = GodotClient::new(target).project_info();
    assert!(
        matches!(result, Err(uebox_engine_godot::Error::InvalidTarget(_))),
        "{result:?}"
    );
}

#[cfg(unix)]
#[test]
fn rejects_a_response_if_an_ambiguous_alias_appears_during_the_request() {
    let project = Project::new();
    let nested = project.0.join("A").join("B");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("project.godot"), "config_version=5\n").unwrap();
    let alias = project.0.join(r"A\B");
    let path = paths::godot_wire_path(&nested);
    let (address, task) = peer(move |request| {
        fs::create_dir(alias).unwrap();
        json!({"version": 1, "request_id": request["request_id"],
            "identity": {"instance_id": INSTANCE, "project_path": path},
            "result": {"name": "Actual project", "godot_version": "4.7.2"}})
    });
    let target = Target::new(address, &nested, INSTANCE, TOKEN, Duration::from_secs(1)).unwrap();
    let result = GodotClient::new(target).project_info();
    task.join().unwrap();
    assert!(
        matches!(result, Err(uebox_engine_godot::Error::InvalidTarget(_))),
        "{result:?}"
    );
}

#[cfg(unix)]
#[test]
fn rejects_repeated_and_dot_segment_backslash_aliases() {
    let project = Project::new();
    let nested = project.0.join("A").join("B");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("project.godot"), "config_version=5\n").unwrap();
    for name in [r"A\\B", r"\A\B", r"A\.\B", r"A\C\..\B"] {
        let literal = project.0.join(name);
        fs::create_dir(&literal).unwrap();
        let result = Target::new(
            "127.0.0.1:1234".parse().unwrap(),
            &nested,
            INSTANCE,
            TOKEN,
            Duration::from_secs(1),
        );
        assert!(
            matches!(result, Err(uebox_engine_godot::Error::InvalidTarget(_))),
            "alias {name}"
        );
        fs::remove_dir(&literal).unwrap();
    }
}

#[test]
fn generated_credentials_are_fresh_and_well_formed() {
    let a = uebox_engine_godot::Credentials::generate().unwrap();
    let b = uebox_engine_godot::Credentials::generate().unwrap();
    assert_eq!(a.instance_id.len(), 32);
    assert_eq!(a.token.len(), 64);
    assert!(a
        .instance_id
        .bytes()
        .chain(a.token.bytes())
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    assert_ne!(a.instance_id, b.instance_id);
    assert_ne!(a.token, b.token);
}

#[test]
fn rejects_an_answer_for_a_different_project_or_instance_or_request() {
    let project = Project::new();
    for mismatch in ["project", "instance", "request", "version"] {
        let expected = paths::godot_wire_path(&project.0);
        let (address, task) = peer(move |request| {
            let path = if mismatch == "project" {
                "/a/different/project"
            } else {
                &expected
            };
            let id = if mismatch == "instance" {
                "ffffffffffffffffffffffffffffffff"
            } else {
                INSTANCE
            };
            let request_id = if mismatch == "request" {
                json!("wrong")
            } else {
                request["request_id"].clone()
            };
            let version = if mismatch == "version" { 2 } else { 1 };
            json!({"version": version, "request_id": request_id,
                "identity": {"instance_id": id, "project_path": path},
                "result": {"name": "wrong project", "godot_version": "4.7.2"}})
        });
        assert!(
            matches!(
                client(address, &project, Duration::from_secs(1)).project_info(),
                Err(uebox_engine_godot::Error::Protocol(_))
            ),
            "{mismatch}"
        );
        task.join().unwrap();
    }
}

#[test]
fn remote_failures_are_reported_without_becoming_success() {
    let project = Project::new();
    for code in ["unauthorized", "wrong_target", "unknown_command"] {
        let (address, task) = peer(
            move |request| json!({"version": 1, "request_id": request["request_id"], "error": {"code": code, "message": "rejected"}}),
        );
        match client(address, &project, Duration::from_secs(1)).project_info() {
            Err(uebox_engine_godot::Error::Remote { code: actual, .. }) => assert_eq!(actual, code),
            other => panic!("expected remote {code}, got {other:?}"),
        }
        task.join().unwrap();
    }
}

#[test]
fn invalid_success_payload_is_not_reported_as_project_data() {
    let project = Project::new();
    let path = paths::godot_wire_path(&project.0);
    let (address, task) = peer(move |request| {
        json!({"version": 1, "request_id": request["request_id"],
        "identity": {"instance_id": INSTANCE, "project_path": path}, "result": {"name": 123}})
    });
    assert!(matches!(
        client(address, &project, Duration::from_secs(1)).project_info(),
        Err(uebox_engine_godot::Error::Protocol(_))
    ));
    task.join().unwrap();
}

#[test]
fn scene_result_without_scene_path_is_not_mistaken_for_no_open_scene() {
    let project = Project::new();
    let path = paths::godot_wire_path(&project.0);
    let (address, task) = peer(move |request| {
        json!({"version": 1, "request_id": request["request_id"],
        "identity": {"instance_id": INSTANCE, "project_path": path}, "result": {"nodes": [], "truncated": false}})
    });
    let result = client(address, &project, Duration::from_secs(1)).scene_tree();
    assert!(
        matches!(result, Err(uebox_engine_godot::Error::Protocol(_))),
        "{result:?}"
    );
    task.join().unwrap();
}

#[test]
fn closed_connection_without_a_complete_frame_is_disconnected() {
    let project = Project::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let task = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        stream.write_all(b"{\"version\":1").unwrap();
    });
    let result = client(address, &project, Duration::from_secs(1)).project_info();
    assert!(
        matches!(result, Err(uebox_engine_godot::Error::Disconnected)),
        "{result:?}"
    );
    task.join().unwrap();
}

#[test]
fn unresponsive_editor_times_out_instead_of_hanging() {
    let project = Project::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let task = thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_millis(300));
    });
    let start = Instant::now();
    assert!(matches!(
        client(address, &project, Duration::from_millis(80)).project_info(),
        Err(uebox_engine_godot::Error::Timeout)
    ));
    assert!(start.elapsed() < Duration::from_secs(1));
    task.join().unwrap();
}

#[test]
fn oversized_response_is_rejected() {
    let project = Project::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let task = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        let _ = stream.write_all(&vec![b'a'; 1_100_000]);
    });
    let result = client(address, &project, Duration::from_secs(1)).project_info();
    assert!(
        matches!(result, Err(uebox_engine_godot::Error::Protocol(_))),
        "{result:?}"
    );
    task.join().unwrap();
}

#[test]
fn immediate_peer_disconnect_is_reported_as_disconnected() {
    let project = Project::new();
    for _ in 0..100 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let task = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.write_all(b"partial");
        });
        let result = client(address, &project, Duration::from_secs(1)).project_info();
        assert!(
            matches!(result, Err(uebox_engine_godot::Error::Disconnected)),
            "{result:?}"
        );
        task.join().unwrap();
    }
}

#[test]
fn a_slow_drip_cannot_extend_the_absolute_timeout() {
    let project = Project::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let task = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        for _ in 0..20 {
            if stream.write_all(b"a").is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(30));
        }
    });
    let start = Instant::now();
    let result = client(address, &project, Duration::from_millis(110)).project_info();
    assert!(
        matches!(result, Err(uebox_engine_godot::Error::Timeout)),
        "{result:?}"
    );
    assert!(start.elapsed() < Duration::from_millis(450));
    task.join().unwrap();
}

#[test]
fn concurrent_calls_keep_responses_on_their_own_connections() {
    use std::sync::Arc;
    let project = Project::new();
    let path = paths::godot_wire_path(&project.0);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let task = thread::spawn(move || {
        let mut handlers = Vec::new();
        for _ in 0..8 {
            let (mut stream, _) = listener.accept().unwrap();
            let path = path.clone();
            handlers.push(thread::spawn(move || {
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                thread::sleep(Duration::from_millis(15));
                let result = match request["command"].as_str().unwrap() {
                    "project.info" => json!({"name": "Correct", "godot_version": "4.7.2"}),
                    "scene.tree" => json!({"scene_path": null, "nodes": [], "truncated": false}),
                    command => panic!("unexpected command {command}"),
                };
                writeln!(
                    stream,
                    "{}",
                    json!({"version": 1, "request_id": request["request_id"],
                    "identity": {"instance_id": INSTANCE, "project_path": path}, "result": result})
                )
                .unwrap();
            }));
        }
        for handler in handlers {
            handler.join().unwrap();
        }
    });
    let client = Arc::new(client(address, &project, Duration::from_secs(2)));
    let calls: Vec<_> = (0..8)
        .map(|index| {
            let client = Arc::clone(&client);
            thread::spawn(move || {
                if index % 2 == 0 {
                    assert_eq!(client.project_info().unwrap().name, "Correct");
                } else {
                    assert_eq!(client.scene_tree().unwrap().nodes.len(), 0);
                }
            })
        })
        .collect();
    for call in calls {
        call.join().unwrap();
    }
    task.join().unwrap();
}
