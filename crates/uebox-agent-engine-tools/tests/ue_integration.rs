#![cfg(feature = "unreal")]

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use uebox_agent_core::{AgentSession, Engine, OpenAiProvider, ProviderConfig, RunTarget};
use uebox_agent_engine_tools::unreal::{BindError, UnrealCandidate, UnrealProjectInfoBinding};
use uebox_engine_core::{
    ConnectionToken, EngineKind, EngineRegistry, InteractionMode, ProjectIdentity,
};
use uebox_engine_unreal::{InstanceSummary, UnrealBridge};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn connect_editor(bridge: &UnrealBridge, name: &str, project_path: &str) -> Ws {
    let (mut socket, _) = connect_async(format!("ws://{}", bridge.local_addr()))
        .await
        .unwrap();
    let welcome = socket.next().await.unwrap().unwrap();
    assert!(matches!(welcome, Message::Text(_)));
    socket.send(Message::Text(json!({"ver":"1.0","type":"evt","method":"project.info","payload":{"projectName":name,"projectPath":project_path,"interactive":true}}).to_string().into())).await.unwrap();
    socket
}

async fn wait_summary(bridge: &UnrealBridge, name: &str) -> InstanceSummary {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(summary) = bridge
                .instances()
                .into_iter()
                .find(|item| item.project_name == name)
            {
                return summary;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

async fn editor_request(socket: &mut Ws) -> Value {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Message::Text(text) = socket.next().await.unwrap().unwrap() {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["type"] == "req" {
                    return value;
                }
            }
        }
    })
    .await
    .unwrap()
}

