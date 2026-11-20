#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use serde_json::{json, Value};
use tokio::{
    io::AsyncWriteExt,
    net::UnixStream,
    sync::{oneshot, Notify},
};
use uebox_cli::{
    run,
    unix::{UnixConnector, UnixServer, SOCKET_NAME},
    Capabilities, Connector, Dispatch, Engine, HostError, HostFuture, HostSession, OnlineProject,
    ReadTool, ToolReply,
};

#[derive(Default)]
struct Fixture {
    calls: AtomicUsize,
    preparing: Notify,
    running: Notify,
    blocking_prepare: bool,
    blocking_call: bool,
    cancelled: AtomicUsize,
    tool_lists: AtomicUsize,
}

impl HostSession for Fixture {
    fn capabilities(&self) -> HostFuture<'_, Capabilities> {
        Box::pin(async {
            Ok(Capabilities {
                cli_contract_version: 1,
                project_targeting: true,
                public_results: true,
            })
        })
    }

    fn projects(&self) -> HostFuture<'_, Vec<OnlineProject>> {
        Box::pin(async {
            Ok(vec![OnlineProject {
                engine: Engine::Unreal,
                name: "Fixture".into(),
                path: PathBuf::from("/fixture/online"),
                connection_id: "generation-1".into(),
            }])
        })
    }

    fn tools(&self) -> HostFuture<'_, Vec<ReadTool>> {
        Box::pin(async {
            if self.tool_lists.fetch_add(1, Ordering::SeqCst) == 1 && self.blocking_prepare {
                self.preparing.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(vec![ReadTool::UnrealProjectInfo])
        })
    }

    fn call<'a>(
        &'a self,
        tool: ReadTool,
        project: &'a OnlineProject,
        dispatch: &'a Dispatch,
    ) -> HostFuture<'a, ToolReply> {
        Box::pin(async move {
            assert_eq!(tool, ReadTool::UnrealProjectInfo);
            assert_eq!(project.connection_id, "generation-1");
            assert_eq!(project.path, Path::new("/fixture/online"));
            dispatch.mark_dispatched();
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.blocking_call {
                let _drop = OnDrop(&self.cancelled);
                self.running.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(ToolReply {
                structured: json!({"fixture": true}),
                content: vec![],
            })
        })
    }
}

struct OnDrop<'a>(&'a AtomicUsize);
impl Drop for OnDrop<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn runtime_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap()
}

async fn start(
    path: &Path,
    fixture: Arc<Fixture>,
) -> (
    oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), HostError>>,
) {
    let server = UnixServer::bind(path).unwrap();
    let (send, receive) = oneshot::channel();
    let handle = tokio::spawn(server.serve(fixture, async {
        let _ = receive.await;
    }));
    (send, handle)
}

fn cli(path: &Path, command: &[&str]) -> Vec<String> {
    let mut args = vec!["--endpoint".into(), path.to_str().unwrap().into()];
    args.extend(command.iter().map(|name| name.to_string()));
    args
}

#[tokio::test]
async fn binary_connects_to_temporary_host_and_concurrent_clients_keep_independent_sessions() {
    let directory = runtime_dir();
    let fixture = Arc::new(Fixture::default());
    let (stop, server) = start(directory.path(), fixture.clone()).await;
    assert_eq!(
        fs::symlink_metadata(directory.path().join(SOCKET_NAME))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let args = cli(directory.path(), &["projects", "list", "--json"]);
    let child = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_uebox"))
            .env_clear()
            .args(args)
            .output()
            .unwrap()
    });
    let reads = cli(directory.path(), &["tools", "call", "ue_project_info"]);
    let (first, second, child) = tokio::join!(
        run(&reads, Path::new(""), &UnixConnector),
        run(&reads, Path::new(""), &UnixConnector),
        child
    );
    for output in [first, second] {
        assert_eq!(output.exit_code, 0, "{}", output.stdout);
        assert_eq!(
            serde_json::from_str::<Value>(&output.stdout).unwrap()["data"]["structured"]["fixture"],
            true
        );
    }
    let child = child.unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stdout)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&child.stdout).unwrap()["data"]["count"],
        1
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert!(!directory.path().join(SOCKET_NAME).exists());
}

