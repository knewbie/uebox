use serde_json::{json, Value};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use uebox_agent_core::{
    AgentSession, Engine, Error, OpenAiProvider, ProviderConfig, ReadOnlyTool, RunPhase, RunTarget,
    ToolDefinition, ToolError, ToolRegistry,
};

type FutureResult = Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send>>;

struct ProjectReader(Arc<Mutex<Vec<(RunTarget, Value)>>>);
impl ReadOnlyTool for ProjectReader {
    fn execute(&self, target: RunTarget, arguments: Value) -> FutureResult {
        self.0.lock().unwrap().push((target, arguments));
        Box::pin(async { Ok(json!({"version":"5.4"})) })
    }
}

async fn request(stream: &mut tokio::net::TcpStream) -> Value {
    let mut bytes = Vec::new();
    let mut buf = [0; 4096];
    let end = loop {
        let n = stream.read(&mut buf).await.unwrap();
        assert_ne!(n, 0);
        bytes.extend_from_slice(&buf[..n]);
        if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
    let len = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    while bytes.len() < end + len {
        let n = stream.read(&mut buf).await.unwrap();
        assert_ne!(n, 0);
        bytes.extend_from_slice(&buf[..n]);
    }
    serde_json::from_slice(&bytes[end..end + len]).unwrap()
}

async fn respond(stream: &mut tokio::net::TcpStream, body: &str) {
    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
}

#[tokio::test]
async fn refusal_is_visible_in_session_history_and_in_the_next_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        request(&mut first).await;
        respond(&mut first, concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":null,\"refusal\":\"I cannot \"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"refusal\":\"help.\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        )).await;
        let (mut second, _) = listener.accept().await.unwrap();
        let body = request(&mut second).await;
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["messages"][1]["content"], "I cannot help.");
        respond(&mut second, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let target = RunTarget::new(Engine::Unreal, "project", "editor", 1).unwrap();
    let session =
        AgentSession::new(provider, ToolRegistry::new(target), Arc::new(|_| true), 2).unwrap();
    let first = session
        .start("request")
        .unwrap()
        .execute(|_| {})
        .await
        .unwrap();
    assert_eq!(first.text, "I cannot help.");
    let snapshot = session.snapshot();
    assert!(
        matches!(&snapshot.messages[1], uebox_agent_core::Message::Assistant(message) if message.text == "I cannot help.")
    );
    assert_eq!(
        snapshot.last_outcome,
        Some(uebox_agent_core::RunOutcome::Completed)
    );
    assert_eq!(
        session
            .start("next")
            .unwrap()
            .execute(|_| {})
            .await
            .unwrap()
            .text,
        "ok"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn empty_refusal_is_not_committed_as_a_completed_assistant_message() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        request(&mut stream).await;
        respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":null,\"refusal\":\"\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let target = RunTarget::new(Engine::Unreal, "project", "empty-refusal-editor", 1).unwrap();
    let session =
        AgentSession::new(provider, ToolRegistry::new(target), Arc::new(|_| true), 2).unwrap();
    assert_eq!(
        session
            .start("request")
            .unwrap()
            .execute(|_| {})
            .await
            .unwrap_err(),
        Error::Protocol
    );
    let snapshot = session.snapshot();
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(
        snapshot.last_outcome,
        Some(uebox_agent_core::RunOutcome::Failed(Error::Protocol))
    );
    server.await.unwrap();
}

#[tokio::test]
async fn cancellation_from_final_delta_does_not_commit_assistant_history() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        request(&mut stream).await;
        respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let target = RunTarget::new(Engine::Unreal, "project", "cancel-final-editor", 1).unwrap();
    let session =
        AgentSession::new(provider, ToolRegistry::new(target), Arc::new(|_| true), 2).unwrap();
    let run = session.start("request").unwrap();
    let cancel = run.cancellation();
    assert_eq!(
        run.execute(|_| cancel.cancel()).await.unwrap_err(),
        Error::Cancelled
    );
    let snapshot = session.snapshot();
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.partial_text, "partial");
    assert_eq!(
        snapshot.last_outcome,
        Some(uebox_agent_core::RunOutcome::Failed(Error::Cancelled))
    );
    server.await.unwrap();
}

