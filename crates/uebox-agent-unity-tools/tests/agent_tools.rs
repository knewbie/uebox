use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uebox_agent_core::{
    AgentSession, Engine, Error, OpenAiProvider, ProviderConfig, RunPhase, RunTarget,
};
use uebox_agent_unity_tools::{ProbeState, UnityTools};
use uebox_engine_core::{
    ConnectionToken, EditorSelection, EngineKind, EngineRegistry, ProjectIdentity,
};
use uebox_engine_unity::{BridgeOptions, Target, UnityClient};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
const SECRET: &str = "fixture-provider-credential-not-for-the-model";

struct TestProject(PathBuf);
impl TestProject {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "uebox-agent-unity-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        Self(std::fs::canonicalize(dir).unwrap())
    }
    fn text(&self) -> String {
        self.0.to_str().unwrap().to_owned()
    }
}
impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read_frame(stream: &mut TcpStream) -> Value {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header).unwrap();
    let length = u64::from_be_bytes(header) as usize;
    assert!((1..4096).contains(&length));
    let mut data = vec![0; length];
    stream.read_exact(&mut data).unwrap();
    serde_json::from_slice(&data).unwrap()
}

fn write_frame(stream: &mut TcpStream, value: Value) {
    let data = serde_json::to_vec(&value).unwrap();
    stream
        .write_all(&(data.len() as u64).to_be_bytes())
        .unwrap();
    stream.write_all(&data).unwrap();
}

fn assert_project_request(stream: &mut TcpStream) {
    assert_eq!(
        read_frame(stream),
        json!({"type":"get_project_info","params":{}})
    );
}

fn project_reply(root: &str, name: &str) -> Value {
    json!({"status":"success","result":{"success":true,"data":{
        "projectRoot":root,"projectName":name,"unityVersion":"2022.3.62f3",
        "assetsPath":format!("{root}/Assets"),"platform":"StandaloneOSX",
        "renderPipeline":"BuiltIn","unrelatedCredential":SECRET
    }}})
}

fn scene_reply() -> Value {
    json!({"status":"success","result":{"success":true,"data":{
        "name":"Live Scene","path":"Assets/Live.unity","buildIndex":3,
        "isDirty":true,"isLoaded":true,"rootCount":7,"unrelatedCredential":SECRET
    }}})
}

fn start_editor<F>(project: &TestProject, after_handshake: F) -> (UnityClient, JoinHandle<()>)
where
    F: FnOnce(TcpStream, String) + Send + 'static,
{
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    assert_ne!(address.port(), 6400);
    let root = project.text();
    let server_root = root.clone();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&server_root, "Initial Snapshot"));
        after_handshake(stream, server_root);
    });
    let target = Target::new(address, root).unwrap().authorize_takeover();
    let client = UnityClient::connect_authorized(
        target,
        BridgeOptions {
            io_timeout: Duration::from_secs(3),
            max_frame_bytes: 64 * 1024,
        },
    )
    .unwrap();
    (client, server)
}

fn selection(
    project: &TestProject,
) -> (Arc<Mutex<EngineRegistry>>, ConnectionToken, EditorSelection) {
    let registry = Arc::new(Mutex::new(EngineRegistry::default()));
    let (token, selection) = {
        let mut registry = registry.lock().unwrap();
        let project = ProjectIdentity::new(EngineKind::Unity, &project.0).unwrap();
        let token = registry
            .open_connection(EngineKind::Unity, "fixture-editor")
            .unwrap();
        registry.identify(&token, project.clone(), true).unwrap();
        let selection = registry.select_exact(&token, &project, true).unwrap();
        (token, selection)
    };
    (registry, token, selection)
}

