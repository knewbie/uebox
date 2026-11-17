use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use uebox_agent_godot_tools::GodotToolBinding;
use uebox_engine_core::{
    ConnectionToken, EditorSelection, EngineKind, EngineRegistry, ProjectIdentity,
};
use uebox_godot_session::{EditorMode, GodotSession, LaunchOptions, SessionError};

static NEXT: AtomicU64 = AtomicU64::new(1);
struct TemporaryRoot(Option<PathBuf>);
impl Drop for TemporaryRoot {
    fn drop(&mut self) {
        if let Some(root) = self.0.take() {
            let _ = fs::remove_dir_all(root);
        }
    }
}
struct StartingEditor(Option<GodotSession>);
impl Drop for StartingEditor {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            let mut child = session.into_editor_process();
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
struct TestEditor {
    root: PathBuf,
    project: PathBuf,
    session: Option<Arc<Mutex<Option<GodotSession>>>>,
}
impl TestEditor {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "uebox-agent-godot-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let mut root_guard = TemporaryRoot(Some(root.clone()));
        let project = root.join("project 世界");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("project.godot"), b"config_version=5\n").unwrap();
        let home = root.join("home");
        for dir in ["appdata", "localappdata", "config", "data", "cache", "temp"] {
            fs::create_dir_all(home.join(dir)).unwrap();
        }
        let executable = PathBuf::from(env!("CARGO_BIN_EXE_godot-agent-test-editor"));
        let options = LaunchOptions::new(executable, &project)
            .acknowledge_legacy_protocol_risks()
            .editor_mode(EditorMode::Headless)
            .isolate_editor_paths(home)
            .query_timeout(Duration::from_secs(5));
        let mut startup = StartingEditor(Some(GodotSession::launch(options).unwrap()));
        let session = startup.0.as_mut().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match session.connect_once() {
                Ok(_) => break,
                Err(SessionError::Disconnected | SessionError::Timeout)
                    if Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(20))
                }
                Err(error) => panic!("test editor did not become ready: {error}"),
            }
        }
        Self {
            root: root_guard.0.take().unwrap(),
            project,
            session: Some(Arc::new(Mutex::new(startup.0.take()))),
        }
    }
    fn session(&self) -> Arc<Mutex<Option<GodotSession>>> {
        self.session.as_ref().unwrap().clone()
    }
}
impl Drop for TestEditor {
    fn drop(&mut self) {
        if let Some(handle) = self.session.take() {
            if let Some(session) = handle.lock().ok().and_then(|mut guard| guard.take()) {
                let mut child = session.into_editor_process();
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn bound(
    editor: &TestEditor,
) -> (
    Arc<Mutex<EngineRegistry>>,
    ConnectionToken,
    EditorSelection,
    Arc<GodotToolBinding>,
) {
    let mut registry = EngineRegistry::default();
    let project = ProjectIdentity::new(EngineKind::Godot, &editor.project).unwrap();
    let id = editor
        .session()
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .instance_id()
        .to_owned();
    let token = registry.open_connection(EngineKind::Godot, id).unwrap();
    registry.identify(&token, project.clone(), false).unwrap();
    let selection = registry.select_exact(&token, &project, false).unwrap();
    let registry = Arc::new(Mutex::new(registry));
    let binding =
        GodotToolBinding::new(registry.clone(), selection.clone(), editor.session()).unwrap();
    (registry, token, selection, binding)
}

#[tokio::test]
async fn scene_tool_preserves_real_peer_truncation_and_node_data() {
    let editor = TestEditor::new();
    fs::write(editor.project.join(".fixture-truncated"), b"").unwrap();
    let (_registry, _token, _selection, binding) = bound(&editor);
    let result = binding
        .scene_tree_tool()
        .execute(binding.target(), json!({}))
        .await
        .unwrap();
    assert_eq!(result["scene_path"], "res://main.tscn");
    assert_eq!(
        result["nodes"][0],
        json!({"path":".","name":"World","class":"Node2D"})
    );
    assert_eq!(result["truncated"], true);
}

#[tokio::test]
async fn rejects_other_engine_project_instance_generation_and_extra_arguments_without_dispatch() {
    use uebox_agent_core::{Engine, RunTarget};
    let editor = TestEditor::new();
    let (_, _, _, binding) = bound(&editor);
    let fixed = binding.target();
    let wrong = [
        RunTarget::new(
            Engine::Unity,
            fixed.project(),
            fixed.instance(),
            fixed.generation(),
        )
        .unwrap(),
        RunTarget::new(
            Engine::Godot,
            "other-project",
            fixed.instance(),
            fixed.generation(),
        )
        .unwrap(),
        RunTarget::new(
            Engine::Godot,
            fixed.project(),
            "other-instance",
            fixed.generation(),
        )
        .unwrap(),
        RunTarget::new(
            Engine::Godot,
            fixed.project(),
            fixed.instance(),
            fixed.generation() + 1,
        )
        .unwrap(),
    ];
    let before = fs::read_to_string(editor.project.join(".fixture-connections")).unwrap();
    for target in wrong {
        assert!(binding
            .project_info_tool()
            .execute(target, json!({}))
            .await
            .is_err());
    }
    assert!(binding
        .scene_tree_tool()
        .execute(fixed, json!({"project":"other"}))
        .await
        .is_err());
    assert_eq!(
        fs::read_to_string(editor.project.join(".fixture-connections")).unwrap(),
        before
    );
}

#[tokio::test]
async fn stale_generation_is_rejected_even_if_the_same_instance_is_registered_again() {
    let editor = TestEditor::new();
    let (registry, token, selection, binding) = bound(&editor);
    let before = fs::read_to_string(editor.project.join(".fixture-connections")).unwrap();
    {
        let mut registry = registry.lock().unwrap();
        assert!(registry.close_connection(&token));
        let replacement = registry
            .open_connection(EngineKind::Godot, selection.instance_id())
            .unwrap();
        registry
            .identify(&replacement, selection.project().clone(), false)
            .unwrap();
        assert_ne!(replacement.generation(), selection.generation());
    }
    assert!(!(binding.validator())(&binding.target()));
    assert!(binding
        .project_info_tool()
        .execute(binding.target(), json!({}))
        .await
        .is_err());
    assert_eq!(
        fs::read_to_string(editor.project.join(".fixture-connections")).unwrap(),
        before
    );
}

#[test]
fn binding_rejects_another_session_and_a_non_godot_selection() {
    use uebox_agent_godot_tools::BindError;
    let first = TestEditor::new();
    let second = TestEditor::new();
    let (registry, _token, selection, _binding) = bound(&first);
    assert!(matches!(
        GodotToolBinding::new(registry.clone(), selection, second.session()),
        Err(BindError::SessionMismatch)
    ));
    let mut wrong = EngineRegistry::default();
    let project = ProjectIdentity::new(EngineKind::Unity, &first.project).unwrap();
    let id = first
        .session()
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .instance_id()
        .to_owned();
    let token = wrong.open_connection(EngineKind::Unity, id).unwrap();
    wrong.identify(&token, project.clone(), false).unwrap();
    let selection = wrong.select_exact(&token, &project, false).unwrap();
    assert!(matches!(
        GodotToolBinding::new(Arc::new(Mutex::new(wrong)), selection, first.session()),
        Err(BindError::WrongEngine)
    ));
}

#[tokio::test]
async fn replacing_the_shared_session_cannot_redirect_a_bound_tool() {
    struct OwnChild(std::process::Child);
    impl Drop for OwnChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let first = TestEditor::new();
    let mut second = TestEditor::new();
    let (_registry, _token, _selection, binding) = bound(&first);
    let second_before = fs::read_to_string(second.project.join(".fixture-connections")).unwrap();
    let second_session = match Arc::try_unwrap(second.session.take().unwrap()) {
        Ok(mutex) => mutex.into_inner().ok().unwrap().unwrap(),
        Err(_) => panic!("fixture session unexpectedly shared"),
    };
    let first_shared = first.session();
    let original = first_shared
        .lock()
        .unwrap()
        .replace(second_session)
        .unwrap();
    let _original_child = OwnChild(original.into_editor_process());
    assert!(binding
        .project_info_tool()
        .execute(binding.target(), json!({}))
        .await
        .is_err());
    assert_eq!(
        fs::read_to_string(second.project.join(".fixture-connections")).unwrap(),
        second_before
    );
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> serde_json::Value {
    use tokio::io::AsyncReadExt;
    let mut data = Vec::new();
    let mut buffer = [0u8; 4096];
    let header_end = loop {
        let count = stream.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        data.extend_from_slice(&buffer[..count]);
        if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = std::str::from_utf8(&data[..header_end]).unwrap();
    let length = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    while data.len() < header_end + length {
        let count = stream.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        data.extend_from_slice(&buffer[..count]);
    }
    serde_json::from_slice(&data[header_end..header_end + length]).unwrap()
}
async fn respond_sse(stream: &mut tokio::net::TcpStream, content: &str) {
    use tokio::io::AsyncWriteExt;
    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", content.len(), content).as_bytes()).await.unwrap();
}
fn tool_call_stream(calls: &[(&str, &str)]) -> String {
    let calls: Vec<_> = calls.iter().enumerate().map(|(index, (id, name))| json!({"index":index,"id":id,"type":"function","function":{"name":name,"arguments":"{}"}})).collect();
    let message =
        json!({"choices":[{"index":0,"delta":{"tool_calls":calls},"finish_reason":"tool_calls"}]});
    format!("data: {message}\n\ndata: [DONE]\n\n")
}
fn final_stream() -> &'static str {
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"complete\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
}
async fn wait_file(path: &std::path::Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "fixture did not reach expected state"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_peer_receipts_reach_the_next_simulated_model_turn() {
    use uebox_agent_core::{AgentSession, OpenAiProvider, ProviderConfig};
    let editor = TestEditor::new();
    fs::write(editor.project.join(".fixture-truncated"), b"").unwrap();
    fs::write(editor.project.join(".fixture-legitimate-hash"), b"").unwrap();
    let (_registry, _token, _selection, binding) = bound(&editor);
    let public_target = binding.target();
    let private_path = editor.project.to_string_lossy().into_owned();
    let raw_instance = editor
        .session()
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .instance_id()
        .to_owned();
    let audit_project = editor.project.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        let initial = read_http_request(&mut first).await;
        let declarations = initial["tools"].as_array().unwrap();
        assert_eq!(declarations.len(), 2);
        assert_eq!(declarations[0]["function"]["name"], "godot_project_info");
        assert_eq!(declarations[1]["function"]["name"], "godot_scene_tree");
        respond_sse(
            &mut first,
            &tool_call_stream(&[
                ("project-call", "godot_project_info"),
                ("scene-call", "godot_scene_tree"),
            ]),
        )
        .await;
        let (mut second, _) = listener.accept().await.unwrap();
        let followup = read_http_request(&mut second).await;
        let messages = followup["messages"].as_array().unwrap();
        assert_eq!(messages[2]["tool_call_id"], "project-call");
        assert_eq!(messages[3]["tool_call_id"], "scene-call");
        let project: serde_json::Value =
            serde_json::from_str(messages[2]["content"].as_str().unwrap()).unwrap();
        let scene: serde_json::Value =
            serde_json::from_str(messages[3]["content"].as_str().unwrap()).unwrap();
        assert_eq!(project["ok"], true);
        assert_eq!(
            project["data"]["name"],
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert_eq!(scene["ok"], true);
        assert_eq!(scene["data"]["scene_path"], "res://main.tscn");
        assert_eq!(
            scene["data"]["nodes"][0]["name"],
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert_eq!(scene["data"]["truncated"], true);
        assert_eq!(scene["target"]["engine"], "godot");
        assert_eq!(scene["target"]["project"], public_target.project());
        assert_eq!(scene["target"]["instance"], public_target.instance());
        assert_eq!(scene["target"]["generation"], public_target.generation());
        let text = json!({"initial_request":initial,"next_request":followup,
            "project_receipt":project,"scene_receipt":scene})
        .to_string();
        assert!(!text.contains(&private_path));
        assert!(
            !text.contains(&raw_instance),
            "model request must not include the raw bridge instance id"
        );
        assert!(!text.contains("UEBOX_GODOT_TOKEN"));
        assert!(!text.contains("UEBOX_TEST_UNTRUSTED_MSG"));
        fs::write(audit_project.join(".fixture-audit-request"), &text).unwrap();
        wait_file(&audit_project.join(".fixture-audit-result")).await;
        assert_eq!(
            fs::read_to_string(audit_project.join(".fixture-audit-result")).unwrap(),
            "clear"
        );
        respond_sse(&mut second, final_stream()).await;
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "fixture-model",
    ))
    .unwrap();
    let agent = AgentSession::new(
        provider,
        binding.tool_registry().unwrap(),
        binding.validator(),
        3,
    )
    .unwrap();
    assert_eq!(
        agent
            .start("inspect")
            .unwrap()
            .execute(|_| {})
            .await
            .unwrap()
            .text,
        "complete"
    );
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_values_in_peer_receipts_become_generic_errors_for_the_model() {
    use uebox_agent_core::{AgentSession, OpenAiProvider, ProviderConfig};
    let editor = TestEditor::new();
    fs::write(editor.project.join(".fixture-leak-instance"), b"").unwrap();
    fs::write(editor.project.join(".fixture-scene-leak-token"), b"").unwrap();
    let (_registry, _token, _selection, binding) = bound(&editor);
    let audit_project = editor.project.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        let initial = read_http_request(&mut first).await;
        respond_sse(
            &mut first,
            &tool_call_stream(&[
                ("project-call", "godot_project_info"),
                ("scene-call", "godot_scene_tree"),
            ]),
        )
        .await;
        let (mut second, _) = listener.accept().await.unwrap();
        let followup = read_http_request(&mut second).await;
        for message in &followup["messages"].as_array().unwrap()[2..4] {
            let result: serde_json::Value =
                serde_json::from_str(message["content"].as_str().unwrap()).unwrap();
            assert_eq!(result["ok"], false);
            assert_eq!(result["error"], "tool_failed");
            assert!(result.get("data").is_none());
        }
        let text = json!({"initial_request": initial, "next_request": followup}).to_string();
        fs::write(audit_project.join(".fixture-audit-request"), text).unwrap();
        wait_file(&audit_project.join(".fixture-audit-result")).await;
        assert_eq!(
            fs::read_to_string(audit_project.join(".fixture-audit-result")).unwrap(),
            "clear"
        );
        respond_sse(&mut second, final_stream()).await;
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "fixture-model",
    ))
    .unwrap();
    let agent = AgentSession::new(
        provider,
        binding.tool_registry().unwrap(),
        binding.validator(),
        3,
    )
    .unwrap();
    assert_eq!(
        agent
            .start("inspect")
            .unwrap()
            .execute(|_| {})
            .await
            .unwrap()
            .text,
        "complete"
    );
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_failure_reaches_the_next_model_turn_as_error_without_diagnostics() {
    use uebox_agent_core::{AgentSession, OpenAiProvider, ProviderConfig};
    let editor = TestEditor::new();
    fs::write(editor.project.join(".fixture-malicious-error"), b"").unwrap();
    let (_registry, _token, _selection, binding) = bound(&editor);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        read_http_request(&mut first).await;
        respond_sse(
            &mut first,
            &tool_call_stream(&[("failed-call", "godot_project_info")]),
        )
        .await;
        let (mut second, _) = listener.accept().await.unwrap();
        let followup = read_http_request(&mut second).await;
        let result: serde_json::Value =
            serde_json::from_str(followup["messages"][2]["content"].as_str().unwrap()).unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"], "tool_failed");
        assert!(result.get("data").is_none());
        assert!(!followup.to_string().contains("UEBOX_TEST_UNTRUSTED_MSG"));
        respond_sse(&mut second, final_stream()).await;
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "fixture-model",
    ))
    .unwrap();
    let agent = AgentSession::new(
        provider,
        binding.tool_registry().unwrap(),
        binding.validator(),
        3,
    )
    .unwrap();
    assert_eq!(
        agent
            .start("inspect")
            .unwrap()
            .execute(|_| {})
            .await
            .unwrap()
            .text,
        "complete"
    );
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_keeps_the_original_session_busy_until_the_tcp_read_finishes() {
    use uebox_agent_core::{AgentSession, Error, OpenAiProvider, ProviderConfig, RunPhase};
    let editor = TestEditor::new();
    fs::write(editor.project.join(".fixture-hold"), b"").unwrap();
    let (_registry, _token, _selection, binding) = bound(&editor);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_request(&mut stream).await;
        respond_sse(
            &mut stream,
            &tool_call_stream(&[("held", "godot_project_info")]),
        )
        .await;
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "fixture-model",
    ))
    .unwrap();
    let agent = AgentSession::new(
        provider,
        binding.tool_registry().unwrap(),
        binding.validator(),
        3,
    )
    .unwrap();
    let run = agent.start("inspect").unwrap();
    let cancel = run.cancellation();
    let task = tokio::spawn(run.execute(|_| {}));
    wait_file(&editor.project.join(".fixture-inflight")).await;
    assert!(editor.session().try_lock().is_err());
    cancel.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        Error::Cancelled
    );
    assert_eq!(agent.snapshot().phase, Some(RunPhase::Draining));
    assert!(matches!(agent.start("another"), Err(Error::Busy)));
    assert!(editor.session().try_lock().is_err());
    let queued = tokio::spawn(
        binding
            .project_info_tool()
            .execute(binding.target(), json!({})),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !queued.is_finished(),
        "second query must not bypass the running read"
    );
    assert_eq!(
        fs::read_to_string(editor.project.join(".fixture-connections")).unwrap(),
        "2"
    );
    fs::write(editor.project.join(".fixture-release"), b"").unwrap();
    let queued_result = tokio::time::timeout(Duration::from_secs(3), queued)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(queued_result["name"], "Fixture project");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while agent.snapshot().phase.is_some() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "cancelled tool did not drain"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(editor.session().try_lock().is_ok());
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_selection_invalidated_during_the_read_cannot_return_stale_data() {
    let editor = TestEditor::new();
    fs::write(editor.project.join(".fixture-hold"), b"").unwrap();
    let (registry, token, _selection, binding) = bound(&editor);
    let query = tokio::spawn(
        binding
            .scene_tree_tool()
            .execute(binding.target(), json!({})),
    );
    wait_file(&editor.project.join(".fixture-inflight")).await;
    assert!(registry.lock().unwrap().close_connection(&token));
    fs::write(editor.project.join(".fixture-release"), b"").unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(3), query)
        .await
        .unwrap()
        .unwrap()
        .is_err());
}

struct ReleasableCleanup {
    handle: Arc<Mutex<Option<GodotSession>>>,
    child: Option<std::process::Child>,
}
impl Drop for ReleasableCleanup {
    fn drop(&mut self) {
        let session = self.handle.lock().ok().and_then(|mut guard| guard.take());
        let child = self
            .child
            .take()
            .or_else(|| session.map(GodotSession::into_editor_process));
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn into_releasable(editor: &mut TestEditor) -> ReleasableCleanup {
    ReleasableCleanup {
        handle: editor.session.take().unwrap(),
        child: None,
    }
}
fn releasable_binding(
    editor: &TestEditor,
    handle: Arc<Mutex<Option<GodotSession>>>,
) -> (
    Arc<Mutex<EngineRegistry>>,
    ConnectionToken,
    EditorSelection,
    Arc<GodotToolBinding>,
) {
    let mut registry = EngineRegistry::default();
    let project = ProjectIdentity::new(EngineKind::Godot, &editor.project).unwrap();
    let id = handle
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .instance_id()
        .to_owned();
    let token = registry.open_connection(EngineKind::Godot, id).unwrap();
    registry.identify(&token, project.clone(), false).unwrap();
    let selection = registry.select_exact(&token, &project, false).unwrap();
    let registry = Arc::new(Mutex::new(registry));
    let binding = GodotToolBinding::new(registry.clone(), selection.clone(), handle).unwrap();
    (registry, token, selection, binding)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn taking_the_host_session_disables_old_tools_without_reconnecting() {
    let mut editor = TestEditor::new();
    let mut cleanup = into_releasable(&mut editor);
    let (registry, token, selection, binding) = releasable_binding(&editor, cleanup.handle.clone());
    assert_eq!(
        binding
            .project_info_tool()
            .execute(binding.target(), json!({}))
            .await
            .unwrap()["name"],
        "Fixture project"
    );
    let before = fs::read_to_string(editor.project.join(".fixture-connections")).unwrap();
    let released = cleanup.handle.lock().unwrap().take().unwrap();
    cleanup.child = Some(released.into_editor_process());
    assert!(!(binding.validator())(&binding.target()));
    assert!(binding
        .project_info_tool()
        .execute(binding.target(), json!({}))
        .await
        .is_err());
    assert_eq!(
        fs::read_to_string(editor.project.join(".fixture-connections")).unwrap(),
        before
    );
    // Releasing the session is not equivalent to retiring the registry token.
    assert!(registry
        .lock()
        .unwrap()
        .resolve_selection(&selection)
        .is_ok());
    assert!(registry.lock().unwrap().close_connection(&token));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn release_waits_until_the_actual_inflight_read_finishes() {
    let mut editor = TestEditor::new();
    let mut cleanup = into_releasable(&mut editor);
    fs::write(editor.project.join(".fixture-hold"), b"").unwrap();
    let (_registry, _token, _selection, binding) =
        releasable_binding(&editor, cleanup.handle.clone());
    let query = tokio::spawn(
        binding
            .scene_tree_tool()
            .execute(binding.target(), json!({})),
    );
    wait_file(&editor.project.join(".fixture-inflight")).await;
    let handle = cleanup.handle.clone();
    let mut release = tokio::task::spawn_blocking(move || handle.lock().unwrap().take());
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut release)
            .await
            .is_err(),
        "release must wait for the real read"
    );
    fs::write(editor.project.join(".fixture-release"), b"").unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(3), query)
        .await
        .unwrap()
        .unwrap()
        .is_ok());
    let released = tokio::time::timeout(Duration::from_secs(3), release)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    cleanup.child = Some(released.into_editor_process());
    assert!(binding
        .scene_tree_tool()
        .execute(binding.target(), json!({}))
        .await
        .is_err());
}

#[test]
fn public_aliases_are_process_unique_and_stable_for_the_same_selection() {
    let first = TestEditor::new();
    let second = TestEditor::new();
    let (registry, _token, selection, binding) = bound(&first);
    let another_binding = GodotToolBinding::new(registry, selection, first.session()).unwrap();
    let (_, _, _, other) = bound(&second);
    let raw = first
        .session()
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .instance_id()
        .to_owned();
    assert_ne!(binding.target().instance(), raw);
    assert_eq!(
        binding.target().instance(),
        another_binding.target().instance()
    );
    assert_ne!(binding.target().instance(), other.target().instance());
}

#[tokio::test]
async fn refuses_receipts_that_embed_the_actual_instance_or_token() {
    let editor = TestEditor::new();
    let (_, _, _, binding) = bound(&editor);
    for marker in [".fixture-leak-instance", ".fixture-leak-token"] {
        fs::write(editor.project.join(marker), b"").unwrap();
        let result = binding
            .project_info_tool()
            .execute(binding.target(), json!({}))
            .await;
        assert!(
            result.is_err(),
            "sensitive editor data must not become a model receipt"
        );
        fs::remove_file(editor.project.join(marker)).unwrap();
    }
    for marker in [".fixture-scene-leak-instance", ".fixture-scene-leak-token"] {
        fs::write(editor.project.join(marker), b"").unwrap();
        assert!(binding
            .scene_tree_tool()
            .execute(binding.target(), json!({}))
            .await
            .is_err());
        fs::remove_file(editor.project.join(marker)).unwrap();
    }
}

#[tokio::test]
async fn unrelated_64_character_hash_is_returned_exactly_in_project_and_scene() {
    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let editor = TestEditor::new();
    fs::write(editor.project.join(".fixture-legitimate-hash"), b"").unwrap();
    fs::write(editor.project.join(".fixture-truncated"), b"").unwrap();
    let (_, _, _, binding) = bound(&editor);
    let project = binding
        .project_info_tool()
        .execute(binding.target(), json!({}))
        .await
        .unwrap();
    let scene = binding
        .scene_tree_tool()
        .execute(binding.target(), json!({}))
        .await
        .unwrap();
    assert_eq!(project["name"], HASH);
    assert_eq!(scene["nodes"][0]["name"], HASH);
    assert_eq!(scene["truncated"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn release_waits_for_a_cancelled_tool_future_to_finish_its_real_read() {
    let mut editor = TestEditor::new();
    let mut cleanup = into_releasable(&mut editor);
    fs::write(editor.project.join(".fixture-hold"), b"").unwrap();
    let (_registry, _token, _selection, binding) =
        releasable_binding(&editor, cleanup.handle.clone());
    let query = tokio::spawn(
        binding
            .scene_tree_tool()
            .execute(binding.target(), json!({})),
    );
    wait_file(&editor.project.join(".fixture-inflight")).await;
    query.abort();
    assert!(query.await.unwrap_err().is_cancelled());
    let handle = cleanup.handle.clone();
    let mut release = tokio::task::spawn_blocking(move || handle.lock().unwrap().take());
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut release)
            .await
            .is_err()
    );
    fs::write(editor.project.join(".fixture-release"), b"").unwrap();
    let released = tokio::time::timeout(Duration::from_secs(3), release)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    cleanup.child = Some(released.into_editor_process());
    assert!(binding
        .scene_tree_tool()
        .execute(binding.target(), json!({}))
        .await
        .is_err());
}

#[tokio::test]
async fn disconnected_session_is_not_reconnected_or_rebound() {
    use uebox_agent_godot_tools::BindError;
    let editor = TestEditor::new();
    let (registry, _token, selection, binding) = bound(&editor);
    fs::write(editor.project.join(".fixture-disconnect"), b"").unwrap();
    assert!(binding
        .project_info_tool()
        .execute(binding.target(), json!({}))
        .await
        .is_err());
    let after_failure = fs::read_to_string(editor.project.join(".fixture-connections")).unwrap();
    assert!(binding
        .project_info_tool()
        .execute(binding.target(), json!({}))
        .await
        .is_err());
    assert_eq!(
        fs::read_to_string(editor.project.join(".fixture-connections")).unwrap(),
        after_failure
    );
    assert!(matches!(
        GodotToolBinding::new(registry, selection, editor.session()),
        Err(BindError::SessionNotReady)
    ));
}

#[test]
fn same_project_different_editor_instance_cannot_bind_to_an_existing_selection() {
    use uebox_agent_godot_tools::BindError;
    let first = TestEditor::new();
    let options = LaunchOptions::new(
        PathBuf::from(env!("CARGO_BIN_EXE_godot-agent-test-editor")),
        &first.project,
    )
    .acknowledge_legacy_protocol_risks()
    .editor_mode(EditorMode::Headless)
    .isolate_editor_paths(first.root.join("home"))
    .query_timeout(Duration::from_secs(5));
    let mut startup = StartingEditor(Some(GodotSession::launch(options).unwrap()));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match startup.0.as_mut().unwrap().connect_once() {
            Ok(_) => break,
            Err(SessionError::Disconnected | SessionError::Timeout)
                if Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(20))
            }
            Err(error) => panic!("second fixture did not start: {error}"),
        }
    }
    let second = ReleasableCleanup {
        handle: Arc::new(Mutex::new(startup.0.take())),
        child: None,
    };
    let (registry, _token, selection, _binding) = bound(&first);
    assert_eq!(
        second
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .project_path(),
        first.project.as_path()
    );
    assert!(matches!(
        GodotToolBinding::new(registry, selection, second.handle.clone()),
        Err(BindError::SessionMismatch)
    ));
}