#[tokio::test]
async fn tools_are_called_and_results_feed_the_next_model_step_and_later_message() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let first = request(&mut stream).await;
        assert_eq!(first["tools"][0]["function"]["name"], "ue_project_info");
        respond(&mut stream, concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"ue_project_info\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        )).await;
        let (mut stream, _) = listener.accept().await.unwrap();
        let second = request(&mut stream).await;
        assert_eq!(second["messages"][1]["tool_calls"][0]["id"], "call-1");
        assert_eq!(second["messages"][2]["role"], "tool");
        assert_eq!(second["messages"][2]["tool_call_id"], "call-1");
        let result: Value =
            serde_json::from_str(second["messages"][2]["content"].as_str().unwrap()).unwrap();
        assert_eq!(
            result,
            json!({"ok":true,"data":{"version":"5.4"},"target":{"engine":"unreal","project":"project-a","instance":"editor-a","generation":7}})
        );
        respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Version 5.4\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
        let (mut stream, _) = listener.accept().await.unwrap();
        let third = request(&mut stream).await;
        assert_eq!(third["messages"].as_array().unwrap().len(), 5);
        assert_eq!(third["messages"][3]["content"], "Version 5.4");
        assert_eq!(third["messages"][4]["content"], "and now?");
        respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Still 5.4\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
    });
    let target = RunTarget::new(Engine::Unreal, "project-a", "editor-a", 7).unwrap();
    let executed = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new(target.clone());
    registry
        .register(
            Engine::Unreal,
            ToolDefinition::new(
                "ue_project_info",
                "Read project",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            Arc::new(ProjectReader(executed.clone())),
        )
        .unwrap();
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/v1/chat/completions"),
        "model",
    ))
    .unwrap();
    let session = AgentSession::new(provider, registry, Arc::new(|_| true), 4).unwrap();
    let result = session
        .start("version?")
        .unwrap()
        .execute(|_| {})
        .await
        .unwrap();
    assert_eq!(result.text, "Version 5.4");
    assert_eq!(executed.lock().unwrap().as_slice(), &[(target, json!({}))]);
    let next = session
        .start("and now?")
        .unwrap()
        .execute(|_| {})
        .await
        .unwrap();
    assert_eq!(next.text, "Still 5.4");
    server.await.unwrap();
}

struct FailingReader;
impl ReadOnlyTool for FailingReader {
    fn execute(&self, _: RunTarget, _: Value) -> FutureResult {
        Box::pin(async { Err(ToolError) })
    }
}

#[tokio::test]
async fn only_allowlisted_schema_valid_calls_run_and_errors_return_in_order() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        request(&mut stream).await;
        let calls = json!([
            {"index":0,"id":"bad-schema","type":"function","function":{"name":"read_project","arguments":"{\"scope\":\"summary\",\"target\":\"other\"}"}},
            {"index":1,"id":"unknown","type":"function","function":{"name":"run_shell","arguments":"{}"}},
            {"index":2,"id":"bad-json","type":"function","function":{"name":"read_project","arguments":"{"}},
            {"index":3,"id":"valid","type":"function","function":{"name":"read_project","arguments":"{\"scope\":\"summary\"}"}},
            {"index":4,"id":"failed","type":"function","function":{"name":"read_failure","arguments":"{}"}}
        ]);
        let event = json!({"choices":[{"index":0,"delta":{"tool_calls":calls},"finish_reason":"tool_calls"}]});
        respond(&mut stream, &format!("data: {event}\n\ndata: [DONE]\n\n")).await;
        let (mut stream, _) = listener.accept().await.unwrap();
        let followup = request(&mut stream).await;
        let messages = followup["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 7);
        for (index, (id, error)) in [
            ("bad-schema", "invalid_arguments"),
            ("unknown", "tool_not_allowed"),
            ("bad-json", "invalid_arguments"),
            ("failed", "tool_failed"),
        ]
        .iter()
        .enumerate()
        {
            let position = if index == 3 { 6 } else { index + 2 };
            assert_eq!(messages[position]["tool_call_id"], *id);
            let result: Value =
                serde_json::from_str(messages[position]["content"].as_str().unwrap()).unwrap();
            assert_eq!(result["error"], *error);
            assert_eq!(result["ok"], false);
            assert_eq!(result["target"]["engine"], "unreal");
        }
        assert_eq!(messages[5]["tool_call_id"], "valid");
        respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"finished\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
    });
    let target = RunTarget::new(Engine::Unreal, "project-a", "editor-a", 7).unwrap();
    let executed = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new(target.clone());
    let schema = json!({"type":"object","properties":{"scope":{"type":"string","enum":["summary"]}},"required":["scope"],"additionalProperties":false});
    registry
        .register(
            Engine::Unreal,
            ToolDefinition::new("read_project", "Read", schema),
            Arc::new(ProjectReader(executed.clone())),
        )
        .unwrap();
    registry
        .register(
            Engine::Unreal,
            ToolDefinition::new(
                "read_failure",
                "Read",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            Arc::new(FailingReader),
        )
        .unwrap();
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let session = AgentSession::new(provider, registry, Arc::new(|_| true), 3).unwrap();
    assert_eq!(
        session
            .start("inspect")
            .unwrap()
            .execute(|_| {})
            .await
            .unwrap()
            .text,
        "finished"
    );
    assert_eq!(
        executed.lock().unwrap().as_slice(),
        &[(target, json!({"scope":"summary"}))]
    );
    server.await.unwrap();
}

