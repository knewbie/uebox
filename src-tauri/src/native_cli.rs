//! Optional native console endpoint inside the running Tauri process.
mod ask;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::oneshot;
use uebox_cli::{
    AskReply, Capabilities, Dispatch, HostError, HostFuture, HostSession, OnlineProject, ReadTool,
    ToolReply, CLI_CONTRACT_VERSION,
};

#[cfg(unix)]
use crate::profile_storage::{private_directory, ProfileError};
#[cfg(unix)]
use uebox_cli::unix::{UnixServer, ENDPOINT_DIRECTORY};
#[cfg(windows)]
use uebox_cli::{
    windows::{WindowsConnector, WindowsServer},
    Connector,
};

#[cfg(unix)]
type EndpointAddress = PathBuf;
#[cfg(windows)]
type EndpointAddress = String;

#[derive(Default)]
pub(crate) struct NativeCli {
    service: tauri::async_runtime::Mutex<Option<Running>>,
    ask_gate: tauri::async_runtime::Mutex<()>,
    #[cfg(all(test, unix))]
    test_profile: std::sync::OnceLock<PathBuf>,
}

struct Running {
    endpoint: EndpointAddress,
    allow_ask: bool,
    stop: oneshot::Sender<()>,
    task: tauri::async_runtime::JoinHandle<Result<(), HostError>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Endpoint {
    endpoint: EndpointAddress,
    allow_ask: bool,
}

fn endpoint_error(error: HostError) -> &'static str {
    match error {
        HostError::EndpointOccupied => "native_cli_endpoint_busy",
        HostError::EndpointUnreadable | HostError::Authentication => "native_cli_endpoint_invalid",
        _ => "native_cli_unavailable",
    }
}

#[cfg(unix)]
fn profile_root<R: Runtime>(
    app: &AppHandle<R>,
    _host: &NativeCli,
) -> Result<PathBuf, &'static str> {
    #[cfg(test)]
    if let Some(root) = _host.test_profile.get() {
        return Ok(root.clone());
    }
    app.path()
        .app_local_data_dir()
        .map_err(|_| "native_cli_unavailable")
}

#[tauri::command]
pub(crate) async fn start_native_cli<R: Runtime>(
    app: AppHandle<R>,
    allow_ask: Option<bool>,
) -> Result<Endpoint, &'static str> {
    let allow_ask = allow_ask.unwrap_or(false);
    let host = app.state::<NativeCli>();
    let mut stored = host.service.lock().await;
    if let Some(service) = stored.as_ref() {
        if !service.stop.is_closed() {
            if service.allow_ask != allow_ask {
                return Err("native_cli_restart_required");
            }
            return Ok(Endpoint {
                endpoint: service.endpoint.clone(),
                allow_ask,
            });
        }
        *stored = None;
    }
    #[cfg(unix)]
    let (socket, endpoint) = {
        let root = profile_root(&app, &host)?;
        let directory = tauri::async_runtime::spawn_blocking(move || {
            private_directory(&root, ENDPOINT_DIRECTORY, true)
                .map_err(|error| match error {
                    ProfileError::Invalid => "native_cli_endpoint_invalid",
                    _ => "native_cli_unavailable",
                })?
                .ok_or("native_cli_unavailable")
        })
        .await
        .map_err(|_| "native_cli_unavailable")??;
        (
            UnixServer::bind(&directory).map_err(endpoint_error)?,
            directory,
        )
    };
    #[cfg(windows)]
    let (socket, endpoint) = {
        let name = WindowsConnector
            .default_endpoint()
            .ok_or("native_cli_endpoint_invalid")?;
        (WindowsServer::bind(&name).map_err(endpoint_error)?, name)
    };
    let (stop, shutdown) = oneshot::channel();
    let console = Arc::new(Console {
        app: app.clone(),
        allow_ask,
    });
    let task = tauri::async_runtime::spawn(socket.serve(console, async {
        let _ = shutdown.await;
    }));
    *stored = Some(Running {
        endpoint: endpoint.clone(),
        allow_ask,
        stop,
        task,
    });
    Ok(Endpoint {
        endpoint,
        allow_ask,
    })
}

