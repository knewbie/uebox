#![cfg(feature = "unreal")]

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::test::MockRuntime;
use tauri::WebviewWindow;
use tokio_tungstenite::tungstenite::Message;

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

fn temporary_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    assert_ne!(port, 17860);
    assert_ne!(port, 6400);
    port
}

enum EditorCommand {
    Announce { name: String, path: String },
    Reply { id: String, result: Value },
    Close,
}

struct EditorFixture {
    commands: tokio::sync::mpsc::UnboundedSender<EditorCommand>,
    requests: mpsc::Receiver<Value>,
    thread: Option<thread::JoinHandle<()>>,
}

impl EditorFixture {
    fn connect(port: u16) -> Self {
        let (commands, mut incoming) = tokio::sync::mpsc::unbounded_channel();
        let (request_tx, requests) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
                    .await.expect("isolated fixture must connect");
                let welcome = tokio::time::timeout(Duration::from_secs(2), socket.next()).await.unwrap().unwrap().unwrap();
                let Message::Text(welcome) = welcome else { panic!("expected welcome event") };
                let welcome: Value = serde_json::from_str(&welcome).unwrap();
                assert_eq!(welcome["method"], "system.connected");
                ready_tx.send(()).unwrap();
                loop {
                    tokio::select! {
                        command = incoming.recv() => match command {
                            Some(EditorCommand::Announce { name, path }) => {
                                let event = json!({"ver":"1.0","type":"evt","method":"project.info","payload":{"projectName":name,"projectPath":path,"interactive":true}});
                                if socket.send(Message::Text(event.to_string().into())).await.is_err() { break }
                            }
                            Some(EditorCommand::Reply { id, result }) => {
                                let response = json!({"ver":"1.0","type":"res","id":id,"code":200,"result":result});
                                if socket.send(Message::Text(response.to_string().into())).await.is_err() { break }
                            }
                            Some(EditorCommand::Close) | None => { let _ = socket.close(None).await; break }
                        },
                        message = socket.next() => match message {
                            Some(Ok(Message::Text(text))) => {
                                let payload: Value = serde_json::from_str(&text).unwrap();
                                if payload["type"] == "req" { let _ = request_tx.send(payload); }
                            }
                            Some(Ok(Message::Ping(bytes))) => { let _ = socket.send(Message::Pong(bytes)).await; }
                            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                            _ => {}
                        }
                    }
                }
            });
        });
        ready_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("fixture handshake");
        Self {
            commands,
            requests,
            thread: Some(thread),
        }
    }

    fn announce(&self, name: &str, path: &str) {
        self.commands
            .send(EditorCommand::Announce {
                name: name.into(),
                path: path.into(),
            })
            .unwrap();
    }

    fn next_request(&self) -> Value {
        self.requests
            .recv_timeout(Duration::from_secs(3))
            .expect("read-only request")
    }

    fn reply(&self, request: &Value, result: Value) {
        self.commands
            .send(EditorCommand::Reply {
                id: request["id"].as_str().unwrap().to_owned(),
                result,
            })
            .unwrap();
    }
}