#[tokio::test]
async fn stale_socket_symlinks_wrong_type_and_nonprivate_directory_are_rejected() {
    let directory = runtime_dir();
    let path = directory.path();
    let socket = path.join(SOCKET_NAME);
    let stale = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    drop(stale);
    assert!(matches!(
        UnixServer::bind(path),
        Err(HostError::EndpointOccupied)
    ));
    assert!(matches!(
        UnixConnector.connect(path.to_str().unwrap()).await,
        Err(HostError::Unavailable)
    ));
    assert!(socket.exists());
    fs::remove_file(&socket).unwrap();
    fs::write(&socket, "fixture").unwrap();
    assert!(matches!(
        UnixConnector.connect(path.to_str().unwrap()).await,
        Err(HostError::EndpointUnreadable)
    ));
    assert!(matches!(
        UnixServer::bind(path),
        Err(HostError::EndpointUnreadable)
    ));
    fs::remove_file(&socket).unwrap();
    symlink("/tmp/uebox-fixture-nonexistent", &socket).unwrap();
    assert!(matches!(
        UnixServer::bind(path),
        Err(HostError::EndpointUnreadable)
    ));
    assert!(matches!(
        UnixConnector.connect(path.to_str().unwrap()).await,
        Err(HostError::EndpointUnreadable)
    ));
    let alias = path.join("alias");
    symlink(path, &alias).unwrap();
    assert!(matches!(
        UnixServer::bind(&alias),
        Err(HostError::EndpointUnreadable)
    ));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        UnixServer::bind(path),
        Err(HostError::EndpointUnreadable)
    ));
}

#[tokio::test]
async fn disconnect_before_and_after_execution_have_different_state_and_no_retry() {
    for committed in [false, true] {
        let directory = runtime_dir();
        let fixture = Arc::new(Fixture {
            blocking_prepare: !committed,
            blocking_call: committed,
            ..Fixture::default()
        });
        let (stop, server) = start(directory.path(), fixture.clone()).await;
        let args = cli(
            directory.path(),
            &["tools", "call", "ue_project_info", "--timeout=5"],
        );
        let request = tokio::spawn(async move { run(&args, Path::new(""), &UnixConnector).await });
        let barrier = if committed {
            &fixture.running
        } else {
            &fixture.preparing
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), barrier.notified())
            .await
            .unwrap();
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
        let output = request.await.unwrap();
        assert_eq!(output.exit_code, 4);
        assert_eq!(
            serde_json::from_str::<Value>(&output.stdout).unwrap()["error"]["execution"],
            if committed { "unknown" } else { "not_started" }
        );
        assert_eq!(fixture.calls.load(Ordering::SeqCst), usize::from(committed));
        assert_eq!(
            fixture.cancelled.load(Ordering::SeqCst),
            usize::from(committed)
        );
    }
}

#[tokio::test]
async fn malformed_oversized_and_unprepared_frames_cannot_dispatch() {
    let directory = runtime_dir();
    let fixture = Arc::new(Fixture::default());
    let (stop, server) = start(directory.path(), fixture.clone()).await;
    for frame in [
        u32::MAX.to_be_bytes().to_vec(),
        framed(br#"{"type":"execute_read"}"#),
        framed(br#"{"type":"prepare_read","tool":"ue_project_info","connectionId":"generation-1","path":"/private"}"#),
        framed(br#"{"type":"prepare_read","tool":"shell","connectionId":"generation-1"}"#),
    ] {
        let mut client = UnixStream::connect(directory.path().join(SOCKET_NAME)).await.unwrap();
        client.write_all(&frame).await.unwrap();
        drop(client);
    }
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

fn framed(json: &[u8]) -> Vec<u8> {
    [(json.len() as u32).to_be_bytes().as_slice(), json].concat()
}