#[tauri::command]
pub(crate) async fn native_cli_endpoint<R: Runtime>(app: AppHandle<R>) -> Option<Endpoint> {
    let host = app.state::<NativeCli>();
    let service = host.service.lock().await;
    service
        .as_ref()
        .filter(|service| !service.stop.is_closed())
        .map(|service| Endpoint {
            endpoint: service.endpoint.clone(),
            allow_ask: service.allow_ask,
        })
}

#[tauri::command]
pub(crate) async fn stop_native_cli<R: Runtime>(app: AppHandle<R>) -> Result<(), &'static str> {
    let host = app.state::<NativeCli>();
    let mut stored = host.service.lock().await;
    if let Some(service) = stored.take() {
        let _ = service.stop.send(());
        service
            .task
            .await
            .map_err(|_| "native_cli_unavailable")?
            .map_err(endpoint_error)?;
    }
    Ok(())
}

struct Console<R: Runtime> {
    app: AppHandle<R>,
    allow_ask: bool,
}

impl<R: Runtime> HostSession for Console<R> {
    fn capabilities(&self) -> HostFuture<'_, Capabilities> {
        Box::pin(async {
            Ok(Capabilities {
                cli_contract_version: CLI_CONTRACT_VERSION,
                project_targeting: true,
                public_results: true,
            })
        })
    }

    fn projects(&self) -> HostFuture<'_, Vec<OnlineProject>> {
        Box::pin(async {
            let mut projects = Vec::new();
            #[cfg(feature = "unreal")]
            projects.extend(
                self.app
                    .state::<crate::unreal::UnrealListenerState>()
                    .cli_projects()
                    .await,
            );
            #[cfg(feature = "unity")]
            projects.extend(
                self.app
                    .state::<crate::unity::UnityQueries>()
                    .agent
                    .cli_projects(),
            );
            #[cfg(feature = "godot")]
            projects.extend(
                self.app
                    .state::<crate::godot::GodotSessions>()
                    .agent
                    .cli_projects(),
            );
            projects.sort_by(|first, second| {
                first
                    .path
                    .cmp(&second.path)
                    .then_with(|| first.connection_id.cmp(&second.connection_id))
            });
            Ok(projects)
        })
    }

    fn tools(&self) -> HostFuture<'_, Vec<ReadTool>> {
        Box::pin(async {
            Ok(vec![
                #[cfg(feature = "unreal")]
                ReadTool::UnrealProjectInfo,
                #[cfg(feature = "unity")]
                ReadTool::UnityProjectInfo,
                #[cfg(feature = "unity")]
                ReadTool::UnityActiveScene,
                #[cfg(feature = "godot")]
                ReadTool::GodotProjectInfo,
                #[cfg(feature = "godot")]
                ReadTool::GodotSceneTree,
            ])
        })
    }

    fn call<'a>(
        &'a self,
        tool: ReadTool,
        project: &'a OnlineProject,
        dispatch: &'a Dispatch,
    ) -> HostFuture<'a, ToolReply> {
        Box::pin(async move {
            if tool.engine() != project.engine {
                return Err(HostError::ToolUnavailable);
            }
            let structured = match tool {
                #[cfg(feature = "unreal")]
                ReadTool::UnrealProjectInfo => {
                    self.app
                        .state::<crate::unreal::UnrealListenerState>()
                        .cli_query(project, dispatch)
                        .await?
                }
                #[cfg(feature = "unity")]
                ReadTool::UnityProjectInfo | ReadTool::UnityActiveScene => {
                    self.app
                        .state::<crate::unity::UnityQueries>()
                        .agent
                        .cli_query(tool, project, dispatch)
                        .await?
                }
                #[cfg(feature = "godot")]
                ReadTool::GodotProjectInfo | ReadTool::GodotSceneTree => {
                    self.app
                        .state::<crate::godot::GodotSessions>()
                        .agent
                        .cli_query(tool, project, dispatch)
                        .await?
                }
                #[allow(unreachable_patterns)]
                _ => return Err(HostError::ToolUnavailable),
            };
            Ok(ToolReply {
                structured,
                content: Vec::new(),
            })
        })
    }
    fn validate_ask_profile<'a>(&'a self, model_profile_id: &'a str) -> HostFuture<'a, ()> {
        Box::pin(async move {
            if !self.allow_ask {
                return Err(HostError::AskUnavailable);
            }
            crate::settings::validate_cli_model(self.app.clone(), model_profile_id.to_owned())
                .await
                .map_err(|_| HostError::ModelUnavailable)
        })
    }
    fn ask<'a>(
        &'a self,
        model_profile_id: &'a str,
        question: &'a str,
        project: &'a OnlineProject,
        allowed_tools: &'a [ReadTool],
        dispatch: &'a Dispatch,
    ) -> HostFuture<'a, AskReply> {
        Box::pin(ask::answer(
            self,
            model_profile_id,
            question,
            project,
            allowed_tools,
            dispatch,
        ))
    }
}