#[tokio::test]
async fn cancellation_releases_busy_session_and_old_handle_cannot_cancel_a_new_run() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut hanging, _) = listener.accept().await.unwrap();
        request(&mut hanging).await;
        started_tx.send(()).unwrap();
        let (mut second, _) = listener.accept().await.unwrap();
        let body = request(&mut second).await;
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        respond(&mut second, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
        drop(hanging);
    });
    let target = RunTarget::new(Engine::Unity, "project-u", "editor-u", 1).unwrap();
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let session = AgentSession::new(
        provider,
        ToolRegistry::new(target.clone()),
        Arc::new(|_| true),
        3,
    )
    .unwrap();
    let run = session.start("first").unwrap();
    let old_cancel = run.cancellation();
    assert_eq!(session.snapshot().phase, Some(RunPhase::Preparing));
    assert!(matches!(session.start("busy"), Err(Error::Busy)));
    let task = tokio::spawn(run.execute(|_| {}));
    started_rx.await.unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.target, target);
    assert_eq!(snapshot.phase, Some(RunPhase::RunningModel));
    old_cancel.cancel();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        Error::Cancelled
    );
    assert_eq!(session.snapshot().phase, None);
    let next = session.start("second").unwrap();
    old_cancel.cancel();
    assert_eq!(next.execute(|_| {}).await.unwrap().text, "ok");
    server.await.unwrap();
}

#[test]
fn registry_rejects_cross_engine_tools_and_external_schema_references() {
    for (target_engine, other) in [
        (Engine::Unreal, Engine::Unity),
        (Engine::Unity, Engine::Godot),
        (Engine::Godot, Engine::Unreal),
    ] {
        let target = RunTarget::new(target_engine, "project", "editor", 1).unwrap();
        let mut registry = ToolRegistry::new(target);
        let reader = Arc::new(ProjectReader(Arc::new(Mutex::new(Vec::new()))));
        let schema = json!({"type":"object","properties":{},"additionalProperties":false});
        assert!(matches!(
            registry.register(
                other,
                ToolDefinition::new("read", "Read", schema),
                reader.clone()
            ),
            Err(Error::WrongEngine)
        ));
        let external = json!({"type":"object","properties":{"x":{"$ref":"file:///does-not-exist.json"}},"additionalProperties":false});
        assert!(matches!(
            registry.register(
                target_engine,
                ToolDefinition::new("read", "Read", external),
                reader.clone()
            ),
            Err(Error::InvalidTool)
        ));
        let unsupported_draft = json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{},"additionalProperties":false});
        assert!(matches!(
            registry.register(
                target_engine,
                ToolDefinition::new("read", "Read", unsupported_draft),
                reader.clone()
            ),
            Err(Error::InvalidTool)
        ));
        let unknown_keyword = json!({"type":"object","properties":{"selection":{"type":"array","prefixItems":[{"const":"safe"}]}},"additionalProperties":false});
        assert!(matches!(
            registry.register(
                target_engine,
                ToolDefinition::new("read", "Read", unknown_keyword),
                reader.clone()
            ),
            Err(Error::InvalidTool)
        ));
        let unknown_format = json!({"type":"object","properties":{"selection":{"type":"string","format":"unsupported-custom"}},"additionalProperties":false});
        assert!(matches!(
            registry.register(
                target_engine,
                ToolDefinition::new("read", "Read", unknown_format),
                reader.clone()
            ),
            Err(Error::InvalidTool)
        ));
        let unsupported_encoding = json!({"type":"object","properties":{"selection":{"type":"string","contentEncoding":"custom-encoding"}},"additionalProperties":false});
        assert!(matches!(
            registry.register(
                target_engine,
                ToolDefinition::new("read", "Read", unsupported_encoding),
                reader
            ),
            Err(Error::InvalidTool)
        ));
    }
}