async fn http_request(stream: &mut tokio::net::TcpStream) -> Value {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let end = loop {
        let count = stream.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(pos) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
    assert!(headers
        .lines()
        .any(|line| line.eq_ignore_ascii_case(&format!("authorization: Bearer {SECRET}"))));
    let length: usize = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
    while bytes.len() < end + length {
        let count = stream.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    let body = &bytes[end..end + length];
    assert!(!String::from_utf8_lossy(body).contains(SECRET));
    serde_json::from_slice(body).unwrap()
}

async fn respond_sse(stream: &mut tokio::net::TcpStream, body: &str) {
    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
}

fn tool_call_event(calls: Value) -> String {
    let event =
        json!({"choices":[{"index":0,"delta":{"tool_calls":calls},"finish_reason":"tool_calls"}]});
    format!("data: {event}\n\ndata: [DONE]\n\n")
}

fn call(index: usize, id: &str, name: &str) -> Value {
    json!({"index":index,"id":id,"type":"function","function":{"name":name,"arguments":"{}"}})
}

fn provider(address: std::net::SocketAddr) -> OpenAiProvider {
    OpenAiProvider::new(
        ProviderConfig::new(format!("http://{address}/completions"), "fixture-model")
            .with_bearer_token(SECRET),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binding_rejects_a_client_for_another_project_before_dispatch() {
    let project = TestProject::new();
    let other = TestProject::new();
    let (client, editor) = start_editor(&project, |mut stream, _| {
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let (registry, token, selected) = selection(&other);
    assert!(UnityTools::new(client, registry, selected, token)
        .await
        .is_err());
    editor.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binding_rejects_a_stale_or_foreign_connection_token() {
    let project = TestProject::new();
    let (client, editor) = start_editor(&project, |mut stream, _| {
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let (registry, token, selected) = selection(&project);
    let foreign = EngineRegistry::default()
        .open_connection(EngineKind::Unity, "fixture-editor")
        .unwrap();
    assert_ne!(foreign.generation(), token.generation());
    assert!(UnityTools::new(client, registry.clone(), selected, foreign)
        .await
        .is_err());
    editor.join().unwrap();
    assert!(registry.lock().unwrap().close_connection(&token));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retargeted_selected_symlink_cannot_rebind_an_existing_client_to_a_new_project() {
    use std::os::unix::fs::symlink;
    let original = TestProject::new();
    let replacement = TestProject::new();
    let container = TestProject::new();
    let alias = container.0.join("chosen-project");
    symlink(&original.0, &alias).unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    assert_ne!(address.port(), 6400);
    let original_root = original.text();
    let editor = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        write_frame(
            &mut stream,
            project_reply(&original_root, "Original Project"),
        );
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let client = UnityClient::connect_authorized(
        Target::new(address, alias.to_str().unwrap())
            .unwrap()
            .authorize_takeover(),
        BridgeOptions::default(),
    )
    .unwrap();
    std::fs::remove_file(&alias).unwrap();
    symlink(&replacement.0, &alias).unwrap();
    let (registry, token, selected) = selection(&replacement);
    assert!(UnityTools::new(client, registry, selected, token)
        .await
        .is_err());
    editor.join().unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_symlink_binds_when_the_original_reply_was_canonical() {
    use std::os::unix::fs::symlink;
    let project = TestProject::new();
    let container = TestProject::new();
    let alias = container.0.join("chosen-project");
    symlink(&project.0, &alias).unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    assert_ne!(address.port(), 6400);
    let root = project.text();
    let editor = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&root, "Initial"));
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&root, "Live"));
    });
    let client = UnityClient::connect_authorized(
        Target::new(address, alias.to_str().unwrap())
            .unwrap()
            .authorize_takeover(),
        BridgeOptions::default(),
    )
    .unwrap();
    let (registry, token, selected) = selection(&project);
    let tools = UnityTools::new(client, registry, selected, token)
        .await
        .unwrap();
    let result = tools
        .project_info_tool()
        .execute(tools.run_target(), json!({}))
        .await
        .unwrap();
    assert_eq!(result["project_name"], "Live");
    editor.join().unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binding_rejects_a_noncanonical_peer_alias_without_resolving_it() {
    use std::os::unix::fs::symlink;
    let project = TestProject::new();
    let container = TestProject::new();
    let alias = container.0.join("chosen-project");
    symlink(&project.0, &alias).unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    assert_ne!(address.port(), 6400);
    let reported = alias.to_str().unwrap().to_owned();
    let editor = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&reported, "Initial"));
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let client = UnityClient::connect_authorized(
        Target::new(address, alias.to_str().unwrap())
            .unwrap()
            .authorize_takeover(),
        BridgeOptions::default(),
    )
    .unwrap();
    let (registry, token, selected) = selection(&project);
    assert!(UnityTools::new(client, registry, selected, token)
        .await
        .is_err());
    editor.join().unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn later_project_reply_using_a_retargeted_alias_is_not_returned_as_success() {
    use std::os::unix::fs::symlink;
    let original = TestProject::new();
    let replacement = TestProject::new();
    let container = TestProject::new();
    let alias = container.0.join("chosen-project");
    symlink(&original.0, &alias).unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    assert_ne!(address.port(), 6400);
    let original_root = original.text();
    let reported_alias = alias.to_str().unwrap().to_owned();
    let editor = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        write_frame(
            &mut stream,
            project_reply(&original_root, "Original Project"),
        );
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&reported_alias, "Wrong Project"));
    });
    let client = UnityClient::connect_authorized(
        Target::new(address, alias.to_str().unwrap())
            .unwrap()
            .authorize_takeover(),
        BridgeOptions::default(),
    )
    .unwrap();
    let (registry, token, selected) = selection(&original);
    let tools = UnityTools::new(client, registry, selected, token)
        .await
        .unwrap();
    std::fs::remove_file(&alias).unwrap();
    symlink(&replacement.0, &alias).unwrap();
    let target = tools.run_target();
    assert!(tools
        .project_info_tool()
        .execute(target.clone(), json!({}))
        .await
        .is_err());
    assert!(!(tools.validator())(&target));
    editor.join().unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scene_query_stops_before_scene_request_if_the_reported_alias_changes() {
    use std::os::unix::fs::symlink;
    let original = TestProject::new();
    let replacement = TestProject::new();
    let container = TestProject::new();
    let alias = container.0.join("chosen-project");
    symlink(&original.0, &alias).unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    assert_ne!(address.port(), 6400);
    let original_root = original.text();
    let reported_alias = alias.to_str().unwrap().to_owned();
    let editor = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        write_frame(
            &mut stream,
            project_reply(&original_root, "Original Project"),
        );
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&reported_alias, "Wrong Project"));
        let mut byte = [0];
        assert_eq!(
            stream.read(&mut byte).unwrap(),
            0,
            "scene request dispatched after an ambiguous project reply"
        );
    });
    let client = UnityClient::connect_authorized(
        Target::new(address, alias.to_str().unwrap())
            .unwrap()
            .authorize_takeover(),
        BridgeOptions::default(),
    )
    .unwrap();
    let (registry, token, selected) = selection(&original);
    let tools = UnityTools::new(client, registry, selected, token)
        .await
        .unwrap();
    std::fs::remove_file(&alias).unwrap();
    symlink(&replacement.0, &alias).unwrap();
    let target = tools.run_target();
    assert!(tools
        .active_scene_tool()
        .execute(target.clone(), json!({}))
        .await
        .is_err());
    assert!(!(tools.validator())(&target));
    editor.join().unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scene_reply_is_discarded_if_the_client_internal_project_recheck_changes() {
    use std::os::unix::fs::symlink;
    let original = TestProject::new();
    let container = TestProject::new();
    let alias = container.0.join("chosen-project");
    symlink(&original.0, &alias).unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    assert_ne!(address.port(), 6400);
    let original_root = original.text();
    let reported_alias = alias.to_str().unwrap().to_owned();
    let editor = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .write_all(b"WELCOME UNITY-MCP 1 FRAMING=1\n")
            .unwrap();
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&original_root, "Initial"));
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&original_root, "Preflight"));
        assert_project_request(&mut stream);
        write_frame(
            &mut stream,
            project_reply(&reported_alias, "Changed representation"),
        );
        assert_eq!(
            read_frame(&mut stream),
            json!({"type":"manage_scene","params":{"action":"get_active"}})
        );
        write_frame(&mut stream, scene_reply());
    });
    let client = UnityClient::connect_authorized(
        Target::new(address, alias.to_str().unwrap())
            .unwrap()
            .authorize_takeover(),
        BridgeOptions::default(),
    )
    .unwrap();
    let (registry, token, selected) = selection(&original);
    let tools = UnityTools::new(client, registry, selected, token)
        .await
        .unwrap();
    let target = tools.run_target();
    assert!(tools
        .active_scene_tool()
        .execute(target.clone(), json!({}))
        .await
        .is_err());
    assert!(!(tools.validator())(&target));
    editor.join().unwrap();
}

