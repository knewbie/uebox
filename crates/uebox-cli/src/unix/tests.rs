use std::{
    future::pending,
    sync::atomic::{AtomicUsize, Ordering},
};

use serde_json::{json, Value};

use super::*;
use crate::{run, AskReply, Engine};

#[derive(Default)]
struct Fixture {
    block_prepare: bool,
    block_read: bool,
    listings: AtomicUsize,
    dispatches: AtomicUsize,
    reject_at_admission: bool,
}

impl HostSession for Fixture {
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
            Ok(vec![OnlineProject {
                engine: Engine::Unreal,
                name: "Fixture".into(),
                path: "/fixture/project".into(),
                connection_id: "generation".into(),
            }])
        })
    }

    fn tools(&self) -> HostFuture<'_, Vec<ReadTool>> {
        Box::pin(async {
            if self.listings.fetch_add(1, Ordering::SeqCst) == 1 && self.block_prepare {
                pending::<()>().await;
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
            assert_eq!(project.connection_id, "generation");
            if self.reject_at_admission {
                return Err(HostError::ProjectNotConnected);
            }
            dispatch.mark_dispatched();
            self.dispatches.fetch_add(1, Ordering::SeqCst);
            if self.block_read {
                pending::<()>().await;
            }
            Ok(ToolReply {
                structured: json!({"fixture": true}),
                content: vec![],
            })
        })
    }
}

struct PairConnector(Mutex<Option<UnixStream>>);

impl Connector for PairConnector {
    fn connect<'a>(&'a self, _: &'a str) -> HostFuture<'a, Box<dyn HostSession + 'a>> {
        Box::pin(async {
            let stream = self.0.lock().await.take().ok_or(HostError::Unavailable)?;
            authenticate(&stream, path::effective_uid())?;
            Ok(Box::new(Session::negotiate(stream).await?) as Box<dyn HostSession>)
        })
    }
}

async fn invoke(fixture: Arc<Fixture>) -> (u8, Value) {
    let (front, back) = UnixStream::pair().unwrap();
    let server = tokio::spawn(async move {
        authenticate(&back, path::effective_uid()).unwrap();
        serve_session(back, fixture.as_ref()).await
    });
    let args = [
        "tools",
        "call",
        "ue_project_info",
        "--endpoint",
        "fixture",
        "--timeout",
        "1",
    ]
    .map(str::to_owned);
    let output = run(
        &args,
        Path::new(""),
        &PairConnector(Mutex::new(Some(front))),
    )
    .await;
    let _ = timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    (
        output.exit_code,
        serde_json::from_str(&output.stdout).unwrap(),
    )
}

