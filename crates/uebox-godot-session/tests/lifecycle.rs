use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use uebox_godot_session::{GodotSession, LaunchOptions, SessionError};

static NEXT: AtomicU64 = AtomicU64::new(1);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "uebox-godot-session-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(fs::canonicalize(root).unwrap())
    }
    fn project(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("project.godot"), b"config_version=5\n").unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn rejects_missing_or_non_absolute_projects() {
    let fixture = Fixture::new();
    let missing = fixture.0.join("missing");
    let no_project = fixture.0.join("no-project");
    fs::create_dir(&no_project).unwrap();
    let executable = std::env::current_exe().unwrap();
    for project in [&missing, &no_project, &PathBuf::from("relative")] {
        let result = GodotSession::launch(LaunchOptions::new(&executable, project));
        assert!(matches!(result, Err(SessionError::InvalidProject)));
    }
}

#[test]
fn rejects_an_unbounded_or_zero_query_timeout_before_launch() {
    let fixture = Fixture::new();
    let project = fixture.project("project");
    let executable = std::env::current_exe().unwrap();
    for duration in [Duration::ZERO, Duration::from_secs(31)] {
        let options = LaunchOptions::new(&executable, &project).query_timeout(duration);
        assert!(matches!(
            GodotSession::launch(options),
            Err(SessionError::InvalidTimeout)
        ));
    }
}

#[cfg(unix)]
#[test]
fn rejects_shell_wrappers_without_executing_them() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let project = fixture.project("project");
    let script = fixture.0.join("godot-wrapper");
    let marker = fixture.0.join("executed");
    fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}\u{27}\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        GodotSession::launch(LaunchOptions::new(&script, &project)),
        Err(SessionError::InvalidExecutable)
    ));
    assert!(!marker.exists());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn reports_os_spawn_failure_without_mislabeling_it_as_a_valid_editor() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let project = fixture.project("project");
    let executable = fixture.0.join("damaged-native-binary");
    let header: &[u8] = if cfg!(target_os = "macos") {
        &[0xcf, 0xfa, 0xed, 0xfe]
    } else {
        &[0x7f, b'E', b'L', b'F']
    };
    fs::write(&executable, header).unwrap();
    // The owner can read but cannot execute; a group execute bit still passes
    // the cheap metadata preflight, so the OS must report the spawn failure.
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o410)).unwrap();
    let options = LaunchOptions::new(&executable, &project).acknowledge_legacy_protocol_risks();
    match GodotSession::launch(options) {
        Err(SessionError::Spawn(kind)) => assert_eq!(kind, std::io::ErrorKind::PermissionDenied),
        Err(error) => panic!("expected spawn error, got {error:?}"),
        Ok(_) => panic!("non-executable binary was accepted by the OS"),
    }
    assert_eq!(
        fs::read(project.join("project.godot")).unwrap(),
        b"config_version=5\n"
    );
}

#[cfg(feature = "test-fixtures")]
mod process_tests {
    use super::*;
    use std::{process::Child, thread, time::Instant};
    use uebox_godot_session::{EditorMode, SessionState};