async fn http_request(stream: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let mut buf = [0; 4096];
    let end = loop {
        let n = stream.read(&mut buf).await.unwrap();
        assert_ne!(n, 0);
        bytes.extend_from_slice(&buf[..n]);
        if let Some(pos) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
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

async fn http_response(stream: &mut TcpStream, body: &str) {
    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
}

struct Fixture {
    _projects: tempfile::TempDir,
    path: String,
    project: ProjectIdentity,
    bridge: Arc<UnrealBridge>,
    editor: Ws,
    registry: Arc<Mutex<EngineRegistry>>,
    token: ConnectionToken,
    summary: InstanceSummary,
    candidate: UnrealCandidate,
    binding: UnrealProjectInfoBinding,
}

async fn fixture() -> Fixture {
    let projects = tempfile::tempdir().unwrap();
    let project = ProjectIdentity::new(EngineKind::Unreal, projects.path()).unwrap();
    let path = project.root().to_str().unwrap().to_owned();
    let bridge = Arc::new(UnrealBridge::bind(0).await.unwrap());
    let editor = connect_editor(&bridge, "A", &path).await;
    let summary = wait_summary(&bridge, "A").await;
    let registry = Arc::new(Mutex::new(EngineRegistry::default()));
    let candidate = UnrealCandidate::capture(
        bridge.clone(),
        registry.clone(),
        project.clone(),
        &summary,
        InteractionMode::Interactive,
    )
    .unwrap();
    let token = candidate.token().clone();
    let binding =
        UnrealProjectInfoBinding::bind_exact(&candidate, &token, Duration::from_secs(1)).unwrap();
    Fixture {
        _projects: projects,
        path,
        project,
        bridge,
        editor,
        registry,
        token,
        summary,
        candidate,
        binding,
    }
}

#[tokio::test]
async fn chosen_ue_connection_supplies_real_project_info_to_the_next_model_step() {
    let projects = tempfile::tempdir().unwrap();
    let dir_a = projects.path().join("a");
    let dir_b = projects.path().join("b");
    std::fs::create_dir(&dir_a).unwrap();
    std::fs::create_dir(&dir_b).unwrap();
    let project_a = ProjectIdentity::new(EngineKind::Unreal, &dir_a).unwrap();
    let project_b = ProjectIdentity::new(EngineKind::Unreal, &dir_b).unwrap();
    let path_a = project_a.root().to_str().unwrap().to_owned();
    let path_b = project_b.root().to_str().unwrap().to_owned();
    let bridge = Arc::new(UnrealBridge::bind(0).await.unwrap());
    let mut editor_a = connect_editor(&bridge, "A", &path_a).await;
    let mut editor_b = connect_editor(&bridge, "B", &path_b).await;
    let summary_a = wait_summary(&bridge, "A").await;
    let summary_b = wait_summary(&bridge, "B").await;
    let registry = Arc::new(Mutex::new(EngineRegistry::default()));
    let candidate_a = UnrealCandidate::capture(
        bridge.clone(),
        registry.clone(),
        project_a,
        &summary_a,
        InteractionMode::Interactive,
    )
    .unwrap();
    let candidate_b = UnrealCandidate::capture(
        bridge.clone(),
        registry.clone(),
        project_b.clone(),
        &summary_b,
        InteractionMode::Interactive,
    )
    .unwrap();
    let binding = UnrealProjectInfoBinding::bind_exact(
        &candidate_a,
        candidate_a.token(),
        Duration::from_secs(2),
    )
    .unwrap();
    let _later_ui_selection = registry
        .lock()
        .unwrap()
        .select_exact(candidate_b.token(), &project_b, false)
        .unwrap();
    let target = binding.target().clone();
    let model = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = model.local_addr().unwrap();
    let expected_path = path_a.clone();
    let ue_selection_credential = summary_a.connection_id.clone();
    let server = tokio::spawn(async move {
        let (mut first, _) = model.accept().await.unwrap();
        let body = http_request(&mut first).await;
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "ue_project_info");
        assert_eq!(
            tools[0]["function"]["parameters"],
            json!({"type":"object","properties":{},"additionalProperties":false})
        );
        http_response(&mut first, concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"ue_project_info\",\"arguments\":\"{\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n"
        )).await;
        let (mut second, _) = model.accept().await.unwrap();
        let body = http_request(&mut second).await;
        let result: Value =
            serde_json::from_str(body["messages"][2]["content"].as_str().unwrap()).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(
            result["data"],
            json!({"projectName":"A","engineVersion":"5.4.3","hasCode":true})
        );
        assert_eq!(result["target"]["engine"], "unreal");
        assert_eq!(result["target"]["project"], target.project());
        assert_ne!(result["target"]["project"], expected_path);
        assert_eq!(result["target"]["instance"], target.instance());
        assert!(result["target"]["instance"].as_str().unwrap() != ue_selection_credential);
        assert_eq!(result["target"]["generation"], target.generation());
        assert!(!body.to_string().contains(&expected_path));
        assert!(!body.to_string().contains("private-engine-path"));
        http_response(&mut second, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"UE 5.4.3\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "test-model",
    ))
    .unwrap();
    let session = AgentSession::new(
        provider,
        binding.tools().unwrap(),
        binding.target_validator(),
        3,
    )
    .unwrap();
    let run = tokio::spawn(session.start("version?").unwrap().execute(|_| {}));
    let request = editor_request(&mut editor_a).await;
    assert_eq!(request["method"], "project.info");
    assert_eq!(request["params"], json!({}));
    assert!(
        tokio::time::timeout(Duration::from_millis(80), editor_b.next())
            .await
            .is_err()
    );
    editor_a.send(Message::Text(json!({"ver":"1.0","type":"res","id":request["id"],"code":200,"result":{"projectName":"A","projectPath":path_a,"engineVersion":"5.4.3","hasCode":true,"engineDir":"/private-engine-path","fixtureValue":17}}).to_string().into())).await.unwrap();
    assert_eq!(run.await.unwrap().unwrap().text, "UE 5.4.3");
    server.await.unwrap();
}

#[tokio::test]
async fn bind_rejects_project_engine_and_deadline_mismatches() {
    let projects = tempfile::tempdir().unwrap();
    let dir_a = projects.path().join("a");
    let dir_b = projects.path().join("b");
    std::fs::create_dir(&dir_a).unwrap();
    std::fs::create_dir(&dir_b).unwrap();
    let project_a = ProjectIdentity::new(EngineKind::Unreal, &dir_a).unwrap();
    let project_b = ProjectIdentity::new(EngineKind::Unreal, &dir_b).unwrap();
    let unity_project = ProjectIdentity::new(EngineKind::Unity, &dir_a).unwrap();
    let bridge = Arc::new(UnrealBridge::bind(0).await.unwrap());
    let _editor = connect_editor(&bridge, "A", project_a.root().to_str().unwrap()).await;
    let summary = wait_summary(&bridge, "A").await;

    let wrong_project_registry = Arc::new(Mutex::new(EngineRegistry::default()));
    assert!(UnrealCandidate::capture(
        bridge.clone(),
        wrong_project_registry,
        project_b,
        &summary,
        InteractionMode::Interactive
    )
    .is_err());

    let wrong_engine_registry = Arc::new(Mutex::new(EngineRegistry::default()));
    assert!(UnrealCandidate::capture(
        bridge.clone(),
        wrong_engine_registry,
        unity_project,
        &summary,
        InteractionMode::Interactive
    )
    .is_err());

    let registry = Arc::new(Mutex::new(EngineRegistry::default()));
    let candidate = UnrealCandidate::capture(
        bridge,
        registry,
        project_a,
        &summary,
        InteractionMode::Interactive,
    )
    .unwrap();
    assert!(
        UnrealProjectInfoBinding::bind_exact(&candidate, candidate.token(), Duration::ZERO)
            .is_err()
    );
}

#[tokio::test]
async fn stale_registry_generation_and_mismatched_run_targets_never_dispatch() {
    let mut fixture = fixture().await;
    let original = fixture.binding.target().clone();
    let candidates = [
        RunTarget::new(
            Engine::Unity,
            original.project(),
            original.instance(),
            original.generation(),
        )
        .unwrap(),
        RunTarget::new(
            Engine::Unreal,
            "/different-project",
            original.instance(),
            original.generation(),
        )
        .unwrap(),
        RunTarget::new(
            Engine::Unreal,
            original.project(),
            "different-instance",
            original.generation(),
        )
        .unwrap(),
        RunTarget::new(
            Engine::Unreal,
            original.project(),
            original.instance(),
            original.generation() + 1,
        )
        .unwrap(),
    ];
    for target in candidates {
        assert!(!(fixture.binding.target_validator())(&target));
        assert!(fixture
            .binding
            .tool()
            .execute(target, json!({}))
            .await
            .is_err());
    }
    {
        let mut registry = fixture.registry.lock().unwrap();
        assert!(registry.close_connection(&fixture.token));
        let replacement = registry
            .open_connection(EngineKind::Unreal, original.instance())
            .unwrap();
        registry
            .identify(&replacement, fixture.project.clone(), true)
            .unwrap();
    }
    assert!(!(fixture.binding.target_validator())(&original));
    assert!(matches!(
        fixture.binding.tools(),
        Err(uebox_agent_core::Error::TargetChanged)
    ));
    assert!(tokio::time::timeout(
        Duration::from_millis(250),
        fixture.binding.tool().execute(original, json!({}))
    )
    .await
    .unwrap()
    .is_err());
    assert!(
        tokio::time::timeout(Duration::from_millis(80), fixture.editor.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn project_change_on_selected_socket_fails_inflight_and_invalidates_binding() {
    let mut fixture = fixture().await;
    let next_dir = fixture._projects.path().join("other");
    std::fs::create_dir(&next_dir).unwrap();
    let target = fixture.binding.target().clone();
    let tool = fixture.binding.tool();
    let pending = tokio::spawn(tool.execute(target.clone(), json!({})));
    let request = editor_request(&mut fixture.editor).await;
    assert_eq!(request["method"], "project.info");
    fixture.editor.send(Message::Text(json!({"type":"evt","method":"project.info","payload":{"projectName":"B","projectPath":next_dir.to_str().unwrap(),"interactive":true}}).to_string().into())).await.unwrap();
    let changed = wait_summary(&fixture.bridge, "B").await;
    assert_ne!(changed.connection_id, fixture.summary.connection_id);
    assert!(!pending.is_finished());
    fixture.editor.send(Message::Text(json!({"type":"res","id":request["id"],"code":200,"result":{"projectName":"A","projectPath":fixture.path}}).to_string().into())).await.unwrap();
    assert!(pending.await.unwrap().is_err());
    assert!(fixture
        .binding
        .tool()
        .execute(target.clone(), json!({}))
        .await
        .is_err());
    assert!(fixture
        .registry
        .lock()
        .unwrap()
        .close_connection(&fixture.token));
    assert!(!(fixture.binding.target_validator())(&target));
    assert!(
        tokio::time::timeout(Duration::from_millis(80), fixture.editor.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn project_info_response_must_match_the_captured_project_identity() {
    let mut fixture = fixture().await;
    let other = fixture._projects.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let target = fixture.binding.target().clone();
    let invalid_results = [
        json!({"projectName":"A","projectPath":other.to_str().unwrap()}),
        json!({"projectName":"B","projectPath":fixture.path}),
        json!({"projectName":"A","projectPath":"relative/path"}),
        json!({"projectName":"A"}),
        json!({"projectPath":fixture.path}),
    ];
    for result in invalid_results {
        let pending = tokio::spawn(fixture.binding.tool().execute(target.clone(), json!({})));
        let request = editor_request(&mut fixture.editor).await;
        fixture
            .editor
            .send(Message::Text(
                json!({"type":"res","id":request["id"],"code":200,"result":result})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        assert!(pending.await.unwrap().is_err());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn capture_rejects_untrusted_symlink_path_even_if_it_resolves_to_selected_project() {
    let projects = tempfile::tempdir().unwrap();
    let directory = projects.path().join("project");
    std::fs::create_dir(&directory).unwrap();
    let project = ProjectIdentity::new(EngineKind::Unreal, &directory).unwrap();
    let alias = projects.path().join("untrusted-alias");
    std::os::unix::fs::symlink(project.root(), &alias).unwrap();
    let bridge = Arc::new(UnrealBridge::bind(0).await.unwrap());
    let _editor = connect_editor(&bridge, "A", alias.to_str().unwrap()).await;
    let summary = wait_summary(&bridge, "A").await;
    let registry = Arc::new(Mutex::new(EngineRegistry::default()));
    assert!(matches!(
        UnrealCandidate::capture(
            bridge,
            registry,
            project,
            &summary,
            InteractionMode::Interactive,
        ),
        Err(BindError::ProjectMismatch)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn response_rejects_untrusted_symlink_path_even_if_it_resolves_to_selected_project() {
    let mut fixture = fixture().await;
    let alias = fixture._projects.path().join("untrusted-alias");
    std::os::unix::fs::symlink(fixture.project.root(), &alias).unwrap();
    let pending = tokio::spawn(
        fixture
            .binding
            .tool()
            .execute(fixture.binding.target().clone(), json!({})),
    );
    let request = editor_request(&mut fixture.editor).await;
    fixture.editor.send(Message::Text(json!({"type":"res","id":request["id"],"code":200,"result":{"projectName":"A","projectPath":alias.to_str().unwrap()}}).to_string().into())).await.unwrap();
    assert!(pending.await.unwrap().is_err());
    assert!(
        tokio::time::timeout(Duration::from_millis(80), fixture.editor.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn capture_accepts_authorized_path_without_reopening_removed_directory() {
    let projects = tempfile::tempdir().unwrap();
    let directory = projects.path().join("project");
    std::fs::create_dir(&directory).unwrap();
    let project = ProjectIdentity::new(EngineKind::Unreal, &directory).unwrap();
    let bridge = Arc::new(UnrealBridge::bind(0).await.unwrap());
    let _editor = connect_editor(&bridge, "A", project.root().to_str().unwrap()).await;
    let summary = wait_summary(&bridge, "A").await;
    std::fs::remove_dir(&directory).unwrap();
    let registry = Arc::new(Mutex::new(EngineRegistry::default()));
    assert!(UnrealCandidate::capture(
        bridge,
        registry,
        project,
        &summary,
        InteractionMode::Interactive,
    )
    .is_ok());
}

#[tokio::test]
async fn response_accepts_authorized_path_without_reopening_removed_directory() {
    let mut fixture = fixture().await;
    std::fs::remove_dir(fixture.project.root()).unwrap();
    let pending = tokio::spawn(
        fixture
            .binding
            .tool()
            .execute(fixture.binding.target().clone(), json!({})),
    );
    let request = editor_request(&mut fixture.editor).await;
    fixture.editor.send(Message::Text(json!({"type":"res","id":request["id"],"code":200,"result":{"projectName":"A","projectPath":fixture.path}}).to_string().into())).await.unwrap();
    let response = tokio::time::timeout(Duration::from_secs(1), pending)
        .await
        .expect("identity validation must not wait on a filesystem resolver")
        .unwrap();
    assert!(response.is_ok());
}

#[tokio::test]
async fn authorized_directory_with_trailing_separator_is_accepted_in_announcement_and_response() {
    let projects = tempfile::tempdir().unwrap();
    let project = ProjectIdentity::new(EngineKind::Unreal, projects.path()).unwrap();
    let path = format!(
        "{}{}",
        project.root().to_str().unwrap(),
        std::path::MAIN_SEPARATOR
    );
    let bridge = Arc::new(UnrealBridge::bind(0).await.unwrap());
    let mut editor = connect_editor(&bridge, "A", &path).await;
    let summary = wait_summary(&bridge, "A").await;
    let candidate = UnrealCandidate::capture(
        bridge,
        Arc::new(Mutex::new(EngineRegistry::default())),
        project,
        &summary,
        InteractionMode::Interactive,
    )
    .unwrap();
    let binding =
        UnrealProjectInfoBinding::bind_exact(&candidate, candidate.token(), Duration::from_secs(1))
            .unwrap();
    let pending = tokio::spawn(binding.tool().execute(binding.target().clone(), json!({})));
    let request = editor_request(&mut editor).await;
    editor.send(Message::Text(json!({"type":"res","id":request["id"],"code":200,"result":{"projectName":"A","projectPath":path}}).to_string().into())).await.unwrap();
    assert!(pending.await.unwrap().is_ok());
}

// These strings are deliberately sent through real loopback fixtures only on
// non-Windows, where they are not filesystem paths. The cross-platform Windows
// lexical tests exercise them without ever contacting a share or a device.
#[cfg(not(windows))]
#[tokio::test]
async fn untrusted_unc_and_device_announcements_and_responses_are_rejected_offline() {
    let projects = tempfile::tempdir().unwrap();
    let project = ProjectIdentity::new(EngineKind::Unreal, projects.path()).unwrap();
    let bridge = Arc::new(UnrealBridge::bind(0).await.unwrap());
    let registry = Arc::new(Mutex::new(EngineRegistry::default()));
    let invalid = [
        r"\\example.invalid\share\Project",
        r"\\?\UNC\example.invalid\share\Project",
        r"\\.\C:\Project",
        r"\\?\GLOBALROOT\Device\HarddiskVolume1\Project",
    ];
    for (index, path) in invalid.iter().enumerate() {
        let name = format!("candidate-{index}");
        let _editor = connect_editor(&bridge, &name, path).await;
        let summary = wait_summary(&bridge, &name).await;
        assert!(matches!(
            UnrealCandidate::capture(
                bridge.clone(),
                registry.clone(),
                project.clone(),
                &summary,
                InteractionMode::Interactive,
            ),
            Err(BindError::ProjectMismatch)
        ));
    }

    let mut fixture = fixture().await;
    for path in invalid {
        let pending = tokio::spawn(
            fixture
                .binding
                .tool()
                .execute(fixture.binding.target().clone(), json!({})),
        );
        let request = editor_request(&mut fixture.editor).await;
        fixture.editor.send(Message::Text(json!({"type":"res","id":request["id"],"code":200,"result":{"projectName":"A","projectPath":path}}).to_string().into())).await.unwrap();
        assert!(pending.await.unwrap().is_err());
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(80), fixture.editor.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn disconnected_selected_connection_never_follows_a_replacement() {
    let mut fixture = fixture().await;
    let target = fixture.binding.target().clone();
    let pending = tokio::spawn(fixture.binding.tool().execute(target.clone(), json!({})));
    assert_eq!(
        editor_request(&mut fixture.editor).await["method"],
        "project.info"
    );
    fixture.editor.close(None).await.unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(1), pending)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    tokio::time::timeout(Duration::from_secs(2), async {
        while !fixture.bridge.instances().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut replacement = connect_editor(&fixture.bridge, "A", &fixture.path).await;
    let new_summary = wait_summary(&fixture.bridge, "A").await;
    assert_ne!(new_summary.connection_id, fixture.summary.connection_id);
    assert!(fixture
        .binding
        .tool()
        .execute(target.clone(), json!({}))
        .await
        .is_err());
    assert!(fixture
        .registry
        .lock()
        .unwrap()
        .close_connection(&fixture.token));
    assert!(!(fixture.binding.target_validator())(&target));
    assert!(
        tokio::time::timeout(Duration::from_millis(80), replacement.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn engine_error_is_returned_to_the_model_without_trying_another_editor() {
    let mut fixture = fixture().await;
    let dir_b = fixture._projects.path().join("other");
    std::fs::create_dir(&dir_b).unwrap();
    let mut editor_b = connect_editor(&fixture.bridge, "B", dir_b.to_str().unwrap()).await;
    wait_summary(&fixture.bridge, "B").await;
    let model = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = model.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut first, _) = model.accept().await.unwrap();
        http_request(&mut first).await;
        http_response(&mut first, "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-err\",\"type\":\"function\",\"function\":{\"name\":\"ue_project_info\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n").await;
        let (mut second, _) = model.accept().await.unwrap();
        let body = http_request(&mut second).await;
        let result: Value =
            serde_json::from_str(body["messages"][2]["content"].as_str().unwrap()).unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"], "tool_failed");
        assert_eq!(result["target"]["engine"], "unreal");
        http_response(&mut second, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"query failed\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "test-model",
    ))
    .unwrap();
    let session = AgentSession::new(
        provider,
        fixture.binding.tools().unwrap(),
        fixture.binding.target_validator(),
        3,
    )
    .unwrap();
    let run = tokio::spawn(session.start("inspect").unwrap().execute(|_| {}));
    let request = editor_request(&mut fixture.editor).await;
    fixture.editor.send(Message::Text(json!({"type":"res","id":request["id"],"code":500,"result":{"message":"fixture engine error"}}).to_string().into())).await.unwrap();
    assert_eq!(run.await.unwrap().unwrap().text, "query failed");
    assert!(matches!(
        &session.snapshot().messages[2],
        uebox_agent_core::Message::Tool { is_error: true, .. }
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(80), editor_b.next())
            .await
            .is_err()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn cancellation_discards_late_result_and_never_dispatches_to_another_editor() {
    let mut fixture = fixture().await;
    let dir_b = fixture._projects.path().join("other");
    std::fs::create_dir(&dir_b).unwrap();
    let mut editor_b = connect_editor(&fixture.bridge, "B", dir_b.to_str().unwrap()).await;
    wait_summary(&fixture.bridge, "B").await;
    let model = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = model.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut first, _) = model.accept().await.unwrap();
        http_request(&mut first).await;
        http_response(&mut first, "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-cancel\",\"type\":\"function\",\"function\":{\"name\":\"ue_project_info\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n").await;
        assert!(
            tokio::time::timeout(Duration::from_millis(350), model.accept())
                .await
                .is_err()
        );
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "test-model",
    ))
    .unwrap();
    let session = AgentSession::new(
        provider,
        fixture.binding.tools().unwrap(),
        fixture.binding.target_validator(),
        3,
    )
    .unwrap();
    let run = session.start("inspect").unwrap();
    let cancel = run.cancellation();
    let task = tokio::spawn(run.execute(|_| {}));
    let request = editor_request(&mut fixture.editor).await;
    cancel.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        uebox_agent_core::Error::Cancelled
    );
    assert_eq!(
        session.snapshot().phase,
        Some(uebox_agent_core::RunPhase::Draining)
    );
    fixture.editor.send(Message::Text(json!({"type":"res","id":request["id"],"code":200,"result":{"projectName":"A","projectPath":fixture.path,"engineVersion":"5.5"}}).to_string().into())).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while session.snapshot().phase.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(session.snapshot().messages.len(), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(80), editor_b.next())
            .await
            .is_err()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn model_cannot_supply_a_method_or_invoke_an_unregistered_tool() {
    let mut fixture = fixture().await;
    let target = fixture.binding.target().clone();
    assert!(fixture
        .binding
        .tool()
        .execute(target, json!({"method":"run_shell"}))
        .await
        .is_err());
    let model = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = model.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut first, _) = model.accept().await.unwrap();
        let body = http_request(&mut first).await;
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        let calls = json!([
            {"index":0,"id":"bad-args","type":"function","function":{"name":"ue_project_info","arguments":"{\"method\":\"run_shell\"}"}},
            {"index":1,"id":"bad-tool","type":"function","function":{"name":"run_shell","arguments":"{}"}}
        ]);
        let event = json!({"choices":[{"index":0,"delta":{"tool_calls":calls},"finish_reason":"tool_calls"}]});
        http_response(&mut first, &format!("data: {event}\n\ndata: [DONE]\n\n")).await;
        let (mut second, _) = model.accept().await.unwrap();
        let body = http_request(&mut second).await;
        let bad_args: Value =
            serde_json::from_str(body["messages"][2]["content"].as_str().unwrap()).unwrap();
        let bad_tool: Value =
            serde_json::from_str(body["messages"][3]["content"].as_str().unwrap()).unwrap();
        assert_eq!(bad_args["error"], "invalid_arguments");
        assert_eq!(bad_tool["error"], "tool_not_allowed");
        http_response(&mut second, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"rejected\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "test-model",
    ))
    .unwrap();
    let session = AgentSession::new(
        provider,
        fixture.binding.tools().unwrap(),
        fixture.binding.target_validator(),
        3,
    )
    .unwrap();
    assert_eq!(
        session
            .start("inspect")
            .unwrap()
            .execute(|_| {})
            .await
            .unwrap()
            .text,
        "rejected"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(80), fixture.editor.next())
            .await
            .is_err()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn displayed_candidate_token_cannot_bind_a_replacement_with_the_same_instance_id() {
    let fixture = fixture().await;
    let instance = fixture.binding.target().instance().to_owned();
    let replacement = {
        let mut registry = fixture.registry.lock().unwrap();
        assert!(registry.close_connection(&fixture.token));
        let token = registry
            .open_connection(EngineKind::Unreal, &instance)
            .unwrap();
        registry
            .identify(&token, fixture.project.clone(), true)
            .unwrap();
        token
    };
    assert!(UnrealProjectInfoBinding::bind_exact(
        &fixture.candidate,
        &fixture.token,
        Duration::from_secs(1)
    )
    .is_err());
    assert!(UnrealProjectInfoBinding::bind_exact(
        &fixture.candidate,
        &replacement,
        Duration::from_secs(1)
    )
    .is_err());
    let fresh_candidate = UnrealCandidate::capture(
        fixture.bridge.clone(),
        fixture.registry.clone(),
        fixture.project.clone(),
        &fixture.summary,
        InteractionMode::Interactive,
    )
    .unwrap();
    let fresh = UnrealProjectInfoBinding::bind_exact(
        &fresh_candidate,
        fresh_candidate.token(),
        Duration::from_secs(1),
    )
    .unwrap();
    assert_ne!(fresh.target().instance(), instance);
    assert_eq!(
        fresh.target().generation(),
        fresh_candidate.token().generation()
    );
    assert_ne!(fresh.target().generation(), fixture.token.generation());
}

#[tokio::test]
async fn explicitly_selected_unknown_interaction_mode_can_bind_read_only_query() {
    let fixture = fixture().await;
    let registry = Arc::new(Mutex::new(EngineRegistry::default()));
    let candidate = UnrealCandidate::capture(
        fixture.bridge.clone(),
        registry,
        fixture.project.clone(),
        &fixture.summary,
        InteractionMode::Unknown,
    )
    .unwrap();
    let binding =
        UnrealProjectInfoBinding::bind_exact(&candidate, candidate.token(), Duration::from_secs(1))
            .unwrap();
    assert!((binding.target_validator())(binding.target()));
    assert_eq!(
        binding.target().generation(),
        candidate.token().generation()
    );
}

#[tokio::test]
async fn ue_timeout_returns_error_without_retrying_or_selecting_another_connection() {
    let mut fixture = fixture().await;
    let binding = UnrealProjectInfoBinding::bind_exact(
        &fixture.candidate,
        &fixture.token,
        Duration::from_millis(60),
    )
    .unwrap();
    let run = tokio::spawn(binding.tool().execute(binding.target().clone(), json!({})));
    assert_eq!(
        editor_request(&mut fixture.editor).await["method"],
        "project.info"
    );
    assert!(tokio::time::timeout(Duration::from_secs(1), run)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), fixture.editor.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn retiring_a_candidate_invalidates_its_binding_without_removing_a_replacement() {
    let mut fixture = fixture().await;
    let target = fixture.binding.target().clone();
    assert!(fixture.candidate.retire());
    assert!(!fixture.candidate.retire());
    assert!(!(fixture.binding.target_validator())(&target));
    assert!(fixture
        .binding
        .tool()
        .execute(target.clone(), json!({}))
        .await
        .is_err());
    let replacement = {
        let mut registry = fixture.registry.lock().unwrap();
        let token = registry
            .open_connection(EngineKind::Unreal, target.instance())
            .unwrap();
        registry
            .identify_with_mode(&token, fixture.project.clone(), InteractionMode::Unknown)
            .unwrap();
        token
    };
    assert!(!fixture.candidate.retire());
    assert!(fixture
        .registry
        .lock()
        .unwrap()
        .select_exact(&replacement, &fixture.project, false)
        .is_ok());
    assert!(
        tokio::time::timeout(Duration::from_millis(80), fixture.editor.next())
            .await
            .is_err()
    );
}
