//! Opt-in test: launches only an editor and projects created by this test.
use serde_json::Value;
use std::{
    fs,
    net::{Ipv4Addr, SocketAddr, TcpStream},
    path::PathBuf,
    process::Child,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use uebox_godot_session::{EditorMode, GodotSession, LaunchOptions, SessionError};

static NEXT: AtomicU64 = AtomicU64::new(1);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "uebox-godot-managed-临时-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct OwnEditor {
    session: Option<GodotSession>,
    child: Option<Child>,
}
impl OwnEditor {
    fn session(&mut self) -> &mut GodotSession {
        self.session.as_mut().unwrap()
    }
    fn release(&mut self) {
        self.child = Some(self.session.take().unwrap().into_editor_process());
    }
}
impl Drop for OwnEditor {
    fn drop(&mut self) {
        let child = self
            .child
            .take()
            .or_else(|| self.session.take().map(GodotSession::into_editor_process));
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
#[test]
fn launches_real_editor_queries_its_scene_and_keeps_it_alive_after_release() {
    let fixture = Fixture::new();
    let project = fixture.0.join("selected project 世界");
    let home = fixture.0.join("isolated-home");
    fs::create_dir(&project).unwrap();
    for dir in ["appdata", "localappdata", "config", "data", "cache", "temp"] {
        fs::create_dir_all(home.join(dir)).unwrap();
    }
    let addon = project.join("addons/uebox_godot");
    fs::create_dir_all(&addon).unwrap();
    let existing_addon = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../integrations/godot/addons/uebox_godot");
    for file in ["plugin.cfg", "plugin.gd"] {
        fs::copy(existing_addon.join(file), addon.join(file)).unwrap();
    }
    let probe = project.join("addons/session_test_probe");
    fs::create_dir_all(&probe).unwrap();
    fs::write(probe.join("plugin.cfg"), "[plugin]\nname=\"Session test probe\"\ndescription=\"Temporary test fixture\"\nauthor=\"test\"\nversion=\"1\"\nscript=\"plugin.gd\"\n").unwrap();
    fs::write(probe.join("plugin.gd"), r#"@tool
extends EditorPlugin
func _enter_tree() -> void:
    var port_file := FileAccess.open("res://session-test-port.txt", FileAccess.WRITE)
    port_file.store_string(OS.get_environment("UEBOX_GODOT_PORT"))
    port_file.close()
    var dirs := FileAccess.open("res://session-test-dirs.json", FileAccess.WRITE)
    dirs.store_string(JSON.stringify({"config": OS.get_config_dir(), "data": OS.get_data_dir(), "cache": OS.get_cache_dir()}))
    dirs.close()
    call_deferred("_open_scene")
func _open_scene() -> void:
    EditorInterface.open_scene_from_path("res://main.tscn")
"#).unwrap();
    fs::write(project.join("project.godot"), "config_version=5\n[application]\nconfig/name=\"Managed real project\"\n[editor_plugins]\nenabled=PackedStringArray(\"res://addons/uebox_godot/plugin.cfg\", \"res://addons/session_test_probe/plugin.cfg\")\n").unwrap();
    fs::write(project.join("main.tscn"), "[gd_scene format=3]\n\n[node name=\"World\" type=\"Node2D\"]\n\n[node name=\"Player\" type=\"Node2D\" parent=\".\"]\n").unwrap();
    let project_before = fs::read(project.join("project.godot")).unwrap();
    let scene_before = fs::read(project.join("main.tscn")).unwrap();
    let executable = std::env::var_os("GODOT_BIN")
        .expect("GODOT_BIN must explicitly point to the real Godot executable");
    let options = LaunchOptions::new(executable, &project)
        .acknowledge_legacy_protocol_risks()
        .editor_mode(EditorMode::Headless)
        .isolate_editor_paths(&home)
        .query_timeout(Duration::from_secs(2));
    let mut editor = OwnEditor {
        session: Some(GodotSession::launch(options).unwrap()),
        child: None,
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    let info = loop {
        match editor.session().connect_once() {
            Ok(info) => break info,
            Err(SessionError::Disconnected | SessionError::Timeout)
                if Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(50))
            }
            Err(error) => panic!("real editor did not become ready: {error}"),
        }
    };
    assert_eq!(info.name, "Managed real project");
    assert!(
        info.godot_version.starts_with("4.7.2"),
        "{}",
        info.godot_version
    );
    let tree = loop {
        let tree = editor.session().scene_tree().unwrap();
        if tree.scene_path.as_deref() == Some("res://main.tscn") {
            break tree;
        }
        assert!(Instant::now() < deadline, "fixture scene was not opened");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        tree.nodes
            .iter()
            .map(|n| (&*n.path, &*n.name))
            .collect::<Vec<_>>(),
        vec![(".", "World"), ("Player", "Player")]
    );
    assert!(!tree.truncated);
    assert!(
        project.join(".godot").is_dir(),
        "Godot creates its editor cache in the selected project"
    );
    assert_eq!(
        fs::read(project.join("project.godot")).unwrap(),
        project_before
    );
    assert_eq!(fs::read(project.join("main.tscn")).unwrap(), scene_before);
    let dirs: Value =
        serde_json::from_slice(&fs::read(project.join("session-test-dirs.json")).unwrap()).unwrap();
    if cfg!(target_os = "macos") {
        assert_eq!(
            dirs["config"],
            home.join("Library/Application Support").to_str().unwrap()
        );
        assert_eq!(
            dirs["data"],
            home.join("Library/Application Support").to_str().unwrap()
        );
        assert_eq!(dirs["cache"], home.join("Library/Caches").to_str().unwrap());
    }
    let port: u16 = fs::read_to_string(project.join("session-test-port.txt"))
        .unwrap()
        .parse()
        .unwrap();
    editor.release();
    assert!(editor.child.as_mut().unwrap().try_wait().unwrap().is_none());
    assert!(
        TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            Duration::from_secs(1)
        )
        .is_ok(),
        "v1 plugin remains listening after manager release"
    );
}
