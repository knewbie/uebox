//! Opt-in: launches a real editor only on an isolated temporary project.
use serde_json::{json, Value};
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
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uebox_agent_core::{AgentSession, OpenAiProvider, ProviderConfig};
use uebox_agent_godot_tools::GodotToolBinding;
use uebox_engine_core::{EngineKind, EngineRegistry, ProjectIdentity};
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
struct RealEditor {
    root: PathBuf,
    project: PathBuf,
    session: Option<Arc<Mutex<Option<GodotSession>>>>,
}
struct StartupGuard(Option<GodotSession>);
impl Drop for StartupGuard {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            let mut child = session.into_editor_process();
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl RealEditor {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "uebox-agent-godot-real-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let mut root_guard = TemporaryRoot(Some(root.clone()));
        let project = root.join("isolated project 世界");
        fs::create_dir(&project).unwrap();
        let home = root.join("home");
        for dir in ["appdata", "localappdata", "config", "data", "cache", "temp"] {
            fs::create_dir_all(home.join(dir)).unwrap();
        }
        let addon = project.join("addons/uebox_godot");
        fs::create_dir_all(&addon).unwrap();
        let original = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../integrations/godot/addons/uebox_godot");
        for file in ["plugin.cfg", "plugin.gd"] {
            fs::copy(original.join(file), addon.join(file)).unwrap();
        }
        let probe = project.join("addons/agent_test_probe");
        fs::create_dir_all(&probe).unwrap();
        fs::write(probe.join("plugin.cfg"), "[plugin]\nname=\"Temporary Agent Probe\"\ndescription=\"Fixture only\"\nauthor=\"test\"\nversion=\"1\"\nscript=\"plugin.gd\"\n").unwrap();
        fs::write(probe.join("plugin.gd"), "@tool\nextends EditorPlugin\nfunc _enter_tree() -> void:\n    call_deferred(\"_open_scene\")\nfunc _open_scene() -> void:\n    EditorInterface.open_scene_from_path(\"res://main.tscn\")\n").unwrap();
        fs::write(project.join("project.godot"), "config_version=5\n[application]\nconfig/name=\"Agent Godot Real\"\n[editor_plugins]\nenabled=PackedStringArray(\"res://addons/uebox_godot/plugin.cfg\", \"res://addons/agent_test_probe/plugin.cfg\")\n").unwrap();
        let mut scene =
            String::from("[gd_scene format=3]\n\n[node name=\"World\" type=\"Node2D\"]\n");
        scene.push_str("\n[node name=\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\" type=\"Node\" parent=\".\"]\n");
        for index in 0..2050 {
            scene.push_str(&format!(
                "\n[node name=\"Child{index}\" type=\"Node\" parent=\".\"]\n"
            ));
        }
        fs::write(project.join("main.tscn"), scene).unwrap();
        let executable = std::env::var_os("GODOT_BIN")
            .expect("GODOT_BIN must point to the selected real Godot binary");
        let options = LaunchOptions::new(executable, &project)
            .acknowledge_legacy_protocol_risks()
            .editor_mode(EditorMode::Headless)
            .isolate_editor_paths(home)
            .query_timeout(Duration::from_secs(3));
        let mut startup = StartupGuard(Some(GodotSession::launch(options).unwrap()));
        let session = startup.0.as_mut().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match session.connect_once() {
                Ok(info) => {
                    assert_eq!(info.name, "Agent Godot Real");
                    assert!(info.godot_version.starts_with("4.7.2"));
                    break;
                }
                Err(SessionError::Disconnected | SessionError::Timeout)
                    if Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(50))
                }
                Err(error) => panic!("real editor did not start: {error}"),
            }
        }
        loop {
            let tree = session.scene_tree().unwrap();
            if tree.scene_path.as_deref() == Some("res://main.tscn") {
                break;
            }
            assert!(Instant::now() < deadline, "real fixture scene did not open");
            thread::sleep(Duration::from_millis(50));
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
impl Drop for RealEditor {
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
async fn request(stream: &mut tokio::net::TcpStream) -> Value {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let header_end = loop {
        let count = stream.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(position) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    while bytes.len() < header_end + length {
        let count = stream.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
}
async fn respond(stream: &mut tokio::net::TcpStream, body: &str) {
    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_receives_real_godot_project_and_truncated_scene_results() {
    let editor = RealEditor::new();
    let project_file = fs::read(editor.project.join("project.godot")).unwrap();
    let scene_file = fs::read(editor.project.join("main.tscn")).unwrap();
    let mut registry = EngineRegistry::default();
    let identity = ProjectIdentity::new(EngineKind::Godot, &editor.project).unwrap();
    let instance_id = editor
        .session()
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .instance_id()
        .to_owned();
    let raw_instance = instance_id.clone();
    let token = registry
        .open_connection(EngineKind::Godot, instance_id)
        .unwrap();
    registry.identify(&token, identity.clone(), false).unwrap();
    let selection = registry.select_exact(&token, &identity, false).unwrap();
    let registry = Arc::new(Mutex::new(registry));
    let binding = GodotToolBinding::new(registry, selection, editor.session()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let private_path = editor.project.to_string_lossy().into_owned();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        request(&mut first).await;
        let calls = json!([
            {"index":0,"id":"project","type":"function","function":{"name":"godot_project_info","arguments":"{}"}},
            {"index":1,"id":"scene","type":"function","function":{"name":"godot_scene_tree","arguments":"{}"}}
        ]);
        let event = json!({"choices":[{"index":0,"delta":{"tool_calls":calls},"finish_reason":"tool_calls"}]});
        respond(&mut first, &format!("data: {event}\n\ndata: [DONE]\n\n")).await;
        let (mut second, _) = listener.accept().await.unwrap();
        let followup = request(&mut second).await;
        let project: Value =
            serde_json::from_str(followup["messages"][2]["content"].as_str().unwrap()).unwrap();
        let scene: Value =
            serde_json::from_str(followup["messages"][3]["content"].as_str().unwrap()).unwrap();
        assert_eq!(project["ok"], true);
        assert_eq!(project["data"]["name"], "Agent Godot Real");
        assert!(project["data"]["godot_version"]
            .as_str()
            .unwrap()
            .starts_with("4.7.2"));
        assert_eq!(scene["ok"], true);
        assert_eq!(scene["data"]["scene_path"], "res://main.tscn");
        assert_eq!(scene["data"]["truncated"], true);
        assert_eq!(scene["data"]["nodes"].as_array().unwrap().len(), 2048);
        assert_eq!(scene["data"]["nodes"][0]["name"], "World");
        assert_eq!(
            scene["data"]["nodes"][1]["name"],
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert!(!followup.to_string().contains(&private_path));
        assert!(!followup.to_string().contains(&raw_instance));
        respond(&mut second, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"real receipt\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").await;
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
        "real receipt"
    );
    server.await.unwrap();
    assert_eq!(
        fs::read(editor.project.join("project.godot")).unwrap(),
        project_file
    );
    assert_eq!(
        fs::read(editor.project.join("main.tscn")).unwrap(),
        scene_file
    );
}
