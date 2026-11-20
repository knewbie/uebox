//! Optional native console endpoint. The socket lives in the running Tauri process.
use std::{path::PathBuf, sync::Arc};

use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::oneshot;
use uebox_cli::{
    unix::UnixServer, Capabilities, Dispatch, HostError, HostFuture, HostSession, OnlineProject,
    ReadTool, ToolReply, CLI_CONTRACT_VERSION,
};

use crate::profile_storage::{private_directory, ProfileError};

#[derive(Default)]
pub(crate) struct NativeCli {
    service: tauri::async_runtime::Mutex<Option<Running>>,
    #[cfg(test)]
    test_profile: std::sync::OnceLock<PathBuf>,
}

struct Running {
    endpoint: PathBuf,
    stop: oneshot::Sender<()>,
    task: tauri::async_runtime::JoinHandle<Result<(), HostError>>,
}

#[derive(Serialize)]
pub(crate) struct Endpoint {
    endpoint: PathBuf,
}

fn endpoint_error(error: HostError) -> &'static str {
    match error {
        HostError::EndpointOccupied => "native_cli_endpoint_busy",
        HostError::EndpointUnreadable | HostError::Authentication => "native_cli_endpoint_invalid",
        _ => "native_cli_unavailable",
    }
}

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
) -> Result<Endpoint, &'static str> {
    let host = app.state::<NativeCli>();
    let mut stored = host.service.lock().await;
    if let Some(service) = stored.as_ref() {
        if !service.stop.is_closed() {
            return Ok(Endpoint {
                endpoint: service.endpoint.clone(),
            });
        }
        *stored = None;
    }
    let root = profile_root(&app, &host)?;
    let directory = tauri::async_runtime::spawn_blocking(move || {
        private_directory(&root, "cli", true)
            .map_err(|error| match error {
                ProfileError::Invalid => "native_cli_endpoint_invalid",
                _ => "native_cli_unavailable",
            })?
            .ok_or("native_cli_unavailable")
    })
    .await
    .map_err(|_| "native_cli_unavailable")??;
    let endpoint = directory.clone();
    let socket = UnixServer::bind(&directory).map_err(endpoint_error)?;
    let (stop, shutdown) = oneshot::channel();
    let console = Arc::new(Console { app: app.clone() });
    let task = tauri::async_runtime::spawn(socket.serve(console, async {
        let _ = shutdown.await;
    }));
    *stored = Some(Running {
        endpoint: endpoint.clone(),
        stop,
        task,
    });
    Ok(Endpoint { endpoint })
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
}

#[cfg(all(test, feature = "unreal"))]
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
        let endpoint = start_native_cli(handle.clone()).await.unwrap().endpoint;
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
                        return;
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
        response.await.unwrap();
        stop_native_cli(handle).await.unwrap();
        assert!(!endpoint.join(SOCKET_NAME).exists());
        bridge.shutdown().await;
    }
}