impl Drop for EditorFixture {
    fn drop(&mut self) {
        let _ = self.commands.send(EditorCommand::Close);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn await_instances(window: &WebviewWindow<MockRuntime>, expected_name: Option<&str>) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let instances = invoke(window, "list_unreal_instances", json!({})).unwrap();
        let instances = instances.as_array().unwrap();
        if match expected_name {
            Some(name) => instances.iter().any(|item| item["project_name"] == name),
            None => instances.is_empty(),
        } {
            return instances.clone();
        }
        assert!(
            Instant::now() < deadline,
            "instances did not reach expected state"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn instances_are_unavailable_before_explicit_start() {
    let error = invoke(&window(), "list_unreal_instances", json!({}))
        .expect_err("listener must not start implicitly");
    assert!(
        error.to_string().to_lowercase().contains("not running"),
        "{error}"
    );
}

#[test]
fn starts_only_the_selected_loopback_port_and_stop_releases_it() {
    let port = temporary_port();
    let window = window();
    let started = invoke(&window, "start_unreal_listener", json!({"port": port})).unwrap();
    assert_eq!(started["port"], port);
    assert_eq!(started["address"], "127.0.0.1");
    assert!(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err());
    assert_eq!(
        invoke(&window, "list_unreal_instances", json!({})).unwrap(),
        json!([])
    );
    invoke(&window, "stop_unreal_listener", json!({})).unwrap();
    let rebound =
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).expect("stop must release the port");
    drop(rebound);
}

#[test]
fn occupied_port_reports_real_bind_error_and_does_not_leave_a_listener() {
    let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = occupied.local_addr().unwrap().port();
    assert_ne!(port, 17860);
    assert_ne!(port, 6400);
    let window = window();
    let expected = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .unwrap_err()
        .to_string();
    let error = invoke(&window, "start_unreal_listener", json!({"port": port}))
        .expect_err("occupied port must fail");
    assert!(error.to_string().contains(&expected), "{error}");
    drop(occupied);
    invoke(&window, "start_unreal_listener", json!({"port": port})).unwrap();
    invoke(&window, "stop_unreal_listener", json!({})).unwrap();
}

#[test]
fn rejects_zero_port_and_never_selects_an_instance_implicitly() {
    let window = window();
    assert!(invoke(&window, "start_unreal_listener", json!({"port":0})).is_err());
    let port = temporary_port();
    invoke(&window, "start_unreal_listener", json!({"port":port})).unwrap();
    let editor = EditorFixture::connect(port);
    editor.announce("Fixture A", "/fixture/A.uproject");
    let instances = await_instances(&window, Some("Fixture A"));
    assert_eq!(instances.len(), 1);
    let error = invoke(&window, "query_unreal_project", json!({"connectionId":""}))
        .expect_err("empty selection must fail");
    assert!(error.to_string().contains("not connected"), "{error}");
    assert!(editor
        .requests
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    invoke(&window, "stop_unreal_listener", json!({})).unwrap();
}

#[test]
fn selected_project_query_uses_the_real_websocket_protocol_and_engine_response() {
    let window = window();
    let port = temporary_port();
    invoke(&window, "start_unreal_listener", json!({"port":port})).unwrap();
    let editor = EditorFixture::connect(port);
    editor.announce("Fixture A", "/fixture/A.uproject");
    let instances = await_instances(&window, Some("Fixture A"));
    let id = instances[0]["connection_id"].as_str().unwrap().to_owned();
    let query_window = window.clone();
    let query = thread::spawn(move || {
        invoke(
            &query_window,
            "query_unreal_project",
            json!({"connectionId":id}),
        )
    });
    let request = editor.next_request();
    assert_eq!(request["method"], "project.info");
    assert_eq!(request["params"], json!({}));
    editor.reply(
        &request,
        json!({"projectName":"Actual engine result","engineVersion":"5.5.fixture"}),
    );
    let result = query.join().unwrap().unwrap();
    assert_eq!(result["projectName"], "Actual engine result");
    assert_eq!(result["engineVersion"], "5.5.fixture");
    invoke(&window, "stop_unreal_listener", json!({})).unwrap();
}

#[test]
fn query_is_sent_only_to_the_explicitly_selected_instance() {
    let window = window();
    let port = temporary_port();
    invoke(&window, "start_unreal_listener", json!({"port":port})).unwrap();
    let first = EditorFixture::connect(port);
    let second = EditorFixture::connect(port);
    first.announce("First", "/fixture/First.uproject");
    second.announce("Second", "/fixture/Second.uproject");
    let instances = await_instances(&window, Some("Second"));
    let selected_id = instances
        .iter()
        .find(|item| item["project_name"] == "Second")
        .unwrap()["connection_id"]
        .as_str()
        .unwrap()
        .to_owned();
    await_instances(&window, Some("First"));
    let query_window = window.clone();
    let query = thread::spawn(move || {
        invoke(
            &query_window,
            "query_unreal_project",
            json!({"connectionId":selected_id}),
        )
    });
    let request = second.next_request();
    assert_eq!(request["method"], "project.info");
    assert!(first
        .requests
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    second.reply(&request, json!({"projectName":"Second response"}));
    assert_eq!(
        query.join().unwrap().unwrap()["projectName"],
        "Second response"
    );
    invoke(&window, "stop_unreal_listener", json!({})).unwrap();
}

#[test]
fn project_change_rotates_selection_and_rejects_old_id_without_a_request() {
    let window = window();
    let port = temporary_port();
    invoke(&window, "start_unreal_listener", json!({"port":port})).unwrap();
    let editor = EditorFixture::connect(port);
    editor.announce("First", "/fixture/First.uproject");
    let first = await_instances(&window, Some("First"));
    let old_id = first[0]["connection_id"].as_str().unwrap().to_owned();
    editor.announce("Second", "/fixture/Second.uproject");
    let second = await_instances(&window, Some("Second"));
    let new_id = second[0]["connection_id"].as_str().unwrap().to_owned();
    assert_ne!(old_id, new_id);
    let error = invoke(
        &window,
        "query_unreal_project",
        json!({"connectionId":old_id}),
    )
    .expect_err("old selection must be stale");
    assert!(error.to_string().contains("not connected"), "{error}");
    assert!(editor
        .requests
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    let query_window = window.clone();
    let query = thread::spawn(move || {
        invoke(
            &query_window,
            "query_unreal_project",
            json!({"connectionId":new_id}),
        )
    });
    let request = editor.next_request();
    editor.reply(&request, json!({"projectName":"Second actual result"}));
    assert_eq!(
        query.join().unwrap().unwrap()["projectName"],
        "Second actual result"
    );
    invoke(&window, "stop_unreal_listener", json!({})).unwrap();
}

#[test]
fn disconnection_invalidates_the_selected_id() {
    let window = window();
    let port = temporary_port();
    invoke(&window, "start_unreal_listener", json!({"port":port})).unwrap();
    let editor = EditorFixture::connect(port);
    editor.announce("Closing", "/fixture/Closing.uproject");
    let instances = await_instances(&window, Some("Closing"));
    let id = instances[0]["connection_id"].as_str().unwrap().to_owned();
    editor.commands.send(EditorCommand::Close).unwrap();
    await_instances(&window, None);
    let error = invoke(&window, "query_unreal_project", json!({"connectionId":id}))
        .expect_err("disconnected id must fail");
    assert!(error.to_string().contains("not connected"), "{error}");
    invoke(&window, "stop_unreal_listener", json!({})).unwrap();
}

#[test]
fn project_change_fails_an_inflight_query_without_retargeting() {
    let window = window();
    let port = temporary_port();
    invoke(&window, "start_unreal_listener", json!({"port":port})).unwrap();
    let editor = EditorFixture::connect(port);
    editor.announce("First", "/fixture/First.uproject");
    let id = await_instances(&window, Some("First"))[0]["connection_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let query_window = window.clone();
    let query = thread::spawn(move || {
        invoke(
            &query_window,
            "query_unreal_project",
            json!({"connectionId":id}),
        )
    });
    let request = editor.next_request();
    editor.announce("Second", "/fixture/Second.uproject");
    let error = query
        .join()
        .unwrap()
        .expect_err("identity change must fail the pending query");
    assert!(error.to_string().contains("project changed"), "{error}");
    editor.reply(&request, json!({"projectName":"late old result"}));
    let changed = await_instances(&window, Some("Second"));
    assert_eq!(changed.len(), 1);
    assert!(editor
        .requests
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    invoke(&window, "stop_unreal_listener", json!({})).unwrap();
}

#[test]
fn listener_restart_does_not_accept_the_old_selection() {
    let window = window();
    let port = temporary_port();
    invoke(&window, "start_unreal_listener", json!({"port":port})).unwrap();
    let first = EditorFixture::connect(port);
    first.announce("Same project", "/fixture/Same.uproject");
    let old_id = await_instances(&window, Some("Same project"))[0]["connection_id"]
        .as_str()
        .unwrap()
        .to_owned();
    invoke(&window, "stop_unreal_listener", json!({})).unwrap();
    drop(first);
    invoke(&window, "start_unreal_listener", json!({"port":port})).unwrap();
    let second = EditorFixture::connect(port);
    second.announce("Same project", "/fixture/Same.uproject");
    let new_id = await_instances(&window, Some("Same project"))[0]["connection_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(old_id, new_id);
    let error = invoke(
        &window,
        "query_unreal_project",
        json!({"connectionId":old_id}),
    )
    .expect_err("old id cannot cross restart");
    assert!(error.to_string().contains("not connected"), "{error}");
    assert!(second
        .requests
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    invoke(&window, "stop_unreal_listener", json!({})).unwrap();
}

#[test]
fn stop_cancels_a_pending_query_and_releases_the_port() {
    let window = window();
    let port = temporary_port();
    invoke(&window, "start_unreal_listener", json!({"port":port})).unwrap();
    let editor = EditorFixture::connect(port);
    editor.announce("Pending", "/fixture/Pending.uproject");
    let id = await_instances(&window, Some("Pending"))[0]["connection_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let query_window = window.clone();
    let query = thread::spawn(move || {
        invoke(
            &query_window,
            "query_unreal_project",
            json!({"connectionId":id}),
        )
    });
    assert_eq!(editor.next_request()["method"], "project.info");
    invoke(&window, "stop_unreal_listener", json!({})).unwrap();
    let error = query
        .join()
        .unwrap()
        .expect_err("stop must cancel pending query");
    assert!(error.to_string().contains("disconnected"), "{error}");
    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).expect("listener must be closed");
}

#[test]
fn timeout_is_reported_without_retrying_the_query() {
    let window = window();
    let port = temporary_port();
    invoke(&window, "start_unreal_listener", json!({"port":port})).unwrap();
    let editor = EditorFixture::connect(port);
    editor.announce("Slow", "/fixture/Slow.uproject");
    let id = await_instances(&window, Some("Slow"))[0]["connection_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let query_window = window.clone();
    let (result_tx, result_rx) = mpsc::channel();
    let query = thread::spawn(move || {
        let _ = result_tx.send(invoke(
            &query_window,
            "query_unreal_project",
            json!({"connectionId":id}),
        ));
    });
    assert_eq!(editor.next_request()["method"], "project.info");
    let error = result_rx
        .recv_timeout(Duration::from_secs(13))
        .expect("query must time out")
        .expect_err("timeout must fail");
    assert!(error.to_string().contains("timed out"), "{error}");
    assert!(editor
        .requests
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    query.join().unwrap();
    invoke(&window, "stop_unreal_listener", json!({})).unwrap();
}

const REQUEST_UE_PROJECT_INFO: &str = concat!(
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"ue-info\",\"type\":\"function\",\"function\":{\"name\":\"ue_project_info\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: [DONE]\n\n"
);

fn model_step(listener: &TcpListener, sse: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "model request never arrived");
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("model accept failed: {error}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut reader = BufReader::new(&mut stream);
    let mut length = None;
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
    }
    let mut bytes = vec![0; length.unwrap()];
    reader.read_exact(&mut bytes).unwrap();
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}", sse.len()).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn create_unreal_agent_fixture(
    window: &WebviewWindow<MockRuntime>,
    connection_id: &Value,
    root: &str,
    endpoint: &str,
) -> Result<Value, Value> {
    invoke(
        window,
        "create_unreal_agent",
        json!({
            "connectionId":connection_id, "projectRoot":root,
            "endpoint":endpoint, "model":"fixture-model", "credential":"fixture-secret"
        }),
    )
}

fn await_unreal_agent_phase(
    window: &WebviewWindow<MockRuntime>,
    run: &Value,
    expected: &str,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let update = invoke(window, "poll_unreal_agent", run.clone()).unwrap();
        if update["phase"] == expected {
            return update;
        }
        assert!(
            Instant::now() < deadline
                && !matches!(
                    update["phase"].as_str(),
                    Some("failed" | "completed" | "cancelled")
                ),
            "expected {expected}: {update}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn unreal_agent_refuses_a_relative_project_root_without_echoing_input() {
    let error = create_unreal_agent_fixture(
        &window(),
        &json!("no-connection"),
        "relative-fixture-path",
        "http://127.0.0.1:1/never-called",
    )
    .unwrap_err();
    assert_eq!(error, "target_unavailable");
}

#[test]
fn unreal_agent_filters_model_output_and_holds_cancelled_instance_until_read_drains() {
    let window = window();
    let port = temporary_port();
    invoke(&window, "start_unreal_listener", json!({"port":port})).unwrap();
    let editor = EditorFixture::connect(port);
    let other_editor = EditorFixture::connect(port);
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let root = root.to_str().unwrap();
    editor.announce("AgentFixture", root);
    other_editor.announce("OtherFixture", root);
    let connection = await_instances(&window, Some("AgentFixture"))
        .into_iter()
        .find(|item| item["project_name"] == "AgentFixture")
        .unwrap()["connection_id"]
        .clone();
    let model = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    model.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/completions", model.local_addr().unwrap());
    let agent = create_unreal_agent_fixture(&window, &connection, root, &endpoint).unwrap();
    assert_eq!(
        create_unreal_agent_fixture(&window, &connection, root, &endpoint).unwrap_err(),
        "agent_already_exists"
    );
    let agent_id = &agent["agentId"];
    let run = invoke(
        &window,
        "start_unreal_agent",
        json!({"agentId":agent_id,"text":"version?"}),
    )
    .unwrap();
    assert_eq!(
        model_step(&model, REQUEST_UE_PROJECT_INFO)["tools"][0]["function"]["name"],
        "ue_project_info"
    );
    let request = editor.next_request();
    assert_eq!(request["method"], "project.info");
    assert_eq!(request["params"], json!({}));
    editor.reply(
        &request,
        json!({
            "projectName":"AgentFixture", "projectPath":root, "engineVersion":"5.4.3",
            "engineDir":"/private-fixture-engine", "hasCode":true
        }),
    );
    let body = model_step(&model, concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"UE 5.4.3\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    ));
    let tool = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap();
    let result: Value = serde_json::from_str(tool["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        result["data"],
        json!({"projectName":"AgentFixture","engineVersion":"5.4.3","hasCode":true})
    );
    assert_ne!(result["target"]["project"], root);
    assert_ne!(result["target"]["instance"], connection);
    assert!(!tool["content"]
        .as_str()
        .unwrap()
        .contains("private-fixture-engine"));
    await_unreal_agent_phase(&window, &run, "completed");

    let run = invoke(
        &window,
        "start_unreal_agent",
        json!({"agentId":agent_id,"text":"version again?"}),
    )
    .unwrap();
    model_step(&model, REQUEST_UE_PROJECT_INFO);
    let request = editor.next_request();
    invoke(&window, "cancel_unreal_agent", run.clone()).unwrap();
    await_unreal_agent_phase(&window, &run, "draining");
    assert_eq!(
        invoke(
            &window,
            "start_unreal_agent",
            json!({"agentId":agent_id,"text":"overlap"})
        )
        .unwrap_err(),
        "agent_busy"
    );
    editor.reply(
        &request,
        json!({"projectName":"AgentFixture", "projectPath":root, "engineVersion":"late"}),
    );
    let terminal = await_unreal_agent_phase(&window, &run, "cancelled");
    assert_eq!(terminal["fragments"], json!([]));
    assert!(matches!(model.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    assert!(other_editor.requests.try_recv().is_err());

    let run = invoke(
        &window,
        "start_unreal_agent",
        json!({"agentId":agent_id,"text":"check before rotation"}),
    )
    .unwrap();
    model_step(&model, REQUEST_UE_PROJECT_INFO);
    let request = editor.next_request();
    editor.announce("RotatedFixture", root);
    let rotated = await_instances(&window, Some("RotatedFixture"))
        .into_iter()
        .find(|item| item["project_name"] == "RotatedFixture")
        .unwrap()["connection_id"]
        .clone();
    assert_ne!(rotated, connection);
    await_unreal_agent_phase(&window, &run, "draining");
    assert_eq!(
        create_unreal_agent_fixture(&window, &rotated, root, &endpoint).unwrap_err(),
        "agent_already_exists"
    );
    assert!(invoke(
        &window,
        "query_unreal_project",
        json!({"connectionId":rotated})
    )
    .unwrap_err()
    .as_str()
    .unwrap()
    .contains("still draining"));
    assert_eq!(
        create_unreal_agent_fixture(&window, &connection, root, &endpoint).unwrap_err(),
        "target_unavailable"
    );
    editor.reply(
        &request,
        json!({"projectName":"AgentFixture","projectPath":root}),
    );
    assert_eq!(
        await_unreal_agent_phase(&window, &run, "failed")["error"],
        "target_unavailable"
    );
    let fresh = create_unreal_agent_fixture(&window, &rotated, root, &endpoint).unwrap();
    drop(editor);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let instances = await_instances(&window, Some("OtherFixture"));
        if instances
            .iter()
            .all(|instance| instance["connection_id"] != rotated)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "rotated socket did not disconnect"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let replacement = EditorFixture::connect(port);
    replacement.announce("ReconnectedFixture", root);
    let reconnected = await_instances(&window, Some("ReconnectedFixture"))
        .into_iter()
        .find(|item| item["project_name"] == "ReconnectedFixture")
        .unwrap()["connection_id"]
        .clone();
    assert_ne!(reconnected, rotated);
    assert_eq!(
        create_unreal_agent_fixture(&window, &rotated, root, &endpoint).unwrap_err(),
        "target_unavailable"
    );
    let replacement_agent =
        create_unreal_agent_fixture(&window, &reconnected, root, &endpoint).unwrap();
    assert_ne!(replacement_agent["agentId"], fresh["agentId"]);
    invoke(&window, "stop_unreal_listener", json!({})).unwrap();
    assert_eq!(
        create_unreal_agent_fixture(&window, &reconnected, root, &endpoint).unwrap_err(),
        "target_unavailable"
    );
}
