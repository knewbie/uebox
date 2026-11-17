//! Test-only bridge for the Vue integration test. Not registered by the app.
//! It substitutes a line-oriented test transport for WebView IPC while using
//! the real Tauri command handler and an isolated real WebSocket editor fixture.
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{test::MockRuntime, WebviewWindow};
use tokio_tungstenite::tungstenite::Message;

fn invoke(window: &WebviewWindow<MockRuntime>, command: &str, args: Value) -> Result<Value, Value> {
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
        body: tauri::ipc::InvokeBody::Json(args),
        headers: Default::default(),
        invoke_key: tauri::test::INVOKE_KEY.to_string(),
    };
    tauri::test::get_ipc_response(window, request).map(|body| body.deserialize().unwrap())
}

fn start_editor_fixture(
    port: u16,
    window: &WebviewWindow<MockRuntime>,
    respond: bool,
) -> Result<thread::JoinHandle<()>, String> {
    if port == 0 || port == 6400 || port == 17860 {
        return Err("fixture requires an isolated test port".into());
    }
    let (ready_tx, ready_rx) = mpsc::channel();
    let thread = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
                .await.expect("fixture WebSocket connection");
            let welcome = socket.next().await.unwrap().unwrap();
            let Message::Text(welcome) = welcome else { panic!("expected welcome") };
            let welcome: Value = serde_json::from_str(&welcome).unwrap();
            assert_eq!(welcome["method"], "system.connected");
            let event = json!({"ver":"1.0","type":"evt","method":"project.info","payload":{
                "projectName":"E2E fixture editor","projectPath":"/fixture/E2E.uproject","interactive":true
            }});
            socket.send(Message::Text(event.to_string().into())).await.unwrap();
            ready_tx.send(()).unwrap();
            while let Some(Ok(message)) = socket.next().await {
                match message {
                    Message::Text(text) => {
                        let request: Value = serde_json::from_str(&text).unwrap();
                        if request["type"] != "req" { continue }
                        assert_eq!(request["method"], "project.info");
                        assert_eq!(request["params"], json!({}));
                        if !respond { continue }
                        let response = json!({"ver":"1.0","type":"res","id":request["id"],"code":200,"result":{
                            "projectName":"E2E actual project result","engineVersion":"5.5.fixture"
                        }});
                        if socket.send(Message::Text(response.to_string().into())).await.is_err() { break }
                    }
                    Message::Ping(bytes) => { let _ = socket.send(Message::Pong(bytes)).await; }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        });
    });
    ready_rx
        .recv_timeout(Duration::from_secs(3))
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let instances = invoke(window, "list_unreal_instances", json!({}))
            .map_err(|error| error.to_string())?;
        if instances.as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item["project_name"] == "E2E fixture editor")
        }) {
            return Ok(thread);
        }
        if Instant::now() >= deadline {
            return Err("fixture registration timed out".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn main() {
    let app = uebox_tauri_lib::configure_commands(tauri::test::mock_builder())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "{}", json!({"ready":true})).unwrap();
    stdout.flush().unwrap();
    let mut editor: Option<thread::JoinHandle<()>> = None;
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        let message: Value = serde_json::from_str(&line).unwrap();
        let id = message["id"].as_u64().unwrap();
        let command = message["command"].as_str().unwrap();
        let args = message.get("args").cloned().unwrap_or_else(|| json!({}));
        let result = match command {
            "fixture_connect" => {
                let port = args["port"]
                    .as_u64()
                    .and_then(|port| u16::try_from(port).ok());
                let respond = args["respond"].as_bool().unwrap_or(true);
                match port {
                    Some(port) if editor.is_none() => start_editor_fixture(port, &window, respond)
                        .map(|handle| {
                            editor = Some(handle);
                            json!({})
                        })
                        .map_err(Value::String),
                    _ => Err(Value::String("invalid fixture target".into())),
                }
            }
            "host_status"
            | "start_unreal_listener"
            | "list_unreal_instances"
            | "query_unreal_project"
            | "stop_unreal_listener" => invoke(&window, command, args),
            _ => Err(Value::String("unsupported test command".into())),
        };
        if command == "stop_unreal_listener" && result.is_ok() {
            if let Some(handle) = editor.take() {
                let _ = handle.join();
            }
        }
        let response = match result {
            Ok(value) => json!({"id":id,"ok":true,"value":value}),
            Err(error) => json!({"id":id,"ok":false,"error":error}),
        };
        writeln!(stdout, "{response}").unwrap();
        stdout.flush().unwrap();
    }
}