#[tokio::test]
async fn truncated_tool_stream_never_dispatches_even_after_finish_reason() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        request(&mut stream).await;
        respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"read_project\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n").await;
    });
    let target = RunTarget::new(Engine::Godot, "project-g", "editor-g", 4).unwrap();
    let executed = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new(target);
    registry
        .register(
            Engine::Godot,
            ToolDefinition::new(
                "read_project",
                "Read",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            Arc::new(ProjectReader(executed.clone())),
        )
        .unwrap();
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let session = AgentSession::new(provider, registry, Arc::new(|_| true), 3).unwrap();
    assert_eq!(
        session
            .start("inspect")
            .unwrap()
            .execute(|_| {})
            .await
            .unwrap_err(),
        Error::Protocol
    );
    assert!(executed.lock().unwrap().is_empty());
    assert_eq!(session.snapshot().messages.len(), 1);
    server.await.unwrap();
}

#[tokio::test]
async fn target_generation_becoming_stale_during_model_response_prevents_dispatch() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let current = Arc::new(AtomicBool::new(true));
    let on_server = current.clone();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        request(&mut stream).await;
        on_server.store(false, Ordering::SeqCst);
        respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"read_project\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n").await;
    });
    let target = RunTarget::new(Engine::Unreal, "project-a", "editor-a", 7).unwrap();
    let executed = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new(target);
    registry
        .register(
            Engine::Unreal,
            ToolDefinition::new(
                "read_project",
                "Read",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            Arc::new(ProjectReader(executed.clone())),
        )
        .unwrap();
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let session = AgentSession::new(
        provider,
        registry,
        Arc::new(move |_| current.load(Ordering::SeqCst)),
        3,
    )
    .unwrap();
    assert_eq!(
        session
            .start("inspect")
            .unwrap()
            .execute(|_| {})
            .await
            .unwrap_err(),
        Error::TargetChanged
    );
    assert!(executed.lock().unwrap().is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn cancellation_during_pre_dispatch_guard_cannot_invoke_tool() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for cancel_at in [2, 3, 4] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            request(&mut stream).await;
            respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"read_project\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n").await;
        });
        let executed = Arc::new(Mutex::new(Vec::new()));
        let mut registry =
            ToolRegistry::new(RunTarget::new(Engine::Unreal, "project", "editor", 1).unwrap());
        registry
            .register(
                Engine::Unreal,
                ToolDefinition::new(
                    "read_project",
                    "Read",
                    json!({"type":"object","properties":{},"additionalProperties":false}),
                ),
                Arc::new(ProjectReader(executed.clone())),
            )
            .unwrap();
        let cancel_slot = Arc::new(Mutex::new(None::<uebox_agent_core::Cancellation>));
        let checks = Arc::new(AtomicUsize::new(0));
        let guard_slot = cancel_slot.clone();
        let guard_checks = checks.clone();
        let guard = Arc::new(move |_: &RunTarget| {
            if guard_checks.fetch_add(1, Ordering::SeqCst) == cancel_at {
                guard_slot.lock().unwrap().as_ref().unwrap().cancel();
            }
            true
        });
        let provider = OpenAiProvider::new(ProviderConfig::new(
            format!("http://{address}/completions"),
            "model",
        ))
        .unwrap();
        let session = AgentSession::new(provider, registry, guard, 3).unwrap();
        let run = session.start("inspect").unwrap();
        *cancel_slot.lock().unwrap() = Some(run.cancellation());
        assert_eq!(run.execute(|_| {}).await.unwrap_err(), Error::Cancelled);
        assert!(executed.lock().unwrap().is_empty());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn cancellation_before_tool_batch_commit_does_not_persist_unfinished_batch() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        request(&mut stream).await;
        respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"unknown\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n").await;
    });
    let cancel_slot = Arc::new(Mutex::new(None::<uebox_agent_core::Cancellation>));
    let checks = Arc::new(AtomicUsize::new(0));
    let guard_slot = cancel_slot.clone();
    let guard = Arc::new(move |_: &RunTarget| {
        if checks.fetch_add(1, Ordering::SeqCst) == 2 {
            guard_slot.lock().unwrap().as_ref().unwrap().cancel();
        }
        true
    });
    let target = RunTarget::new(Engine::Unreal, "project", "commit-editor", 1).unwrap();
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let session = AgentSession::new(provider, ToolRegistry::new(target), guard, 3).unwrap();
    let run = session.start("inspect").unwrap();
    *cancel_slot.lock().unwrap() = Some(run.cancellation());
    assert_eq!(run.execute(|_| {}).await.unwrap_err(), Error::Cancelled);
    assert_eq!(session.snapshot().messages.len(), 1);
    server.await.unwrap();
}

