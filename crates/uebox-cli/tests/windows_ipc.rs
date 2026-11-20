#![cfg(windows)]

use std::{
    path::Path,
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use serde_json::{json, Value};
use tokio::{
    io::AsyncWriteExt,
    net::windows::named_pipe::{ClientOptions, ServerOptions},
    sync::{oneshot, Notify},
};
use uebox_cli::{
    run,
    windows::{WindowsConnector, WindowsServer, APP_IDENTIFIER, ENDPOINT_COMPONENT, PIPE_PREFIX},
    Capabilities, Connector, Dispatch, Engine, HostError, HostFuture, HostSession, OnlineProject,
    ReadTool, ToolReply,
};

fn fixture(tag: &str) -> String {
    format!("{PIPE_PREFIX}{APP_IDENTIFIER}.{ENDPOINT_COMPONENT}-ipc-{tag}")
}

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
                path: Path::new("C:\\fixture\\online").to_path_buf(),
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
            assert_eq!(project.path, Path::new("C:\\fixture\\online"));
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

async fn start(
    name: String,
    fixture: Arc<Fixture>,
) -> (
    oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), HostError>>,
) {
    let server = WindowsServer::bind(&name).unwrap();
    let (send, receive) = oneshot::channel();
    let handle = tokio::spawn(server.serve(fixture, async {
        let _ = receive.await;
    }));
    (send, handle)
}

fn cli(endpoint: &str, command: &[&str]) -> Vec<String> {
    let mut args = vec!["--endpoint".into(), endpoint.into()];
    args.extend(command.iter().map(|name| name.to_string()));
    args
}

#[tokio::test]
async fn two_clients_keep_independent_sessions_and_the_name_has_one_owner() {
    let name = fixture("exclusive");
    let fixture = Arc::new(Fixture::default());
    let (stop, server) = start(name.clone(), fixture.clone()).await;

    // The name is owned by this host alone: a second one is refused instead of inheriting or
    // rewriting the descriptor, which is the counterpart of never unlinking an existing socket.
    assert!(matches!(
        WindowsServer::bind(&name),
        Err(HostError::EndpointOccupied)
    ));

    let reads = cli(&name, &["tools", "call", "ue_project_info"]);
    let (first, second) = tokio::join!(
        run(&reads, Path::new(""), &WindowsConnector),
        run(&reads, Path::new(""), &WindowsConnector),
    );
    for output in [first, second] {
        assert_eq!(output.exit_code, 0, "{}", output.stdout);
        assert_eq!(
            serde_json::from_str::<Value>(&output.stdout).unwrap()["data"]["structured"]["fixture"],
            true
        );
    }
    let listing = run(
        &cli(&name, &["projects", "list", "--json"]),
        Path::new(""),
        &WindowsConnector,
    )
    .await;
    assert_eq!(listing.exit_code, 0, "{}", listing.stdout);
    assert_eq!(
        serde_json::from_str::<Value>(&listing.stdout).unwrap()["data"]["count"],
        1
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);

    stop.send(()).unwrap();
    server.await.unwrap().unwrap();

    // Nothing is left behind to block the next start: the name disappears with the last instance
    // handle, so a restarted host binds cleanly.
    WindowsServer::bind(&name).unwrap();
}

// Needs the host wiring in `main.rs` (the Windows branch of the connector selection) to pass.
#[tokio::test]
async fn binary_connects_to_temporary_host() {
    let name = fixture("binary");
    let fixture = Arc::new(Fixture::default());
    let (stop, server) = start(name.clone(), fixture.clone()).await;
    let args = cli(&name, &["projects", "list", "--json"]);
    // No env_clear: a Windows child needs SYSTEMROOT to start, and the console reads no ambient
    // configuration anyway — the endpoint is always explicit here.
    let child = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_uebox"))
            .args(args)
            .output()
            .unwrap()
    });
    let child = child.await.unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stdout)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&child.stdout).unwrap()["data"]["count"],
        1
    );
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn taken_names_foreign_descriptors_and_remote_endpoints_are_refused() {
    let taken = fixture("taken");
    // Somebody else established the name first, with the default descriptor: full control for
    // LocalSystem and administrators, read for Everyone and the anonymous account.
    let foreign = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&taken)
        .unwrap();

    // Neither end takes over. Binding is refused because the first-instance flag says the name is
    // spoken for, and connecting is refused because the descriptor is not the private one — the
    // client never sends a frame to a pipe it cannot identify. The timeout also pins down that the
    // open does not wait for a server that never accepts: a stalled runtime would be a hang here.
    assert!(matches!(
        WindowsServer::bind(&taken),
        Err(HostError::EndpointOccupied)
    ));
    let refused = tokio::time::timeout(Duration::from_secs(10), WindowsConnector.connect(&taken))
        .await
        .expect("the client refuses an unidentified pipe instead of waiting for it");
    assert!(matches!(refused, Err(HostError::Authentication)));
    assert!(matches!(
        WindowsServer::bind(&taken),
        Err(HostError::EndpointOccupied)
    ));
    drop(foreign);

    // A name that was never created is an outage, not a licence to guess.
    assert!(matches!(
        WindowsConnector.connect(&fixture("absent")).await,
        Err(HostError::Unavailable)
    ));

    for endpoint in [
        r"\\fileserver\pipe\uebox".to_owned(),
        r"\\.\pipe".to_owned(),
        PIPE_PREFIX.to_owned(),
        r"\\.\pipe\a\b".to_owned(),
        String::new(),
    ] {
        assert!(matches!(
            WindowsServer::bind(&endpoint),
            Err(HostError::EndpointUnreadable)
        ));
        assert!(matches!(
            WindowsConnector.connect(&endpoint).await,
            Err(HostError::EndpointUnreadable)
        ));
    }
}

#[tokio::test]
async fn disconnect_before_and_after_execution_have_different_state_and_no_retry() {
    for committed in [false, true] {
        let name = fixture(if committed {
            "after-dispatch"
        } else {
            "before-dispatch"
        });
        let fixture = Arc::new(Fixture {
            blocking_prepare: !committed,
            blocking_call: committed,
            ..Fixture::default()
        });
        let (stop, server) = start(name.clone(), fixture.clone()).await;
        let args = cli(&name, &["tools", "call", "ue_project_info", "--timeout=5"]);
        let request =
            tokio::spawn(async move { run(&args, Path::new(""), &WindowsConnector).await });
        let barrier = if committed {
            &fixture.running
        } else {
            &fixture.preparing
        };
        tokio::time::timeout(Duration::from_secs(5), barrier.notified())
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
    let name = fixture("bad-frames");
    let fixture = Arc::new(Fixture::default());
    let (stop, server) = start(name.clone(), fixture.clone()).await;
    for frame in [
        u32::MAX.to_be_bytes().to_vec(),
        framed(br#"{"type":"execute_read"}"#),
        framed(br#"{"type":"prepare_read","tool":"ue_project_info","connectionId":"generation-1","path":"C:\\private"}"#),
        framed(br#"{"type":"prepare_read","tool":"shell","connectionId":"generation-1"}"#),
    ] {
        // A raw client that skips the handshake.
        let mut client = ClientOptions::new().open(&name).unwrap();
        // The write itself may fail: a client that asked for no quality of service can be refused
        // at impersonation, and that is also a session that never reaches the editor. Either way no
        // frame is dispatched — the unit tests are what prove the parser rejects these exact frames.
        let _ = client.write_all(&frame).await;
        drop(client);
    }
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

fn framed(json: &[u8]) -> Vec<u8> {
    [(json.len() as u32).to_be_bytes().as_slice(), json].concat()
}
