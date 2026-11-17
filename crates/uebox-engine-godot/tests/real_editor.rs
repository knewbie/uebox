//! Explicit opt-in: this test launches only its own temporary Godot projects.
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use uebox_engine_godot::{Credentials, Error, GodotClient, Target};
#[path = "support/environment.rs"]
mod environment;
#[path = "support/host.rs"]
mod host;
#[path = "support/paths.rs"]
mod paths;

struct Editor {
    dir: PathBuf,
    child: Child,
    address: SocketAddr,
    credentials: Credentials,
    owns_dir: bool,
    log_path: PathBuf,
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn prepare_home(home: &std::path::Path) {
    for dir in ["appdata", "localappdata", "config", "data", "cache", "temp"] {
        fs::create_dir_all(home.join(dir)).unwrap();
    }
}

impl Editor {
    fn start(name: &str) -> Self {
        Self::start_with(name, true, true)
    }
    fn start_large_scene() -> Self {
        let mut scene =
            String::from("[gd_scene format=3]\n\n[node name=\"World\" type=\"Node2D\"]\n");
        for index in 0..2050 {
            scene.push_str(&format!(
                "\n[node name=\"Child{index}\" type=\"Node\" parent=\".\"]\n"
            ));
        }
        Self::start_with_scene("Large temporary scene", true, true, Some(scene))
    }
    fn start_deep_scene() -> Self {
        let mut scene =
            String::from("[gd_scene format=3]\n\n[node name=\"World\" type=\"Node2D\"]\n");
        let name = "N".repeat(128);
        let mut parent = String::from(".");
        for _ in 0..160 {
            scene.push_str(&format!(
                "\n[node name=\"{name}\" type=\"Node\" parent=\"{parent}\"]\n"
            ));
            parent = if parent == "." {
                name.clone()
            } else {
                format!("{parent}/{name}")
            };
        }
        Self::start_with_scene("Deep temporary scene", true, true, Some(scene))
    }
    fn start_wide_unicode_scene() -> Self {
        let mut scene =
            String::from("[gd_scene format=3]\n\n[node name=\"World\" type=\"Node\"]\n");
        for index in 0..1100 {
            let name = format!("{}{index}", "界".repeat(220));
            scene.push_str(&format!(
                "\n[node name=\"{name}\" type=\"Node\" parent=\".\"]\n"
            ));
        }
        Self::start_with_scene("Wide Unicode scene", true, true, Some(scene))
    }
    fn start_with(name: &str, configured: bool, open_scene: bool) -> Self {
        Self::start_with_scene(name, configured, open_scene, None)
    }
    fn start_with_scene(
        name: &str,
        configured: bool,
        open_scene: bool,
        scene: Option<String>,
    ) -> Self {
        let executable =
            std::env::var_os("GODOT_BIN").expect("GODOT_BIN must point to a real Godot 4 editor");
        let credentials = Credentials::generate().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "uebox-godot-临时 project-{}-{}",
            std::process::id(),
            credentials.instance_id
        ));
        fs::create_dir(&dir).unwrap();
        let dir = fs::canonicalize(dir).unwrap();
        let addon_dir = dir.join("addons/uebox_godot");
        fs::create_dir_all(&addon_dir).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../integrations/godot/addons/uebox_godot");
        for file in ["plugin.cfg", "plugin.gd"] {
            fs::copy(source.join(file), addon_dir.join(file)).unwrap();
        }
        let probe = dir.join("addons/uebox_test_probe");
        fs::create_dir_all(&probe).unwrap();
        fs::write(probe.join("plugin.cfg"), "[plugin]\nname=\"Fixture directory probe\"\ndescription=\"Reports isolated test paths\"\nauthor=\"test\"\nversion=\"1\"\nscript=\"plugin.gd\"\n").unwrap();
        fs::write(probe.join("plugin.gd"), "@tool\nextends EditorPlugin\nfunc _enter_tree() -> void:\n    print(\"UEBOX_TEST_DIRS=\" + JSON.stringify({\"config\": OS.get_config_dir(), \"data\": OS.get_data_dir(), \"cache\": OS.get_cache_dir()}))\n").unwrap();
        fs::write(dir.join("project.godot"), format!("config_version=5\n[application]\nconfig/name=\"{name}\"\n[editor_plugins]\nenabled=PackedStringArray(\"res://addons/uebox_godot/plugin.cfg\", \"res://addons/uebox_test_probe/plugin.cfg\")\n")).unwrap();
        let scene = scene.unwrap_or_else(|| "[gd_scene format=3]\n\n[node name=\"World\" type=\"Node2D\"]\n\n[node name=\"Player\" type=\"Node2D\" parent=\".\"]\n\n[node name=\"Camera\" type=\"Camera2D\" parent=\"Player\"]\n".into());
        fs::write(dir.join("main.tscn"), scene).unwrap();
        let home = dir.join("home");
        prepare_home(&home);
        let port = free_port();
        let log_path = dir.join("editor.log");
        let log = fs::File::create(&log_path).unwrap();
        let mut command = Command::new(executable);
        command
            .args(["--headless", "--editor", "--path"])
            .arg(host::godot_host_path(&dir))
            .args([
                "--lsp-port",
                &free_port().to_string(),
                "--dap-port",
                &free_port().to_string(),
            ])
            .env("UEBOX_GODOT_PORT", port.to_string())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log));
        environment::configure_isolated_environment(&mut command, &host::godot_host_path(&home));
        if configured {
            command
                .env("UEBOX_GODOT_INSTANCE_ID", &credentials.instance_id)
                .env("UEBOX_GODOT_TOKEN", &credentials.token);
        } else {
            command
                .env_remove("UEBOX_GODOT_INSTANCE_ID")
                .env_remove("UEBOX_GODOT_TOKEN");
        }
        if open_scene {
            command.arg("res://main.tscn");
        }
        let child = command.spawn().expect("failed to start Godot editor");
        Self {
            dir,
            child,
            address: SocketAddr::from(([127, 0, 0, 1], port)),
            credentials,
            owns_dir: true,
            log_path,
        }
    }
    fn start_same_project(&self) -> Self {
        let executable =
            std::env::var_os("GODOT_BIN").expect("GODOT_BIN must point to a real Godot 4 editor");
        let credentials = Credentials::generate().unwrap();
        let home = self.dir.join(format!("home-{}", credentials.instance_id));
        prepare_home(&home);
        let port = free_port();
        let log_path = self
            .dir
            .join(format!("editor-{}.log", credentials.instance_id));
        let log = fs::File::create(&log_path).unwrap();
        let mut command = Command::new(executable);
        command
            .args(["--headless", "--editor", "--path"])
            .arg(host::godot_host_path(&self.dir))
            .args([
                "--lsp-port",
                &free_port().to_string(),
                "--dap-port",
                &free_port().to_string(),
            ])
            .env("UEBOX_GODOT_PORT", port.to_string())
            .env("UEBOX_GODOT_INSTANCE_ID", &credentials.instance_id)
            .env("UEBOX_GODOT_TOKEN", &credentials.token)
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log));
        environment::configure_isolated_environment(&mut command, &host::godot_host_path(&home));
        let child = command
            .spawn()
            .expect("failed to start second Godot editor");
        Self {
            dir: self.dir.clone(),
            child,
            address: SocketAddr::from(([127, 0, 0, 1], port)),
            credentials,
            owns_dir: false,
            log_path,
        }
    }
    fn client(&self) -> GodotClient {
        GodotClient::new(
            Target::new(
                self.address,
                &self.dir,
                &self.credentials.instance_id,
                &self.credentials.token,
                Duration::from_millis(500),
            )
            .unwrap(),
        )
    }
    fn wait_for_project(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match self.client().project_info() {
                Ok(_) => return,
                Err(Error::Disconnected) | Err(Error::Timeout) if Instant::now() < deadline => {
                    if let Some(status) = self.child.try_wait().unwrap() {
                        panic!(
                            "Godot exited: {status}; log: {}",
                            fs::read_to_string(&self.log_path).unwrap()
                        );
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                Err(error) => panic!(
                    "editor did not respond: {error}; log: {}",
                    fs::read_to_string(&self.log_path).unwrap()
                ),
            }
        }
    }
    fn raw(&self, token: &str, project_path: &str, instance_id: &str, command: &str) -> Value {
        let mut stream = TcpStream::connect_timeout(&self.address, Duration::from_secs(1)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let request = json!({"version": 1, "request_id": "real-test", "token": token,
            "target": {"project_path": project_path, "instance_id": instance_id}, "command": command});
        writeln!(stream, "{request}").unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        serde_json::from_str(&response).unwrap()
    }
}
impl Drop for Editor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.owns_dir {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

#[test]
fn actual_godot_editors_enforce_identity_authentication_and_read_only_queries() {
    let mut first = Editor::start("Isolated Godot Project A");
    first.wait_for_project();
    let project_before = fs::read(first.dir.join("project.godot")).unwrap();
    let scene_before = fs::read(first.dir.join("main.tscn")).unwrap();
    let info = first.client().project_info().unwrap();
    assert_eq!(info.name, "Isolated Godot Project A");
    assert!(
        info.godot_version.starts_with("4.7.2"),
        "{}",
        info.godot_version
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let tree = loop {
        let result = first.client().scene_tree().unwrap();
        if result.scene_path.as_deref() == Some("res://main.tscn") {
            break result;
        }
        assert!(Instant::now() < deadline, "scene did not open: {result:?}");
        thread::sleep(Duration::from_millis(50));
    };
    assert!(!tree.truncated);
    assert_eq!(
        tree.nodes
            .iter()
            .map(|node| (&*node.path, &*node.class))
            .collect::<Vec<_>>(),
        vec![
            (".", "Node2D"),
            ("Player", "Node2D"),
            ("Player/Camera", "Camera2D")
        ]
    );

    let mut second = Editor::start_with("Isolated Godot Project B", true, false);
    second.wait_for_project();
    assert_eq!(
        second.client().project_info().unwrap().name,
        "Isolated Godot Project B"
    );
    let no_scene = second.client().scene_tree().unwrap();
    assert_eq!(no_scene.scene_path, None);
    assert!(no_scene.nodes.is_empty());
    let first_path = paths::godot_wire_path(&first.dir);
    let second_path = paths::godot_wire_path(&second.dir);
    let baseline = first.raw(
        &first.credentials.token,
        &first_path,
        &first.credentials.instance_id,
        "project.info",
    );
    assert_eq!(baseline["result"]["name"], "Isolated Godot Project A");
    let invalid_token = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    for (token, path, id, command, expected) in [
        (
            invalid_token,
            first_path.as_str(),
            &*first.credentials.instance_id,
            "project.info",
            "unauthorized",
        ),
        (
            &*first.credentials.token,
            second_path.as_str(),
            &*first.credentials.instance_id,
            "project.info",
            "wrong_target",
        ),
        (
            &*first.credentials.token,
            first_path.as_str(),
            &*second.credentials.instance_id,
            "project.info",
            "wrong_target",
        ),
        (
            &*first.credentials.token,
            first_path.as_str(),
            &*first.credentials.instance_id,
            "scene.write",
            "unknown_command",
        ),
        (
            &*first.credentials.token,
            first_path.as_str(),
            &*first.credentials.instance_id,
            "does.not.exist",
            "unknown_command",
        ),
    ] {
        let response = first.raw(token, path, id, command);
        assert_eq!(response["error"]["code"], expected, "{response}");
        assert!(
            response.get("result").is_none(),
            "rejected request returned data: {response}"
        );
        assert!(
            response.get("identity").is_none(),
            "rejected request leaked identity: {response}"
        );
    }
    assert_eq!(
        first.client().project_info().unwrap().name,
        "Isolated Godot Project A"
    );
    assert_eq!(
        second.client().project_info().unwrap().name,
        "Isolated Godot Project B"
    );
    assert_eq!(
        fs::read(first.dir.join("project.godot")).unwrap(),
        project_before
    );
    assert_eq!(fs::read(first.dir.join("main.tscn")).unwrap(), scene_before);
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    let result = first.client().project_info();
    assert!(matches!(result, Err(Error::Disconnected)), "{result:?}");
}

#[test]
fn addon_does_not_listen_without_explicit_credentials() {
    let mut editor = Editor::start_with("Unconfigured isolated project", false, false);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let log = fs::read_to_string(editor.dir.join("editor.log")).unwrap();
        if log.contains("loading_editor_layout") {
            break;
        }
        if let Some(status) = editor.child.try_wait().unwrap() {
            panic!("Godot exited: {status}; log: {log}");
        }
        assert!(Instant::now() < deadline, "Godot did not initialize: {log}");
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        TcpStream::connect_timeout(&editor.address, Duration::from_millis(250)).is_err(),
        "unconfigured addon unexpectedly listened"
    );
}

#[test]
fn editors_of_the_same_project_require_the_correct_instance() {
    let mut first = Editor::start("Shared temporary project");
    first.wait_for_project();
    let mut second = first.start_same_project();
    second.wait_for_project();
    assert_ne!(
        first.credentials.instance_id,
        second.credentials.instance_id
    );
    assert_ne!(first.address, second.address);
    assert_eq!(
        first.client().project_info().unwrap().name,
        "Shared temporary project"
    );
    assert_eq!(
        second.client().project_info().unwrap().name,
        "Shared temporary project"
    );
    let wrong = first.raw(
        &first.credentials.token,
        &paths::godot_wire_path(&first.dir),
        &second.credentials.instance_id,
        "project.info",
    );
    assert_eq!(wrong["error"]["code"], "wrong_target");
    assert!(wrong.get("result").is_none());
    drop(second);
    assert_eq!(
        first.client().project_info().unwrap().name,
        "Shared temporary project"
    );
}

#[test]
fn real_scene_query_marks_a_tree_over_the_limit_as_truncated() {
    let mut editor = Editor::start_large_scene();
    editor.wait_for_project();
    let deadline = Instant::now() + Duration::from_secs(15);
    let tree = loop {
        let tree = editor.client().scene_tree().unwrap();
        if tree.scene_path.as_deref() == Some("res://main.tscn") {
            break tree;
        }
        assert!(Instant::now() < deadline, "large scene did not open");
        thread::sleep(Duration::from_millis(50));
    };
    assert!(tree.truncated);
    assert_eq!(tree.nodes.len(), 2048);
    assert_eq!(tree.nodes[0].name, "World");
}

#[test]
fn deep_long_paths_are_truncated_before_exceeding_response_budget() {
    let mut editor = Editor::start_deep_scene();
    editor.wait_for_project();
    let deadline = Instant::now() + Duration::from_secs(20);
    let tree = loop {
        let tree = editor
            .client()
            .scene_tree()
            .expect("deep scene should return a bounded result");
        if tree.scene_path.as_deref() == Some("res://main.tscn") {
            break tree;
        }
        assert!(Instant::now() < deadline, "deep scene did not open");
        thread::sleep(Duration::from_millis(50));
    };
    assert!(tree.truncated);
    assert!(tree.nodes.len() > 1 && tree.nodes.len() < 161);
    assert!(tree.nodes.iter().all(|node| node.path.len() <= 8192));
    assert_eq!(
        editor.client().project_info().unwrap().name,
        "Deep temporary scene"
    );
}

#[test]
fn unicode_scene_nodes_are_truncated_by_encoded_response_budget() {
    let mut editor = Editor::start_wide_unicode_scene();
    editor.wait_for_project();
    let deadline = Instant::now() + Duration::from_secs(20);
    let tree = loop {
        let tree = editor
            .client()
            .scene_tree()
            .expect("wide scene should return a bounded result");
        if tree.scene_path.as_deref() == Some("res://main.tscn") {
            break tree;
        }
        assert!(Instant::now() < deadline, "wide scene did not open");
        thread::sleep(Duration::from_millis(50));
    };
    assert!(tree.truncated);
    assert!(
        tree.nodes.len() > 100 && tree.nodes.len() < 1101,
        "{}",
        tree.nodes.len()
    );
    let encoded_bytes: usize = tree
        .nodes
        .iter()
        .map(|node| {
            serde_json::to_vec(&json!({"path": node.path, "name": node.name, "class": node.class}))
                .unwrap()
                .len()
                + 1
        })
        .sum();
    assert!(
        encoded_bytes <= 524_288,
        "encoded node data is {encoded_bytes} bytes"
    );
    assert_eq!(
        editor.client().project_info().unwrap().name,
        "Wide Unicode scene"
    );
}

#[test]
fn real_editors_report_private_config_data_and_cache_directories() {
    fn reported(editor: &Editor) -> Value {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let log = fs::read_to_string(&editor.log_path).unwrap();
            if let Some(line) = log
                .lines()
                .find_map(|line| line.strip_prefix("UEBOX_TEST_DIRS="))
            {
                return serde_json::from_str(line).unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "Godot did not report its directories: {log}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
    fn check(dirs: Value, home: &std::path::Path) {
        let (config, data, cache) = if cfg!(target_os = "windows") {
            (
                home.join("appdata"),
                home.join("appdata"),
                home.join("localappdata"),
            )
        } else if cfg!(target_os = "macos") {
            (
                home.join("Library/Application Support"),
                home.join("Library/Application Support"),
                home.join("Library/Caches"),
            )
        } else {
            (home.join("config"), home.join("data"), home.join("cache"))
        };
        assert_eq!(dirs["config"], paths::godot_wire_path(&config));
        assert_eq!(dirs["data"], paths::godot_wire_path(&data));
        assert_eq!(dirs["cache"], paths::godot_wire_path(&cache));
    }
    let mut first = Editor::start("Directory isolation test");
    first.wait_for_project();
    check(reported(&first), &first.dir.join("home"));
    let mut second = first.start_same_project();
    second.wait_for_project();
    check(
        reported(&second),
        &first
            .dir
            .join(format!("home-{}", second.credentials.instance_id)),
    );
}

#[cfg(unix)]
#[test]
fn unsafe_launch_path_must_not_identify_a_different_nested_project() {
    let credentials = Credentials::generate().unwrap();
    let root =
        std::env::temp_dir().join(format!("uebox-godot-ambiguous-{}", credentials.instance_id));
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let _cleanup = Cleanup(root.clone());
    let literal = root.join(r"A\B");
    let nested = root.join("A").join("B");
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../integrations/godot/addons/uebox_godot");
    for (dir, name) in [
        (&literal, "Literal backslash project"),
        (&nested, "Nested project"),
    ] {
        let addon = dir.join("addons/uebox_godot");
        fs::create_dir_all(&addon).unwrap();
        for file in ["plugin.cfg", "plugin.gd"] {
            fs::copy(source.join(file), addon.join(file)).unwrap();
        }
        fs::write(dir.join("project.godot"), format!("config_version=5\n[application]\nconfig/name=\"{name}\"\n[editor_plugins]\nenabled=PackedStringArray(\"res://addons/uebox_godot/plugin.cfg\")\n")).unwrap();
    }
    let home = root.join("home");
    prepare_home(&home);
    let address = SocketAddr::from(([127, 0, 0, 1], free_port()));
    let log_path = root.join("editor.log");
    let log = fs::File::create(&log_path).unwrap();
    let mut command = Command::new(std::env::var_os("GODOT_BIN").unwrap());
    command
        .args(["--headless", "--editor", "--path"])
        .arg(&literal)
        .args([
            "--lsp-port",
            &free_port().to_string(),
            "--dap-port",
            &free_port().to_string(),
        ])
        .env("UEBOX_GODOT_PORT", address.port().to_string())
        .env("UEBOX_GODOT_INSTANCE_ID", &credentials.instance_id)
        .env("UEBOX_GODOT_TOKEN", &credentials.token)
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log));
    environment::configure_isolated_environment(&mut command, &home);
    let child = command.spawn().unwrap();
    let mut editor = Editor {
        dir: nested,
        child,
        address,
        credentials,
        owns_dir: false,
        log_path,
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
            panic!("ambiguous launch path must not open the editor bridge");
        }
        let log = fs::read_to_string(&editor.log_path).unwrap();
        if log.contains("UEBox Godot bridge cannot verify the actual project directory") {
            break;
        }
        if let Some(status) = editor.child.try_wait().unwrap() {
            panic!("Godot exited: {status}; log: {log}");
        }
        assert!(
            Instant::now() < deadline,
            "expected project identity rejection; log: {log}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}