#[tokio::test]
async fn system_instruction_is_sent_once_on_each_step_without_becoming_history() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for expected_len in [2, 4] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let body = request(&mut stream).await;
            let messages = body["messages"].as_array().unwrap();
            assert_eq!(messages.len(), expected_len);
            assert_eq!(messages[0], json!({"role":"system","content":"Be concise"}));
            assert_eq!(
                messages
                    .iter()
                    .filter(|message| message["role"] == "system")
                    .count(),
                1
            );
            respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
        }
    });
    let target = RunTarget::new(Engine::Godot, "project", "editor", 1).unwrap();
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let session = AgentSession::new_with_system_prompt(
        provider,
        ToolRegistry::new(target),
        Arc::new(|_| true),
        2,
        "Be concise",
    )
    .unwrap();
    session
        .start("first")
        .unwrap()
        .execute(|_| {})
        .await
        .unwrap();
    session
        .start("second")
        .unwrap()
        .execute(|_| {})
        .await
        .unwrap();
    assert_eq!(session.snapshot().messages.len(), 4);
    server.await.unwrap();
}

struct HangingReader {
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    finish: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}
impl ReadOnlyTool for HangingReader {
    fn execute(&self, _: RunTarget, _: Value) -> FutureResult {
        let notify = self.started.lock().unwrap().take().unwrap();
        let finish = self.finish.lock().unwrap().take().unwrap();
        Box::pin(async move {
            notify.send(()).unwrap();
            finish.await.unwrap();
            Ok(json!({"drained":true}))
        })
    }
}

#[tokio::test]
async fn cancellation_returns_promptly_but_session_remains_busy_until_tool_drains() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        request(&mut stream).await;
        respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"read_wait\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n").await;
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = request(&mut stream).await;
        let messages = request["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert!(messages.iter().all(|message| message["role"] == "user"));
        respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
    });
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let target = RunTarget::new(Engine::Unity, "project", "editor", 1).unwrap();
    let mut registry = ToolRegistry::new(target);
    registry
        .register(
            Engine::Unity,
            ToolDefinition::new(
                "read_wait",
                "Read",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            Arc::new(HangingReader {
                started: Mutex::new(Some(started_tx)),
                finish: Mutex::new(Some(finish_rx)),
            }),
        )
        .unwrap();
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let session = AgentSession::new(provider, registry, Arc::new(|_| true), 3).unwrap();
    let run = session.start("first").unwrap();
    let cancel = run.cancellation();
    let task = tokio::spawn(run.execute(|_| {}));
    started_rx.await.unwrap();
    assert_eq!(session.snapshot().phase, Some(RunPhase::RunningTools));
    cancel.cancel();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        Error::Cancelled
    );
    assert_eq!(session.snapshot().messages.len(), 1);
    assert_eq!(session.snapshot().phase, Some(RunPhase::Draining));
    assert!(matches!(session.start("too early"), Err(Error::Busy)));
    finish_tx.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while session.snapshot().phase.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        session
            .start("second")
            .unwrap()
            .execute(|_| {})
            .await
            .unwrap()
            .text,
        "ok"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn snapshot_contains_streamed_text_while_model_response_is_in_flight() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        request(&mut stream).await;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"draft\"},\"finish_reason\":null}]}\n\n").await.unwrap();
        release_rx.await.unwrap();
        stream.write_all(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await.unwrap();
    });
    let target = RunTarget::new(Engine::Godot, "project", "editor", 1).unwrap();
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let session =
        AgentSession::new(provider, ToolRegistry::new(target), Arc::new(|_| true), 2).unwrap();
    let (event_tx, event_rx) = tokio::sync::oneshot::channel();
    let mut event_tx = Some(event_tx);
    let run = session.start("inspect").unwrap();
    let task = tokio::spawn(run.execute(move |_| {
        if let Some(sender) = event_tx.take() {
            sender.send(()).unwrap();
        }
    }));
    event_rx.await.unwrap();
    assert_eq!(session.snapshot().partial_text, "draft");
    release_tx.send(()).unwrap();
    assert_eq!(task.await.unwrap().unwrap().text, "draft done");
    assert!(session.snapshot().partial_text.is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn interrupted_stream_keeps_partial_text_separate_from_committed_history() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        request(&mut stream).await;
        respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n").await;
    });
    let target = RunTarget::new(Engine::Godot, "project", "editor", 1).unwrap();
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let session =
        AgentSession::new(provider, ToolRegistry::new(target), Arc::new(|_| true), 2).unwrap();
    assert_eq!(
        session
            .start("inspect")
            .unwrap()
            .execute(|_| {})
            .await
            .unwrap_err(),
        Error::Protocol
    );
    let snapshot = session.snapshot();
    assert_eq!(snapshot.partial_text, "partial");
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(
        snapshot.last_outcome,
        Some(uebox_agent_core::RunOutcome::Failed(Error::Protocol))
    );
    server.await.unwrap();
}