#[tokio::test]
async fn authenticated_pairs_support_concurrent_reads_without_crossing_sessions() {
    let fixture = Arc::new(Fixture::default());
    let (one, two) = tokio::join!(invoke(fixture.clone()), invoke(fixture.clone()));
    for (exit, value) in [one, two] {
        assert_eq!(exit, 0);
        assert_eq!(value["data"]["structured"], json!({"fixture": true}));
    }
    assert_eq!(fixture.dispatches.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn time_limit_drops_the_session_on_either_side_of_dispatch() {
    for dispatched in [false, true] {
        let fixture = Arc::new(Fixture {
            block_prepare: !dispatched,
            block_read: dispatched,
            ..Fixture::default()
        });
        let (exit, output) = invoke(fixture.clone()).await;
        assert_eq!(exit, 7);
        assert_eq!(
            output["error"]["execution"],
            if dispatched { "unknown" } else { "not_started" }
        );
        assert_eq!(
            fixture.dispatches.load(Ordering::SeqCst),
            usize::from(dispatched)
        );
    }
}

#[tokio::test]
async fn a_definite_admission_rejection_is_not_reported_as_dispatched() {
    let fixture = Arc::new(Fixture {
        reject_at_admission: true,
        ..Fixture::default()
    });
    let (exit, output) = invoke(fixture.clone()).await;
    assert_eq!(exit, 5);
    assert_eq!(output["error"]["code"], "PROJECT_NOT_CONNECTED");
    assert_eq!(output["error"]["execution"], "not_started");
    assert_eq!(fixture.dispatches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn framing_rejects_oversized_values_and_reads_with_paths_or_unknown_tools() {
    use tokio::io::AsyncWriteExt;

    let capacity = MAX_RESPONSE;
    let oversized = json!({"text": "x".repeat(capacity)});
    let (mut write, _) = UnixStream::pair().unwrap();
    assert_eq!(
        wire::write(&mut write, &oversized, capacity)
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );

    for raw in [
        br#"{"type":"prepare_read","tool":"ue_project_info","connectionId":"generation","path":"/private"}"#.as_slice(),
        br#"{"type":"prepare_read","tool":"shell","connectionId":"generation"}"#.as_slice(),
    ] {
        let fixture = Arc::new(Fixture::default());
        let (mut front, back) = UnixStream::pair().unwrap();
        let host = fixture.clone();
        let server = tokio::spawn(async move { serve_session(back, host.as_ref()).await });
        assert!(matches!(exchange(&mut front, Request::Hello { version: CLI_CONTRACT_VERSION }).await.unwrap(), Response::Hello { .. }));
        front.write_u32(raw.len() as u32).await.unwrap();
        front.write_all(raw).await.unwrap();
        assert_eq!(server.await.unwrap().unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(fixture.dispatches.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn ask_frames_name_only_the_host_project_profile_and_bounded_question() {
    assert_eq!(
        serde_json::to_value(Request::PrepareAsk {
            project_id: "generation-1".into(),
            model_profile_id: "0123456789abcdef0123456789abcdef".into(),
            question: "Summarize the project".into(),
        })
        .unwrap(),
        json!({
            "type": "prepare_ask",
            "projectId": "generation-1",
            "modelProfileId": "0123456789abcdef0123456789abcdef",
            "question": "Summarize the project"
        })
    );
    assert_eq!(
        serde_json::to_value(Request::ExecuteAsk).unwrap(),
        json!({"type": "execute_ask"})
    );
    assert_eq!(
        serde_json::to_value(Response::Ask {
            reply: AskReply {
                text: "Fixture answer".into()
            }
        })
        .unwrap(),
        json!({"type":"ask","reply":{"text":"Fixture answer"}})
    );
}

#[tokio::test]
async fn prepare_ask_rejection_never_sends_execute_ask_or_marks_dispatch() {
    let (client, mut old_peer) = UnixStream::pair().unwrap();
    let old_host = tokio::spawn(async move {
        assert!(matches!(
            wire::read::<Request>(&mut old_peer, MAX_REQUEST)
                .await
                .unwrap(),
            Request::Hello {
                version: CLI_CONTRACT_VERSION
            }
        ));
        wire::write(
            &mut old_peer,
            &Response::Hello {
                capabilities: Capabilities {
                    cli_contract_version: CLI_CONTRACT_VERSION,
                    project_targeting: true,
                    public_results: true,
                },
            },
            MAX_RESPONSE,
        )
        .await
        .unwrap();
        assert!(matches!(
            wire::read::<Request>(&mut old_peer, MAX_REQUEST)
                .await
                .unwrap(),
            Request::PrepareAsk { .. }
        ));
        wire::write(
            &mut old_peer,
            &Response::rejected(HostError::Incompatible),
            MAX_RESPONSE,
        )
        .await
        .unwrap();
        match wire::read::<Request>(&mut old_peer, MAX_REQUEST).await {
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof),
            Ok(_) => panic!("the client sent ExecuteAsk after prepare was rejected"),
        }
    });

    let session = Session::negotiate(client).await.unwrap();
    let project = OnlineProject {
        engine: Engine::Unreal,
        name: "Fixture".into(),
        path: "/fixture/project".into(),
        connection_id: "generation".into(),
    };
    let dispatch = Dispatch::default();
    let result = session
        .ask(
            "0123456789abcdef0123456789abcdef",
            "question",
            &project,
            &[ReadTool::UnrealProjectInfo],
            &dispatch,
        )
        .await;
    assert_eq!(result.unwrap_err(), HostError::Incompatible);
    assert!(!dispatch.is_dispatched());
    old_host.await.unwrap();
}