async fn read_scene_fixture_path(path: &str) -> Result<Value, uebox_agent_core::ToolError> {
    let project = TestProject::new();
    let path = path.to_owned();
    let (client, editor) = start_editor(&project, move |mut stream, root| {
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&root, "Selected"));
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&root, "Selected"));
        assert_eq!(
            read_frame(&mut stream),
            json!({"type":"manage_scene","params":{"action":"get_active"}})
        );
        let mut reply = scene_reply();
        reply["result"]["data"]["path"] = json!(path);
        write_frame(&mut stream, reply);
    });
    let (registry, token, selected) = selection(&project);
    let tools = UnityTools::new(client, registry, selected, token)
        .await
        .unwrap();
    let result = tools
        .active_scene_tool()
        .execute(tools.run_target(), json!({}))
        .await;
    editor.join().unwrap();
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scene_reply_rejects_absolute_device_and_traversal_paths() {
    let unsafe_paths = [
        "/another-project/Assets/Other.unity",
        "C:/another-project/Assets/Other.unity",
        r"C:\another-project\Assets\Other.unity",
        r"\\server\share\Assets\Other.unity",
        r"\\?\C:\another-project\Assets\Other.unity",
        r"\\.\device\Other.unity",
        "//?/UNC/server/share/Other.unity",
        "Assets/../../another-project/Assets/Other.unity",
        "Assets/../Assets/Other.unity",
        r"Assets\..\another-project\Other.unity",
        "Packages/com.example.package/../../../Other.unity",
        "Assets//Scenes/Other.unity",
        "Assets/./Other.unity",
        "Assets/Scenes/..",
        "Assets/Scenes/",
        "Packages/com.example.package",
        "Packages//Scenes/Other.unity",
        "OtherProject/Assets/Other.unity",
        "Assets/Scenes/file:stream.unity",
        "Assets/Scenes/Other\n.unity",
    ];
    for path in unsafe_paths {
        let result = read_scene_fixture_path(path).await;
        assert!(result.is_err(), "unsafe scene path was accepted: {path:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scene_reply_preserves_unsaved_project_and_package_paths() {
    // These paths do not exist in the temporary project: peer-reported scene
    // paths must be checked lexically, not resolved or probed on disk.
    let valid_paths = [
        "",
        "Assets/Live.unity",
        "Assets/Scenes/入口 - 1.unity",
        "Assets/Foo..Bar/Scene.unity",
        "Assets/百分比%/Scene.unity",
        "Packages/com.example.test/Samples~/Room #1.unity",
        "Packages/com.example.test/Scene.unity",
    ];
    for path in valid_paths {
        let result = read_scene_fixture_path(path).await.unwrap();
        assert_eq!(result["path"], path);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn absolute_scene_from_changed_peer_never_becomes_a_model_success() {
    let project = TestProject::new();
    let other = TestProject::new();
    let other_scene = format!("{}/Assets/Other.unity", other.text());
    let (client, editor) = start_editor(&project, move |mut stream, root| {
        // The peer still reports the selected project for both project checks,
        // then returns an absolute scene from a different project.
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&root, "Selected"));
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&root, "Selected"));
        assert_eq!(
            read_frame(&mut stream),
            json!({"type":"manage_scene","params":{"action":"get_active"}})
        );
        let mut reply = scene_reply();
        reply["result"]["data"]["path"] = json!(other_scene);
        write_frame(&mut stream, reply);
    });
    let (registry, token, selected) = selection(&project);
    let tools = UnityTools::new(client, registry, selected, token)
        .await
        .unwrap();
    let model = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = model.local_addr().unwrap();
    assert_ne!(address.port(), 6400);
    let model_task = tokio::spawn(async move {
        let (mut first, _) = model.accept().await.unwrap();
        http_request(&mut first).await;
        respond_sse(
            &mut first,
            &tool_call_event(json!([call(0, "scene", "unity_active_scene")])),
        )
        .await;
        if let Ok(Ok((mut second, _))) =
            tokio::time::timeout(Duration::from_millis(500), model.accept()).await
        {
            let request = http_request(&mut second).await;
            let receipt: Value =
                serde_json::from_str(request["messages"][2]["content"].as_str().unwrap()).unwrap();
            respond_sse(&mut second, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
            Some(receipt)
        } else {
            None
        }
    });
    let session = AgentSession::new(
        provider(address),
        tools.tool_registry().unwrap(),
        tools.validator(),
        3,
    )
    .unwrap();
    let result = session
        .start("Inspect scene")
        .unwrap()
        .execute(|_| {})
        .await;
    let receipt = model_task.await.unwrap();
    editor.join().unwrap();
    assert!(
        matches!(result, Err(Error::TargetChanged)),
        "unsafe scene returned to the model: result={result:?}, receipt={receipt:?}"
    );
    assert!(receipt.is_none(), "unexpected model receipt: {receipt:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_token_cannot_bind_two_different_live_clients() {
    let project = TestProject::new();
    let (first, first_editor) = start_editor(&project, |mut stream, _| {
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let (second, second_editor) = start_editor(&project, |mut stream, _| {
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let (registry, token, selected) = selection(&project);
    let tools = UnityTools::new(first, registry.clone(), selected.clone(), token.clone())
        .await
        .unwrap();
    assert!(UnityTools::new(second, registry, selected, token)
        .await
        .is_err());
    assert!((tools.validator())(&tools.run_target()));
    drop(tools);
    first_editor.join().unwrap();
    second_editor.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_a_binding_while_the_registry_is_locked_never_deadlocks() {
    let project = TestProject::new();
    let (client, editor) = start_editor(&project, |mut stream, _| {
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let (registry, token, selected) = selection(&project);
    let tools = UnityTools::new(client, registry.clone(), selected.clone(), token)
        .await
        .unwrap();
    let registry_guard = registry.lock().unwrap();
    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
    let drop_thread = thread::spawn(move || {
        drop(tools);
        dropped_tx.send(()).unwrap();
    });
    let dropped_without_waiting = dropped_rx.recv_timeout(Duration::from_millis(250)).is_ok();
    drop(registry_guard);
    drop_thread.join().unwrap();
    assert!(
        dropped_without_waiting,
        "dropping a binding blocked on the registry"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while registry
        .lock()
        .unwrap()
        .resolve_selection(&selected)
        .is_ok()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "connection token was not retired"
        );
        thread::yield_now();
    }
    editor.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_tool_calls_return_live_unity_receipts_to_the_next_model_step() {
    let project = TestProject::new();
    let private_root = project.text();
    let (client, editor) = start_editor(&project, |mut stream, root| {
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&root, "Live Project"));
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&root, "Live Project"));
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&root, "Live Project"));
        assert_eq!(
            read_frame(&mut stream),
            json!({"type":"manage_scene","params":{"action":"get_active"}})
        );
        write_frame(&mut stream, scene_reply());
    });
    let (registry, token, selection) = selection(&project);
    let tools = UnityTools::new(client, registry, selection, token)
        .await
        .unwrap();
    let target = tools.run_target();
    let model = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = model.local_addr().unwrap();
    assert_ne!(address.port(), 6400);
    let expected_target = json!({"engine":"unity","project":target.project(),"instance":target.instance(),"generation":target.generation()});
    let model_task = tokio::spawn(async move {
        let (mut first, _) = model.accept().await.unwrap();
        let request = http_request(&mut first).await;
        let declarations = request["tools"].as_array().unwrap();
        assert_eq!(declarations.len(), 2);
        let mut names: Vec<_> = declarations
            .iter()
            .map(|v| v["function"]["name"].as_str().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["unity_active_scene", "unity_project_info"]);
        for declaration in declarations {
            assert_eq!(
                declaration["function"]["parameters"]["additionalProperties"],
                false
            );
            assert_eq!(
                declaration["function"]["parameters"]["properties"],
                json!({})
            );
        }
        respond_sse(
            &mut first,
            &tool_call_event(json!([
                call(0, "p", "unity_project_info"),
                call(1, "s", "unity_active_scene")
            ])),
        )
        .await;
        let (mut second, _) = model.accept().await.unwrap();
        let request = http_request(&mut second).await;
        let messages = request["messages"].as_array().unwrap();
        assert_eq!(messages[2]["tool_call_id"], "p");
        assert_eq!(messages[3]["tool_call_id"], "s");
        let project_result: Value =
            serde_json::from_str(messages[2]["content"].as_str().unwrap()).unwrap();
        let scene_result: Value =
            serde_json::from_str(messages[3]["content"].as_str().unwrap()).unwrap();
        for receipt in [&project_result, &scene_result] {
            let contents = receipt.to_string();
            assert!(!contents.contains(&private_root));
            assert_ne!(receipt["target"]["project"], private_root.as_str());
            assert!(!contents.contains("fixture-editor"));
            assert!(!contents.contains(SECRET));
        }
        assert!(project_result["data"].get("project_root").is_none());
        assert!(project_result["data"].get("assets_path").is_none());
        assert_eq!(scene_result["data"]["path"], "Assets/Live.unity");
        assert_eq!(project_result["ok"], true);
        assert_eq!(project_result["target"], expected_target);
        assert_eq!(project_result["data"]["project_name"], "Live Project");
        assert_eq!(scene_result["ok"], true);
        assert_eq!(scene_result["target"], expected_target);
        assert_eq!(scene_result["data"]["name"], "Live Scene");
        assert_eq!(scene_result["data"]["root_count"], 7);
        respond_sse(&mut second, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Observed live scene\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
    });
    let session = AgentSession::new(
        provider(address),
        tools.tool_registry().unwrap(),
        tools.validator(),
        3,
    )
    .unwrap();
    let answer = session
        .start("Inspect project and scene")
        .unwrap()
        .execute(|_| {})
        .await
        .unwrap();
    assert_eq!(answer.text, "Observed live scene");
    model_task.await.unwrap();
    editor.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_retires_immediately_but_keeps_the_socket_guard_until_an_admitted_read_finishes() {
    struct SocketGuard(std::sync::mpsc::Sender<()>);
    impl Drop for SocketGuard {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    let project = TestProject::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
    let (client, editor) = start_editor(&project, move |mut stream, root| {
        assert_project_request(&mut stream);
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        write_frame(&mut stream, project_reply(&root, "Late reply"));
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
    });
    let (registry, token, selection) = selection(&project);
    let tools = UnityTools::new_with_socket_guard(
        client,
        registry.clone(),
        selection,
        token.clone(),
        SocketGuard(dropped_tx),
    )
    .await
    .unwrap();
    let target = tools.run_target();
    let work = tokio::spawn(tools.project_info_tool().execute(target.clone(), json!({})));
    started_rx.await.unwrap();
    assert_eq!(tools.probe().await, ProbeState::Busy);

    let mut close = Box::pin(tools.close());
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut close)
        .await
        .is_err());
    assert!(!(tools.validator())(&target));
    assert_eq!(tools.probe().await, ProbeState::Closed);
    assert!(dropped_rx.try_recv().is_err());
    release_tx.send(()).unwrap();
    assert!(work.await.unwrap().is_err());
    close.await;
    dropped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(!registry.lock().unwrap().close_connection(&token));
    editor.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_uses_the_existing_connection_and_retires_it_on_eof() {
    let project = TestProject::new();
    let (client, editor) = start_editor(&project, |mut stream, _| {
        let mut request = [0; 12];
        stream.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"\0\0\0\0\0\0\0\x04ping");
        write_frame(
            &mut stream,
            json!({"status":"success","result":{"message":"pong"}}),
        );
    });
    let (registry, token, selection) = selection(&project);
    let tools = UnityTools::new(client, registry, selection, token)
        .await
        .unwrap();
    assert_eq!(tools.probe().await, ProbeState::Ready);
    editor.join().unwrap();
    assert_eq!(tools.probe().await, ProbeState::Closed);
    assert!(tools.is_retired());
    assert!(!(tools.validator())(&tools.run_target()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_cannot_retarget_or_call_an_arbitrary_editor_method() {
    let project = TestProject::new();
    let (client, editor) = start_editor(&project, |mut stream, root| {
        // There must be exactly one dispatch, from the final valid call.
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&root, "Only Allowed Read"));
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let (registry, token, selected) = selection(&project);
    let tools = UnityTools::new(client, registry, selected, token)
        .await
        .unwrap();
    let target = tools.run_target();
    let model = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = model.local_addr().unwrap();
    assert_ne!(address.port(), 6400);
    let model_task = tokio::spawn(async move {
        let (mut first, _) = model.accept().await.unwrap();
        http_request(&mut first).await;
        let calls = json!([
            {"index":0,"id":"cross","type":"function","function":{"name":"unity_project_info","arguments":"{\"project\":\"/another-project\"}"}},
            {"index":1,"id":"method","type":"function","function":{"name":"unity_execute","arguments":"{}"}},
            call(2,"allowed","unity_project_info")
        ]);
        respond_sse(&mut first, &tool_call_event(calls)).await;
        let (mut second, _) = model.accept().await.unwrap();
        let request = http_request(&mut second).await;
        let messages = request["messages"].as_array().unwrap();
        let parse = |i: usize| -> Value {
            serde_json::from_str(messages[i]["content"].as_str().unwrap()).unwrap()
        };
        assert_eq!(parse(2)["error"], "invalid_arguments");
        assert_eq!(parse(3)["error"], "tool_not_allowed");
        assert_eq!(parse(4)["data"]["project_name"], "Only Allowed Read");
        for i in 2..5 {
            let result = parse(i);
            assert_eq!(result["target"]["engine"], "unity");
            assert_eq!(result["target"]["project"], target.project());
            assert_eq!(result["target"]["instance"], target.instance());
            assert_eq!(result["target"]["generation"], target.generation());
        }
        respond_sse(&mut second, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Finished\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
    });
    let session = AgentSession::new(
        provider(address),
        tools.tool_registry().unwrap(),
        tools.validator(),
        3,
    )
    .unwrap();
    assert_eq!(
        session
            .start("Inspect")
            .unwrap()
            .execute(|_| {})
            .await
            .unwrap()
            .text,
        "Finished"
    );
    model_task.await.unwrap();
    drop(session);
    drop(tools);
    editor.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selection_invalidated_during_model_stream_never_dispatches() {
    let project = TestProject::new();
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    let (client, editor) = start_editor(&project, move |mut stream, _| {
        let mut byte = [0];
        assert_eq!(
            stream.read(&mut byte).unwrap(),
            0,
            "invalidated run sent an editor request"
        );
        closed_tx.send(()).unwrap();
    });
    let (registry, token, selected) = selection(&project);
    let tools = UnityTools::new(client, registry.clone(), selected, token.clone())
        .await
        .unwrap();
    let model = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = model.local_addr().unwrap();
    assert_ne!(address.port(), 6400);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let model_task = tokio::spawn(async move {
        let (mut first, _) = model.accept().await.unwrap();
        http_request(&mut first).await;
        started_tx.send(()).unwrap();
        release_rx.await.unwrap();
        respond_sse(
            &mut first,
            &tool_call_event(json!([call(0, "p", "unity_project_info")])),
        )
        .await;
    });
    let session = AgentSession::new(
        provider(address),
        tools.tool_registry().unwrap(),
        tools.validator(),
        3,
    )
    .unwrap();
    let task = tokio::spawn(session.start("Inspect").unwrap().execute(|_| {}));
    started_rx.await.unwrap();
    assert!(registry.lock().unwrap().close_connection(&token));
    release_tx.send(()).unwrap();
    assert!(matches!(task.await.unwrap(), Err(Error::TargetChanged)));
    model_task.await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), closed_rx)
        .await
        .expect("invalidated connection stayed open")
        .unwrap();
    drop(session);
    drop(tools);
    editor.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mismatched_target_fields_and_arguments_never_dispatch() {
    let project = TestProject::new();
    let (client, editor) = start_editor(&project, |mut stream, root| {
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&root, "One Allowed Read"));
    });
    let (registry, token, selected) = selection(&project);
    let tools = UnityTools::new(client, registry, selected, token)
        .await
        .unwrap();
    let target = tools.run_target();
    let tool = tools.project_info_tool();
    for other in [
        RunTarget::new(
            Engine::Godot,
            target.project(),
            target.instance(),
            target.generation(),
        )
        .unwrap(),
        RunTarget::new(
            Engine::Unity,
            "/different-project",
            target.instance(),
            target.generation(),
        )
        .unwrap(),
        RunTarget::new(
            Engine::Unity,
            target.project(),
            "different-instance",
            target.generation(),
        )
        .unwrap(),
        RunTarget::new(
            Engine::Unity,
            target.project(),
            target.instance(),
            target.generation() + 1,
        )
        .unwrap(),
    ] {
        assert!(tool.execute(other, json!({})).await.is_err());
    }
    assert!(tool
        .execute(target.clone(), json!({"method":"write"}))
        .await
        .is_err());
    let result = tool.execute(target, json!({})).await.unwrap();
    assert_eq!(result["project_name"], "One Allowed Read");
    editor.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replacement_generation_cannot_revive_a_stale_selection() {
    let project = TestProject::new();
    let (client, editor) = start_editor(&project, |mut stream, _| {
        let mut byte = [0];
        assert_eq!(
            stream.read(&mut byte).unwrap(),
            0,
            "stale binding sent a request"
        );
    });
    let (registry, old_token, selected) = selection(&project);
    let tools = UnityTools::new(client, registry.clone(), selected, old_token.clone())
        .await
        .unwrap();
    let target = tools.run_target();
    let replacement = {
        let mut registry = registry.lock().unwrap();
        assert!(registry.close_connection(&old_token));
        let replacement = registry
            .open_connection(EngineKind::Unity, "fixture-editor")
            .unwrap();
        let project = ProjectIdentity::new(EngineKind::Unity, &project.0).unwrap();
        registry.identify(&replacement, project, true).unwrap();
        replacement
    };
    assert!(!(tools.validator())(&target));
    assert!(tools
        .project_info_tool()
        .execute(target, json!({}))
        .await
        .is_err());
    drop(tools);
    editor.join().unwrap();
    assert!(registry.lock().unwrap().close_connection(&replacement));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_failure_is_not_success_and_raw_editor_error_is_not_returned() {
    let project = TestProject::new();
    let (client, editor) = start_editor(&project, |mut stream, _| {
        assert_project_request(&mut stream);
        write_frame(&mut stream, json!({"status":"error","error":SECRET}));
    });
    let (registry, token, selected) = selection(&project);
    let tools = UnityTools::new(client, registry, selected, token)
        .await
        .unwrap();
    let target = tools.run_target();
    let error = tools
        .project_info_tool()
        .execute(target.clone(), json!({}))
        .await
        .unwrap_err();
    assert!(!format!("{error:?}").contains(SECRET));
    assert!(!(tools.validator())(&target));
    assert!(tools
        .active_scene_tool()
        .execute(target, json!({}))
        .await
        .is_err());
    editor.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_run_stops_on_editor_failure_without_a_success_receipt() {
    let project = TestProject::new();
    let (client, editor) = start_editor(&project, |mut stream, _| {
        assert_project_request(&mut stream);
        write_frame(&mut stream, json!({"status":"error","error":SECRET}));
    });
    let (registry, token, selected) = selection(&project);
    let tools = UnityTools::new(client, registry, selected, token)
        .await
        .unwrap();
    let model = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = model.local_addr().unwrap();
    assert_ne!(address.port(), 6400);
    let model_task = tokio::spawn(async move {
        let (mut first, _) = model.accept().await.unwrap();
        http_request(&mut first).await;
        respond_sse(
            &mut first,
            &tool_call_event(json!([call(0, "p", "unity_project_info")])),
        )
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(200), model.accept())
                .await
                .is_err(),
            "failed read was sent to another model step"
        );
    });
    let session = AgentSession::new(
        provider(address),
        tools.tool_registry().unwrap(),
        tools.validator(),
        3,
    )
    .unwrap();
    let error = session
        .start("Inspect")
        .unwrap()
        .execute(|_| {})
        .await
        .unwrap_err();
    assert!(matches!(error, Error::TargetChanged));
    assert!(!format!("{error:?}").contains(SECRET));
    model_task.await.unwrap();
    editor.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editor_project_change_is_rejected_before_any_scene_request() {
    let project = TestProject::new();
    let other = TestProject::new();
    let wrong_root = other.text();
    let (client, editor) = start_editor(&project, move |mut stream, _| {
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&wrong_root, "Wrong Project"));
        let mut byte = [0];
        assert_eq!(
            stream.read(&mut byte).unwrap(),
            0,
            "scene command followed a project mismatch"
        );
    });
    let (registry, token, selected) = selection(&project);
    let tools = UnityTools::new(client, registry, selected, token)
        .await
        .unwrap();
    let target = tools.run_target();
    assert!(tools
        .active_scene_tool()
        .execute(target.clone(), json!({}))
        .await
        .is_err());
    assert!(!(tools.validator())(&target));
    editor.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalidation_during_a_read_discards_the_reply_and_closes_the_original_connection() {
    let project = TestProject::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (client, editor) = start_editor(&project, move |mut stream, root| {
        assert_project_request(&mut stream);
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        write_frame(&mut stream, project_reply(&root, "stale reply"));
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let (registry, token, selected) = selection(&project);
    let tools = UnityTools::new(client, registry.clone(), selected, token.clone())
        .await
        .unwrap();
    let task = tokio::spawn(
        tools
            .project_info_tool()
            .execute(tools.run_target(), json!({})),
    );
    started_rx.await.unwrap();
    let new_token = {
        let mut registry = registry.lock().unwrap();
        assert!(registry.close_connection(&token));
        let new_token = registry
            .open_connection(EngineKind::Unity, "fixture-editor")
            .unwrap();
        registry
            .identify(
                &new_token,
                ProjectIdentity::new(EngineKind::Unity, &project.0).unwrap(),
                true,
            )
            .unwrap();
        new_token
    };
    release_tx.send(()).unwrap();
    assert!(task.await.unwrap().is_err());
    editor.join().unwrap();
    assert!(registry.lock().unwrap().close_connection(&new_token));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_agent_run_keeps_the_original_connection_busy_until_the_read_finishes() {
    let project = TestProject::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (checked_tx, checked_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (client, editor) = start_editor(&project, move |mut stream, root| {
        assert_project_request(&mut stream);
        started_tx.send(()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut byte = [0];
        assert!(
            matches!(stream.peek(&mut byte), Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
            "another request reached an in-flight connection"
        );
        checked_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write_frame(&mut stream, project_reply(&root, "cancelled read"));
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&root, "after drain"));
    });
    let (registry, token, selected) = selection(&project);
    let tools = UnityTools::new(client, registry, selected, token)
        .await
        .unwrap();
    let model = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = model.local_addr().unwrap();
    assert_ne!(address.port(), 6400);
    let model_task = tokio::spawn(async move {
        let (mut stream, _) = model.accept().await.unwrap();
        http_request(&mut stream).await;
        respond_sse(
            &mut stream,
            &tool_call_event(json!([call(0, "p", "unity_project_info")])),
        )
        .await;
    });
    let session = AgentSession::new(
        provider(address),
        tools.tool_registry().unwrap(),
        tools.validator(),
        3,
    )
    .unwrap();
    let run = session.start("Read project").unwrap();
    let cancellation = run.cancellation();
    let run_task = tokio::spawn(run.execute(|_| {}));
    started_rx.await.unwrap();
    cancellation.cancel();
    assert!(matches!(run_task.await.unwrap(), Err(Error::Cancelled)));
    assert_eq!(session.snapshot().phase, Some(RunPhase::Draining));
    assert!(matches!(session.start("another run"), Err(Error::Busy)));
    let busy = tokio::time::timeout(
        Duration::from_millis(100),
        tools
            .project_info_tool()
            .execute(tools.run_target(), json!({})),
    )
    .await
    .unwrap();
    assert!(busy.is_err());
    checked_rx.await.unwrap();
    release_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while session.snapshot().phase.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let result = tools
        .project_info_tool()
        .execute(tools.run_target(), json!({}))
        .await
        .unwrap();
    assert_eq!(result["project_name"], "after drain");
    model_task.await.unwrap();
    editor.join().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn editor_io_does_not_block_a_single_thread_tokio_runtime() {
    let project = TestProject::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (client, editor) = start_editor(&project, move |mut stream, root| {
        assert_project_request(&mut stream);
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        write_frame(&mut stream, project_reply(&root, "worker completed"));
    });
    let (registry, token, selected) = selection(&project);
    let tools = UnityTools::new(client, registry, selected, token)
        .await
        .unwrap();
    let task = tokio::spawn(
        tools
            .project_info_tool()
            .execute(tools.run_target(), json!({})),
    );
    started_rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    release_tx.send(()).unwrap();
    assert_eq!(
        task.await.unwrap().unwrap()["project_name"],
        "worker completed"
    );
    editor.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_a_tool_future_does_not_release_an_inflight_client() {
    let project = TestProject::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (client, editor) = start_editor(&project, move |mut stream, root| {
        assert_project_request(&mut stream);
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        write_frame(&mut stream, project_reply(&root, "discarded"));
        assert_project_request(&mut stream);
        write_frame(&mut stream, project_reply(&root, "after drop"));
    });
    let (registry, token, selected) = selection(&project);
    let tools = UnityTools::new(client, registry, selected, token)
        .await
        .unwrap();
    let task = tokio::spawn(
        tools
            .project_info_tool()
            .execute(tools.run_target(), json!({})),
    );
    started_rx.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(tools
        .project_info_tool()
        .execute(tools.run_target(), json!({}))
        .await
        .is_err());
    release_tx.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match tools
                .project_info_tool()
                .execute(tools.run_target(), json!({}))
                .await
            {
                Ok(result) => break result,
                Err(_) => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(result["project_name"], "after drop");
    editor.join().unwrap();
}