#[test]
fn simultaneous_starts_reserve_the_session_atomically() {
    use std::sync::Barrier;
    let target = RunTarget::new(Engine::Unreal, "project", "editor", 1).unwrap();
    let provider = OpenAiProvider::new(ProviderConfig::new(
        "http://127.0.0.1:1/completions",
        "model",
    ))
    .unwrap();
    let session = Arc::new(
        AgentSession::new(provider, ToolRegistry::new(target), Arc::new(|_| true), 2).unwrap(),
    );
    let start = Arc::new(Barrier::new(9));
    let finish = Arc::new(Barrier::new(9));
    let mut workers = Vec::new();
    for _ in 0..8 {
        let session = session.clone();
        let start = start.clone();
        let finish = finish.clone();
        workers.push(std::thread::spawn(move || {
            start.wait();
            let run = session.start("inspect");
            let accepted = run.is_ok();
            finish.wait();
            drop(run);
            accepted
        }));
    }
    start.wait();
    finish.wait();
    assert_eq!(
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|accepted| *accepted)
            .count(),
        1
    );
    assert_eq!(session.snapshot().messages.len(), 1);
}

struct SharedReader {
    started: tokio::sync::mpsc::UnboundedSender<usize>,
    calls: std::sync::atomic::AtomicUsize,
    release: Arc<tokio::sync::Notify>,
}
impl ReadOnlyTool for SharedReader {
    fn execute(&self, _: RunTarget, _: Value) -> FutureResult {
        use std::sync::atomic::Ordering;
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.send(index).unwrap();
        let release = self.release.clone();
        Box::pin(async move {
            if index == 0 {
                release.notified().await;
            }
            Ok(json!({"index":index}))
        })
    }
}

#[tokio::test]
async fn separate_sessions_serialize_tools_on_the_same_editor_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut handlers = Vec::new();
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().await.unwrap();
            handlers.push(tokio::spawn(async move {
                let body = request(&mut stream).await;
                if body["messages"].as_array().unwrap().len() == 1 {
                    respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"read_project\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n").await;
                } else {
                    respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
                }
            }));
        }
        for handler in handlers {
            handler.await.unwrap();
        }
    });
    let target = RunTarget::new(Engine::Unity, "project", "editor", 42).unwrap();
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let reader = Arc::new(SharedReader {
        started: started_tx,
        calls: std::sync::atomic::AtomicUsize::new(0),
        release: release.clone(),
    });
    let make_session = || {
        let mut registry = ToolRegistry::new(target.clone());
        registry
            .register(
                Engine::Unity,
                ToolDefinition::new(
                    "read_project",
                    "Read",
                    json!({"type":"object","properties":{},"additionalProperties":false}),
                ),
                reader.clone(),
            )
            .unwrap();
        let provider = OpenAiProvider::new(ProviderConfig::new(
            format!("http://{address}/completions"),
            "model",
        ))
        .unwrap();
        AgentSession::new(provider, registry, Arc::new(|_| true), 3).unwrap()
    };
    let first = make_session();
    let second = make_session();
    let first_run = tokio::spawn(first.start("first").unwrap().execute(|_| {}));
    let second_run = tokio::spawn(second.start("second").unwrap().execute(|_| {}));
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), started_rx.recv())
            .await
            .unwrap(),
        Some(0)
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), started_rx.recv())
            .await
            .is_err()
    );
    release.notify_one();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), started_rx.recv())
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(first_run.await.unwrap().unwrap().text, "ok");
    assert_eq!(second_run.await.unwrap().unwrap().text, "ok");
    server.await.unwrap();
}