#[cfg(all(test, feature = "unreal", unix))]
mod tests {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{json, Value};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    use tauri::test::{mock_builder, mock_context, noop_assets};
    use tokio_tungstenite::{connect_async, tungstenite::Message};
    use uebox_cli::{
        run,
        unix::{UnixConnector, SOCKET_NAME},
    };
    use uebox_engine_unreal::UnrealBridge;

    use super::*;

    #[test]
    fn command_line_discovery_uses_the_application_identifier() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(config["identifier"], uebox_cli::unix::APP_IDENTIFIER);
    }

    async fn model_step(listener: &tokio::net::TcpListener, event: Value) -> Value {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut content_length = None;
        loop {
            let mut line = String::new();
            assert!(stream.read_line(&mut line).await.unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                if key.eq_ignore_ascii_case("content-length") {
                    content_length = Some(value.trim().parse::<usize>().unwrap());
                }
            }
        }
        let mut body = vec![0; content_length.unwrap()];
        stream.read_exact(&mut body).await.unwrap();
        let sse = format!("data: {event}\n\ndata: [DONE]\n\n");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}", sse.len()
        );
        stream
            .into_inner()
            .write_all(response.as_bytes())
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn native_console_uses_real_socket_and_fixed_read_only_unreal_target() {
        // /var and /tmp are symlink aliases on macOS. The endpoint requires
        // actual directories for every ancestor.
        let data = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let app = crate::configure_commands(mock_builder())
            .build(mock_context(noop_assets()))
            .unwrap();
        let handle = app.handle().clone();
        app.state::<NativeCli>()
            .test_profile
            .set(data.path().join("p"))
            .unwrap();
        assert!(native_cli_endpoint(handle.clone()).await.is_none());
        assert!(!data.path().join("p").exists());
        let default_start = start_native_cli(handle.clone(), None).await.unwrap();
        assert!(!default_start.allow_ask);
        assert_eq!(
            start_native_cli(handle.clone(), Some(true)).await.err(),
            Some("native_cli_restart_required")
        );
        stop_native_cli(handle.clone()).await.unwrap();
        let started = start_native_cli(handle.clone(), Some(true)).await.unwrap();
        assert!(started.allow_ask);
        let endpoint = started.endpoint;
        assert!(endpoint.join(SOCKET_NAME).exists());
        let bridge = Arc::new(UnrealBridge::bind(0).await.unwrap());
        let address = bridge.local_addr();
        *app.state::<crate::unreal::UnrealListenerState>()
            .bridge
            .lock()
            .await = Some(bridge.clone());
        let (mut peer, _) = connect_async(format!("ws://{address}")).await.unwrap();
        let project = data.path().join("fixture").to_str().unwrap().to_owned();
        peer.send(Message::Text(
            json!({
                "ver":"1.0","type":"evt","method":"project.info",
                "payload":{"projectName":"Fixture UE","projectPath":project,"interactive":true}
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let wait = async {
            while bridge.instances().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(2), wait)
            .await
            .unwrap();
        let response = tokio::spawn(async move {
            let mut count = 0;
            while let Some(message) = peer.next().await {
                match message.unwrap() {
                    Message::Text(text) => {
                        let request: Value = serde_json::from_str(&text).unwrap();
                        if request["type"] != "req" {
                            continue;
                        }
                        assert_eq!(request["method"], "project.info");
                        peer.send(Message::Text(
                            json!({
                                "ver":"1.0","type":"res","id":request["id"],"code":200,
                                "result":{"projectName":"Fixture UE","projectPath":project,
                                          "token":"private-fixture","engineVersion":"5.5"}
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .unwrap();
                        count += 1;
                        if count == 2 {
                            while let Some(message) = peer.next().await {
                                match message {
                                    Ok(Message::Ping(payload)) => {
                                        peer.send(Message::Pong(payload)).await.unwrap()
                                    }
                                    Err(_) => break,
                                    _ => {}
                                }
                            }
                            return;
                        }
                    }
                    Message::Ping(payload) => peer.send(Message::Pong(payload)).await.unwrap(),
                    _ => {}
                }
            }
            panic!("project info was not dispatched");
        });
        let args = vec![
            "--endpoint".to_owned(),
            endpoint.to_string_lossy().into_owned(),
            "tools".to_owned(),
            "call".to_owned(),
            "ue_project_info".to_owned(),
        ];
        let result = run(&args, data.path(), &UnixConnector).await;
        let document: Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(result.exit_code, 0, "{}", result.stdout);
        assert_eq!(document["data"]["structured"]["projectName"], "Fixture UE");
        assert!(!result.stdout.contains("private-fixture"));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let model_address = listener.local_addr().unwrap();
        let profile_id = crate::settings::cli_test_profile(
            handle.clone(),
            data.path().join("models"),
            format!("http://{model_address}/v1/chat/completions"),
        )
        .await;
        let model = tokio::spawn(async move {
            let request = model_step(&listener, json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"id":"fixture-call","type":"function","function":{"name":"ue_project_info","arguments":"{}"}
            }]},"finish_reason":"tool_calls"}]})).await;
            let definitions = request["tools"].as_array().unwrap();
            assert_eq!(definitions.len(), 1);
            assert_eq!(definitions[0]["function"]["name"], "ue_project_info");
            let request = model_step(&listener, json!({"choices":[{"index":0,"delta":{"content":"Fixture model answer"},"finish_reason":"stop"}]})).await;
            let text = request.to_string();
            assert!(text.contains("Fixture UE"));
            assert!(!text.contains("private-fixture"));
        });
        let ask_args = vec![
            "--endpoint".to_owned(),
            endpoint.to_string_lossy().into_owned(),
            "ask".to_owned(),
            "What project is selected?".to_owned(),
            "--model".to_owned(),
            profile_id.clone(),
        ];
        let mut unbound = ask_args.clone();
        *unbound.last_mut().unwrap() = "0".repeat(32);
        let refused = run(&unbound, data.path(), &UnixConnector).await;
        assert_ne!(refused.exit_code, 0, "{}", refused.stdout);
        let error: Value = serde_json::from_str(&refused.stdout).unwrap();
        assert_eq!(error["error"]["code"], "MODEL_UNAVAILABLE");
        let reply = tokio::time::timeout(
            Duration::from_secs(7),
            run(&ask_args, data.path(), &UnixConnector),
        )
        .await
        .unwrap();
        assert_eq!(reply.exit_code, 0, "{}", reply.stdout);
        let document: Value = serde_json::from_str(&reply.stdout).unwrap();
        assert_eq!(document["data"]["text"], "Fixture model answer");
        assert_eq!(document["data"]["allowedTools"], json!(["ue_project_info"]));
        model.await.unwrap();
        stop_native_cli(handle).await.unwrap();
        assert!(!endpoint.join(SOCKET_NAME).exists());
        bridge.shutdown().await;
        response.await.unwrap();
    }
}