    struct Managed(Option<GodotSession>);
    impl Managed {
        fn session(&mut self) -> &mut GodotSession {
            self.0.as_mut().unwrap()
        }
        fn take_child(mut self) -> Child {
            self.0.take().unwrap().into_editor_process()
        }
    }
    impl Drop for Managed {
        fn drop(&mut self) {
            if let Some(session) = self.0.take() {
                let mut child = session.into_editor_process();
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    fn launch(project: &PathBuf, timeout: Duration) -> Managed {
        let executable = PathBuf::from(env!("CARGO_BIN_EXE_session-fixture-editor"));
        let options = LaunchOptions::new(executable, project)
            .acknowledge_legacy_protocol_risks()
            .editor_mode(EditorMode::Headless)
            .query_timeout(timeout);
        Managed(Some(GodotSession::launch(options).unwrap()))
    }

    #[test]
    fn v1_launch_requires_an_explicit_risk_acknowledgment() {
        let fixture = Fixture::new();
        let project = fixture.project("project");
        let executable = PathBuf::from(env!("CARGO_BIN_EXE_session-fixture-editor"));
        let options = LaunchOptions::new(executable, &project).editor_mode(EditorMode::Headless);
        assert!(matches!(
            GodotSession::launch(options),
            Err(SessionError::LegacyProtocolNotAcknowledged)
        ));
        assert!(!project.join(".fixture-ready").exists());
    }

    #[test]
    fn launches_with_separately_selected_private_config_and_temp_paths() {
        let fixture = Fixture::new();
        let project = fixture.project("project");
        let home = fixture.0.join("editor-home");
        for dir in ["appdata", "localappdata", "config", "data", "cache", "temp"] {
            fs::create_dir_all(home.join(dir)).unwrap();
        }
        fs::write(
            project.join(".fixture-expected-home"),
            home.to_str().unwrap(),
        )
        .unwrap();
        let executable = PathBuf::from(env!("CARGO_BIN_EXE_session-fixture-editor"));
        let options = LaunchOptions::new(executable, &project)
            .acknowledge_legacy_protocol_risks()
            .editor_mode(EditorMode::Headless)
            .isolate_editor_paths(&home);
        let _editor = Managed(Some(GodotSession::launch(options).unwrap()));
        wait_for_file(&project.join(".fixture-isolation"));
        assert_eq!(fs::read(project.join(".fixture-isolation")).unwrap(), b"ok");
    }
    fn wait_for_file(path: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.is_file() {
            assert!(
                Instant::now() < deadline,
                "fixture process did not initialize"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn wait_for_exit(session: &mut GodotSession) -> SessionState {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = session.status().unwrap();
            if matches!(state, SessionState::Exited(_)) {
                return state;
            }
            assert!(Instant::now() < deadline, "fixture process did not exit");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn launches_the_selected_project_without_a_separate_copy() {
        let fixture = Fixture::new();
        let project = fixture.project("selected project");
        let project_before = fs::read(project.join("project.godot")).unwrap();
        let executable = PathBuf::from(env!("CARGO_BIN_EXE_session-fixture-editor"));
        let options = LaunchOptions::new(executable, &project)
            .acknowledge_legacy_protocol_risks()
            .editor_mode(EditorMode::Headless);
        let mut editor = Managed(Some(GodotSession::launch(options).unwrap()));
        wait_for_file(&project.join(".fixture-ready"));
        assert_eq!(editor.session().project_path(), project.as_path());
        assert_eq!(
            editor.session().connect_once().unwrap().name,
            "Fixture project"
        );
        assert_eq!(
            fs::read(project.join("project.godot")).unwrap(),
            project_before
        );
        assert!(
            !project.join("addons").exists(),
            "session must not install plugins"
        );
    }

    #[cfg(unix)]
    #[test]
    fn launches_a_selected_project_with_a_linked_project_file() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let config_owner = fixture.project("config-owner");
        let project = fixture.0.join("selected project");
        fs::create_dir(&project).unwrap();
        symlink(
            config_owner.join("project.godot"),
            project.join("project.godot"),
        )
        .unwrap();
        let executable = PathBuf::from(env!("CARGO_BIN_EXE_session-fixture-editor"));
        let options = LaunchOptions::new(executable, &project)
            .acknowledge_legacy_protocol_risks()
            .editor_mode(EditorMode::Headless);
        let mut editor = Managed(Some(GodotSession::launch(options).unwrap()));
        wait_for_file(&project.join(".fixture-ready"));
        assert_eq!(
            editor.session().connect_once().unwrap().name,
            "Fixture project"
        );
    }

    #[test]
    fn allows_an_explicit_editor_home_that_contains_the_selected_project() {
        let fixture = Fixture::new();
        let project = fixture.project("selected project");
        let executable = PathBuf::from(env!("CARGO_BIN_EXE_session-fixture-editor"));
        let options = LaunchOptions::new(executable, &project)
            .acknowledge_legacy_protocol_risks()
            .editor_mode(EditorMode::Headless)
            .isolate_editor_paths(&fixture.0);
        let mut editor = Managed(Some(GodotSession::launch(options).unwrap()));
        wait_for_file(&project.join(".fixture-ready"));
        assert_eq!(editor.session().project_path(), project.as_path());
    }

    #[test]
    fn launches_with_fixed_arguments_private_child_environment_and_fresh_instances() {
        let fixture = Fixture::new();
        let first_project = fixture.project("project ' ; $(not-a-shell) 世界");
        let second_project = fixture.project("project two");
        std::env::set_var("UEBOX_SESSION_PRIVATE_SENTINEL", "do-not-inherit");
        let mut first = launch(&first_project, Duration::from_secs(1));
        let mut second = launch(&second_project, Duration::from_secs(1));
        std::env::remove_var("UEBOX_SESSION_PRIVATE_SENTINEL");
        wait_for_file(&first_project.join(".fixture-ready"));
        wait_for_file(&second_project.join(".fixture-ready"));
        assert_ne!(
            first.session().instance_id(),
            second.session().instance_id()
        );
        assert_eq!(first.session().project_path(), first_project.as_path());
        assert_eq!(first.session().status().unwrap(), SessionState::Starting);
        assert!(matches!(
            first.session().scene_tree(),
            Err(SessionError::NotReady)
        ));
        assert!(!first_project.join(".fixture-connections").exists());
        assert_eq!(
            first.session().connect_once().unwrap().name,
            "Fixture project"
        );
        assert_eq!(
            second.session().connect_once().unwrap().name,
            "Fixture project"
        );
    }

    #[test]
    fn reads_fixture_scene_and_never_reconnects_after_a_ready_session_disconnects() {
        let fixture = Fixture::new();
        let project = fixture.project("project");
        let mut editor = launch(&project, Duration::from_secs(1));
        wait_for_file(&project.join(".fixture-ready"));
        editor.session().connect_once().unwrap();
        let tree = editor.session().scene_tree().unwrap();
        assert_eq!(tree.scene_path.as_deref(), Some("res://main.tscn"));
        assert_eq!(tree.nodes[0].name, "World");
        fs::write(project.join(".fixture-disconnect"), b"").unwrap();
        assert!(matches!(
            editor.session().project_info(),
            Err(SessionError::Disconnected)
        ));
        assert_eq!(
            editor.session().status().unwrap(),
            SessionState::Disconnected
        );
        assert!(matches!(
            editor.session().project_info(),
            Err(SessionError::SessionClosed)
        ));
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            fs::read_to_string(project.join(".fixture-connections")).unwrap(),
            "3"
        );
    }

    #[test]
    fn initial_readiness_attempts_are_only_made_when_explicitly_requested() {
        let fixture = Fixture::new();
        let project = fixture.project("project");
        fs::write(project.join(".fixture-delayed-listen"), b"").unwrap();
        let mut editor = launch(&project, Duration::from_millis(200));
        assert!(matches!(
            editor.session().connect_once(),
            Err(SessionError::Disconnected)
        ));
        assert_eq!(editor.session().status().unwrap(), SessionState::Starting);
        wait_for_file(&project.join(".fixture-ready"));
        assert!(!project.join(".fixture-connections").exists());
        assert_eq!(
            editor.session().connect_once().unwrap().name,
            "Fixture project"
        );
        assert_eq!(
            fs::read_to_string(project.join(".fixture-connections")).unwrap(),
            "1"
        );
    }

    #[test]
    fn preserves_remote_size_error_without_turning_it_into_an_empty_scene() {
        let fixture = Fixture::new();
        let project = fixture.project("project");
        let mut editor = launch(&project, Duration::from_secs(1));
        wait_for_file(&project.join(".fixture-ready"));
        editor.session().connect_once().unwrap();
        fs::write(project.join(".fixture-remote-error"), b"").unwrap();
        assert!(matches!(
            editor.session().scene_tree(),
            Err(SessionError::ResponseTooLarge)
        ));
        assert_eq!(editor.session().status().unwrap(), SessionState::Ready);
    }

    #[test]
    fn session_checks_only_its_actual_private_values_and_fails_closed_after_disconnect() {
        let fixture = Fixture::new();
        let project = fixture.project("project");
        let mut editor = launch(&project, Duration::from_secs(1));
        wait_for_file(&project.join(".fixture-ready"));
        editor.session().connect_once().unwrap();
        let id = editor.session().instance_id().to_owned();
        assert!(editor
            .session()
            .contains_private_bridge_values(&format!("prefix-{id}-suffix")));
        assert!(!editor
            .session()
            .contains_private_bridge_values(&"c".repeat(64)));
        fs::write(project.join(".fixture-disconnect"), b"").unwrap();
        assert!(matches!(
            editor.session().project_info(),
            Err(SessionError::Disconnected)
        ));
        assert!(editor
            .session()
            .contains_private_bridge_values("unavailable"));
    }

    #[test]
    fn private_value_check_is_scoped_to_the_original_session() {
        let fixture = Fixture::new();
        let first_project = fixture.project("first");
        let second_project = fixture.project("second");
        let mut first = launch(&first_project, Duration::from_secs(1));
        let mut second = launch(&second_project, Duration::from_secs(1));
        wait_for_file(&first_project.join(".fixture-ready"));
        wait_for_file(&second_project.join(".fixture-ready"));
        first.session().connect_once().unwrap();
        second.session().connect_once().unwrap();
        let id = first.session().instance_id().to_owned();
        assert!(first.session().contains_private_bridge_values(&id));
        assert!(!second.session().contains_private_bridge_values(&id));
    }

    #[test]
    fn private_values_in_initial_project_info_are_not_returned_to_the_host() {
        let fixture = Fixture::new();
        for (index, marker) in [".fixture-private-id", ".fixture-private-token"]
            .iter()
            .enumerate()
        {
            let project = fixture.project(&format!("initial-{index}"));
            fs::write(project.join(marker), b"").unwrap();
            let mut editor = launch(&project, Duration::from_secs(1));
            wait_for_file(&project.join(".fixture-ready"));
            assert!(matches!(
                editor.session().connect_once(),
                Err(SessionError::PrivateValueInResult)
            ));
            assert_eq!(editor.session().status().unwrap(), SessionState::Starting);
            fs::remove_file(project.join(marker)).unwrap();
            assert!(editor.session().connect_once().is_ok());
        }
    }

    #[test]
    fn private_values_in_project_and_scene_are_not_returned_to_the_host() {
        const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let fixture = Fixture::new();
        let project = fixture.project("project");
        let mut editor = launch(&project, Duration::from_secs(1));
        wait_for_file(&project.join(".fixture-ready"));
        editor.session().connect_once().unwrap();
        for marker in [
            ".fixture-private-id",
            ".fixture-private-token",
            ".fixture-version-private-token",
        ] {
            fs::write(project.join(marker), b"").unwrap();
            assert!(matches!(
                editor.session().project_info(),
                Err(SessionError::PrivateValueInResult)
            ));
            fs::remove_file(project.join(marker)).unwrap();
        }
        for marker in [
            ".fixture-scene-private-id",
            ".fixture-scene-private-token",
            ".fixture-node-path-private-id",
            ".fixture-node-class-private-token",
        ] {
            fs::write(project.join(marker), b"").unwrap();
            assert!(matches!(
                editor.session().scene_tree(),
                Err(SessionError::PrivateValueInResult)
            ));
            fs::remove_file(project.join(marker)).unwrap();
        }
        assert_eq!(editor.session().status().unwrap(), SessionState::Ready);
        fs::write(project.join(".fixture-legitimate-hash"), b"").unwrap();
        assert_eq!(editor.session().project_info().unwrap().name, HASH);
        let scene = editor.session().scene_tree().unwrap();
        assert_eq!(scene.nodes[0].name, HASH);
        assert!(scene.truncated);
    }

    #[test]
    fn does_not_echo_untrusted_remote_error_text_or_token() {
        let fixture = Fixture::new();
        let project = fixture.project("project");
        let mut editor = launch(&project, Duration::from_secs(1));
        wait_for_file(&project.join(".fixture-ready"));
        editor.session().connect_once().unwrap();
        fs::write(project.join(".fixture-malicious-error"), b"").unwrap();
        let error = editor.session().scene_tree().unwrap_err();
        assert_eq!(format!("{error}"), "Godot session error: RemoteRejected");
        assert!(!format!("{error:?}").contains("UEBOX_TEST_UNTRUSTED_MSG"));
    }

    #[test]
    fn times_out_once_and_does_not_reconnect_after_ready() {
        let fixture = Fixture::new();
        let project = fixture.project("project");
        let mut editor = launch(&project, Duration::from_millis(100));
        wait_for_file(&project.join(".fixture-ready"));
        editor.session().connect_once().unwrap();
        fs::write(project.join(".fixture-hang"), b"").unwrap();
        let start = Instant::now();
        assert!(matches!(
            editor.session().project_info(),
            Err(SessionError::Timeout)
        ));
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(
            editor.session().status().unwrap(),
            SessionState::Disconnected
        );
        assert!(matches!(
            editor.session().project_info(),
            Err(SessionError::SessionClosed)
        ));
    }

    #[test]
    fn expired_startup_does_not_open_a_connection_or_restart_the_editor() {
        let fixture = Fixture::new();
        let project = fixture.project("project");
        let executable = PathBuf::from(env!("CARGO_BIN_EXE_session-fixture-editor"));
        let options = LaunchOptions::new(executable, &project)
            .acknowledge_legacy_protocol_risks()
            .editor_mode(EditorMode::Headless)
            .startup_timeout(Duration::from_millis(100));
        let mut editor = Managed(Some(GodotSession::launch(options).unwrap()));
        wait_for_file(&project.join(".fixture-ready"));
        thread::sleep(Duration::from_millis(150));
        assert!(matches!(
            editor.session().connect_once(),
            Err(SessionError::StartupTimeout)
        ));
        assert_eq!(
            editor.session().status().unwrap(),
            SessionState::StartupExpired
        );
        assert!(!project.join(".fixture-connections").exists());
    }

    #[test]
    fn observes_normal_and_unsuccessful_exits_without_killing_the_editor() {
        for (marker, code) in [(".fixture-exit", 0), (".fixture-error", 23)] {
            let fixture = Fixture::new();
            let project = fixture.project("project");
            let mut editor = launch(&project, Duration::from_secs(1));
            wait_for_file(&project.join(".fixture-ready"));
            fs::write(project.join(marker), b"").unwrap();
            match wait_for_exit(editor.session()) {
                SessionState::Exited(exit) => {
                    assert_eq!(exit.code, Some(code));
                    assert_eq!(exit.success, code == 0);
                }
                _ => unreachable!(),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn observes_signal_exit_from_its_own_fixture_process() {
        let fixture = Fixture::new();
        let project = fixture.project("project");
        let mut editor = launch(&project, Duration::from_secs(1));
        wait_for_file(&project.join(".fixture-ready"));
        fs::write(project.join(".fixture-signal"), b"").unwrap();
        match wait_for_exit(editor.session()) {
            SessionState::Exited(exit) => {
                assert!(!exit.success);
                assert_eq!(exit.signal, Some(15));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn releasing_management_retains_the_child_process() {
        let fixture = Fixture::new();
        let project = fixture.project("project");
        let editor = launch(&project, Duration::from_secs(1));
        wait_for_file(&project.join(".fixture-ready"));
        let mut child = editor.take_child();
        assert!(child.try_wait().unwrap().is_none());
        fs::write(project.join(".fixture-exit"), b"").unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn dropping_the_session_does_not_terminate_its_editor() {
        let fixture = Fixture::new();
        let project = fixture.project("project");
        let mut editor = launch(&project, Duration::from_secs(1));
        wait_for_file(&project.join(".fixture-ready"));
        drop(editor.0.take().unwrap());
        fs::write(project.join(".fixture-check-alive"), b"").unwrap();
        wait_for_file(&project.join(".fixture-alive-after-drop"));
        fs::write(project.join(".fixture-exit"), b"").unwrap();
        wait_for_file(&project.join(".fixture-finished"));
    }
}