#[tokio::test]
async fn cancelling_a_tool_queued_behind_another_session_never_dispatches_it() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut handlers = Vec::new();
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            handlers.push(tokio::spawn(async move {
                let body = request(&mut stream).await;
                if body["messages"].as_array().unwrap().len() == 1 {
                    respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"read_project\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n").await;
                } else {
                    respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
                }
            }));
        }
        for handler in handlers {
            handler.await.unwrap();
        }
    });
    let target = RunTarget::new(Engine::Unity, "project", "queued-editor", 42).unwrap();
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let reader = Arc::new(SharedReader {
        started: started_tx,
        calls: std::sync::atomic::AtomicUsize::new(0),
        release: release.clone(),
    });
    let make_session = || {
        let mut registry = ToolRegistry::new(target.clone());
        registry
            .register(
                Engine::Unity,
                ToolDefinition::new(
                    "read_project",
                    "Read",
                    json!({"type":"object","properties":{},"additionalProperties":false}),
                ),
                reader.clone(),
            )
            .unwrap();
        let provider = OpenAiProvider::new(ProviderConfig::new(
            format!("http://{address}/completions"),
            "model",
        ))
        .unwrap();
        AgentSession::new(provider, registry, Arc::new(|_| true), 3).unwrap()
    };
    let first = make_session();
    let second = make_session();
    let first_run = tokio::spawn(first.start("first").unwrap().execute(|_| {}));
    assert_eq!(started_rx.recv().await, Some(0));
    let second_run = second.start("second").unwrap();
    let cancel = second_run.cancellation();
    let second_task = tokio::spawn(second_run.execute(|_| {}));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while second.snapshot().phase != Some(RunPhase::RunningTools) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    assert_eq!(second_task.await.unwrap().unwrap_err(), Error::Cancelled);
    assert_eq!(second.snapshot().phase, None);
    assert!(started_rx.try_recv().is_err());
    release.notify_one();
    assert_eq!(first_run.await.unwrap().unwrap().text, "ok");
    server.await.unwrap();
}

#[tokio::test]
async fn new_generation_waits_for_cancelled_old_tool_to_finish_draining() {
    use std::sync::atomic::{AtomicU64, Ordering};
    struct GenerationReader {
        started: tokio::sync::mpsc::UnboundedSender<usize>,
        calls: std::sync::atomic::AtomicUsize,
        release_old: Arc<tokio::sync::Notify>,
        release_new: Arc<tokio::sync::Notify>,
    }
    impl ReadOnlyTool for GenerationReader {
        fn execute(&self, _: RunTarget, _: Value) -> FutureResult {
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.send(index).unwrap();
            let release = if index == 0 {
                self.release_old.clone()
            } else {
                self.release_new.clone()
            };
            Box::pin(async move {
                release.notified().await;
                Ok(json!({"index":index}))
            })
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut handlers = Vec::new();
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            handlers.push(tokio::spawn(async move {
                let body = request(&mut stream).await;
                if body["messages"].as_array().unwrap().len() == 1 {
                    respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"read_project\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n").await;
                } else {
                    respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
                }
            }));
        }
        for handler in handlers {
            handler.await.unwrap();
        }
    });
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let release_old = Arc::new(tokio::sync::Notify::new());
    let release_new = Arc::new(tokio::sync::Notify::new());
    let reader = Arc::new(GenerationReader {
        started: started_tx,
        calls: std::sync::atomic::AtomicUsize::new(0),
        release_old: release_old.clone(),
        release_new: release_new.clone(),
    });
    let current = Arc::new(AtomicU64::new(1));
    let make_session = |generation| {
        let target = RunTarget::new(Engine::Unity, "project", "stable-editor", generation).unwrap();
        let mut registry = ToolRegistry::new(target);
        registry
            .register(
                Engine::Unity,
                ToolDefinition::new(
                    "read_project",
                    "Read",
                    json!({"type":"object","properties":{},"additionalProperties":false}),
                ),
                reader.clone(),
            )
            .unwrap();
        let provider = OpenAiProvider::new(ProviderConfig::new(
            format!("http://{address}/completions"),
            "model",
        ))
        .unwrap();
        let current = current.clone();
        AgentSession::new(
            provider,
            registry,
            Arc::new(move |target| target.generation() == current.load(Ordering::SeqCst)),
            3,
        )
        .unwrap()
    };
    let old = make_session(1);
    let old_run = old.start("old").unwrap();
    let cancel = old_run.cancellation();
    let old_task = tokio::spawn(old_run.execute(|_| {}));
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), started_rx.recv())
            .await
            .unwrap(),
        Some(0)
    );
    cancel.cancel();
    assert_eq!(old_task.await.unwrap().unwrap_err(), Error::Cancelled);
    assert_eq!(old.snapshot().phase, Some(RunPhase::Draining));
    assert_eq!(old.snapshot().messages.len(), 1);
    drop(old);
    current.store(2, Ordering::SeqCst);
    let new = make_session(2);
    let new_task = tokio::spawn(new.start("new").unwrap().execute(|_| {}));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while new.snapshot().phase != Some(RunPhase::RunningTools) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::task::yield_now().await;
    assert_eq!(reader.calls.load(Ordering::SeqCst), 1);
    assert!(started_rx.try_recv().is_err());
    release_old.notify_one();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), started_rx.recv())
            .await
            .unwrap(),
        Some(1)
    );
    release_new.notify_one();
    assert_eq!(new_task.await.unwrap().unwrap().text, "ok");
    server.await.unwrap();
}

#[tokio::test]
async fn identical_instance_names_in_different_engines_do_not_share_a_gate() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut handlers = Vec::new();
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().await.unwrap();
            handlers.push(tokio::spawn(async move {
                let body = request(&mut stream).await;
                if body["messages"].as_array().unwrap().len() == 1 {
                    respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"read_project\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n").await;
                } else {
                    respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
                }
            }));
        }
        for handler in handlers {
            handler.await.unwrap();
        }
    });
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let reader = Arc::new(SharedReader {
        started: started_tx,
        calls: std::sync::atomic::AtomicUsize::new(0),
        release: release.clone(),
    });
    let make_session = |engine| {
        let target = RunTarget::new(engine, "project", "same-instance", 42).unwrap();
        let mut registry = ToolRegistry::new(target);
        registry
            .register(
                engine,
                ToolDefinition::new(
                    "read_project",
                    "Read",
                    json!({"type":"object","properties":{},"additionalProperties":false}),
                ),
                reader.clone(),
            )
            .unwrap();
        let provider = OpenAiProvider::new(ProviderConfig::new(
            format!("http://{address}/completions"),
            "model",
        ))
        .unwrap();
        AgentSession::new(provider, registry, Arc::new(|_| true), 3).unwrap()
    };
    let unity = make_session(Engine::Unity);
    let godot = make_session(Engine::Godot);
    let first = tokio::spawn(unity.start("unity").unwrap().execute(|_| {}));
    assert_eq!(started_rx.recv().await, Some(0));
    let second = tokio::spawn(godot.start("godot").unwrap().execute(|_| {}));
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), started_rx.recv())
            .await
            .unwrap(),
        Some(1)
    );
    release.notify_one();
    assert_eq!(first.await.unwrap().unwrap().text, "ok");
    assert_eq!(second.await.unwrap().unwrap().text, "ok");
    server.await.unwrap();
}

#[tokio::test]
async fn model_step_limit_stops_before_a_tool_that_cannot_be_followed_up() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            request(&mut stream).await;
            respond(&mut stream, "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"read_project\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n").await;
        }
    });
    let target = RunTarget::new(Engine::Godot, "project", "limit-editor", 1).unwrap();
    let executed = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new(target);
    registry
        .register(
            Engine::Godot,
            ToolDefinition::new(
                "read_project",
                "Read",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            Arc::new(ProjectReader(executed.clone())),
        )
        .unwrap();
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let session = AgentSession::new(provider, registry, Arc::new(|_| true), 2).unwrap();
    assert_eq!(
        session
            .start("inspect")
            .unwrap()
            .execute(|_| {})
            .await
            .unwrap_err(),
        Error::StepLimit
    );
    assert_eq!(executed.lock().unwrap().len(), 1);
    assert_eq!(session.snapshot().messages.len(), 3);
    server.await.unwrap();
}
