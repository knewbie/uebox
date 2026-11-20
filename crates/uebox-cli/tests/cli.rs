use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::Duration,
};

use serde_json::{json, Value};
use uebox_cli::{
    run, Capabilities, Connector, Dispatch, Engine, HostError, HostFuture, HostSession,
    OnlineProject, ReadTool, ToolReply,
};

#[derive(Debug, Eq, PartialEq)]
struct AskCall {
    model_profile_id: String,
    question: String,
    project_id: String,
    allowed_tools: Vec<ReadTool>,
}

#[derive(Default)]
struct Fixture {
    delay: Duration,
    generation: Option<&'static str>,
    bad_contract: bool,
    auth_failed: bool,
    model_error: Option<HostError>,
    default_endpoint: Option<&'static str>,
    projects: Option<Vec<OnlineProject>>,
    call_error: Option<HostError>,
    calls: Mutex<Vec<ReadTool>>,
    asks: Mutex<Vec<AskCall>>,
}

impl Connector for Fixture {
    fn default_endpoint(&self) -> Option<String> {
        self.default_endpoint.map(str::to_owned)
    }

    fn connect<'a>(&'a self, endpoint: &'a str) -> HostFuture<'a, Box<dyn HostSession + 'a>> {
        Box::pin(async move {
            assert_eq!(endpoint, "fixture-private-endpoint");
            if self.auth_failed {
                return Err(HostError::Authentication);
            }
            tokio::time::sleep(self.delay).await;
            Ok(Box::new(Session(self)) as Box<dyn HostSession>)
        })
    }
}

struct Session<'a>(&'a Fixture);

impl HostSession for Session<'_> {
    fn capabilities(&self) -> HostFuture<'_, Capabilities> {
        Box::pin(async move {
            Ok(Capabilities {
                cli_contract_version: 1,
                project_targeting: !self.0.bad_contract,
                public_results: true,
            })
        })
    }

    fn projects(&self) -> HostFuture<'_, Vec<OnlineProject>> {
        Box::pin(async move {
            tokio::time::sleep(self.0.delay).await;
            if let Some(projects) = &self.0.projects {
                return Ok(projects.clone());
            }
            Ok(self
                .0
                .generation
                .map(|generation| OnlineProject {
                    engine: Engine::Unreal,
                    name: "Fixture".into(),
                    path: fixture_path(),
                    connection_id: generation.into(),
                })
                .into_iter()
                .collect())
        })
    }

    fn tools(&self) -> HostFuture<'_, Vec<ReadTool>> {
        Box::pin(async move {
            tokio::time::sleep(self.0.delay).await;
            Ok(vec![ReadTool::UnrealProjectInfo, ReadTool::GodotSceneTree])
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
            dispatch.mark_dispatched();
            self.0.calls.lock().unwrap().push(tool);
            tokio::time::sleep(self.0.delay).await;
            if let Some(error) = self.0.call_error {
                return Err(error);
            }
            Ok(ToolReply {
                structured: json!({"name": "Fixture"}),
                content: Vec::new(),
            })
        })
    }

    fn ask<'a>(
        &'a self,
        model_profile_id: &'a str,
        question: &'a str,
        project: &'a OnlineProject,
        allowed_tools: &'a [ReadTool],
        dispatch: &'a Dispatch,
    ) -> HostFuture<'a, uebox_cli::AskReply> {
        Box::pin(async move {
            if let Some(error) = self.0.model_error {
                return Err(error);
            }
            self.0.asks.lock().unwrap().push(AskCall {
                model_profile_id: model_profile_id.to_owned(),
                question: question.to_owned(),
                project_id: project.connection_id.clone(),
                allowed_tools: allowed_tools.to_vec(),
            });
            dispatch.mark_dispatched();
            Ok(uebox_cli::AskReply {
                text: "Fixture answer".into(),
            })
        })
    }
}

#[cfg(windows)]
fn fixture_path() -> PathBuf {
    PathBuf::from(r"C:\uebox-cli-fixture\project")
}
#[cfg(not(windows))]
fn fixture_path() -> PathBuf {
    PathBuf::from("/uebox-cli-fixture/project")
}

fn decode(stdout: &str) -> Value {
    assert_eq!(stdout.lines().count(), 1);
    assert!(stdout.ends_with('\n'));
    let value: Value = serde_json::from_str(stdout).unwrap();
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["artifacts"], json!([]));
    assert!(value["warnings"].is_array());
    value
}

fn argv(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).into()).collect()
}

#[test]
fn binary_starts_with_an_empty_environment_and_reports_unreachable_endpoint() {
    let binary = env!("CARGO_BIN_EXE_uebox");
    for command in ["--help", "--version"] {
        let output = Command::new(binary)
            .env_clear()
            .arg(command)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
    let temp = tempfile::tempdir().unwrap();
    let missing = temp
        .path()
        .canonicalize()
        .unwrap()
        .join("secret-fixture-address");
    let output = Command::new(binary)
        .env_clear()
        .args(["doctor", "--endpoint", missing.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(decode(&stdout)["error"]["code"], "SERVICE_UNAVAILABLE");
    assert!(!stdout.contains("secret-fixture-address"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret-fixture-address"));
}

#[tokio::test]
async fn projects_catalog_show_and_call_use_only_typed_fixture_operations() {
    let fixture = Fixture {
        generation: Some("generation-1"),
        ..Fixture::default()
    };
    for (command, expected) in [
        (vec!["doctor"], "checks"),
        (vec!["projects", "list"], "projects"),
        (vec!["tools", "list", "--search", "unreal"], "tools"),
        (
            vec!["tools", "show", "--name", "ue_project_info"],
            "inputSchema",
        ),
        (
            vec!["tools", "call", "ue_project_info", "--args", "{}"],
            "structured",
        ),
    ] {
        let mut args = argv(&command);
        args.extend(argv(&["--endpoint=fixture-private-endpoint", "--json"]));
        let output = run(&args, Path::new(""), &fixture).await;
        let value = decode(&output.stdout);
        assert_eq!(output.exit_code, 0, "{}", output.stdout);
        assert!(value.get("error").is_none());
        assert!(value["data"].get(expected).is_some());
        if expected == "tools" {
            assert_eq!(value["data"]["returnedCount"], 1);
        }
        if expected == "checks" {
            assert_eq!(value["data"]["checks"].as_array().unwrap().len(), 4);
            assert!(output.stderr.contains("contract: ok"));
        }
    }
    assert_eq!(
        *fixture.calls.lock().unwrap(),
        [ReadTool::UnrealProjectInfo]
    );

    let empty = run(
        &argv(&["projects", "list", "--endpoint=fixture-private-endpoint"]),
        Path::new(""),
        &Fixture::default(),
    )
    .await;
    assert_eq!(empty.exit_code, 0);
    assert_eq!(
        decode(&empty.stdout)["data"],
        json!({"projects": [], "count": 0})
    );
}

#[tokio::test]
async fn ask_sends_only_registered_model_id_project_and_fixed_engine_read_tools() {
    let fixture = Fixture {
        generation: Some("generation-1"),
        ..Fixture::default()
    };
    let output = run(
        &argv(&[
            "ask",
            "Summarize",
            "the",
            "open",
            "project",
            "--model",
            "0123456789abcdef0123456789abcdef",
            "--endpoint",
            "fixture-private-endpoint",
            "--json",
        ]),
        Path::new(""),
        &fixture,
    )
    .await;

    assert_eq!(output.exit_code, 0, "{}", output.stdout);
    let value = decode(&output.stdout);
    assert_eq!(value["data"]["text"], "Fixture answer");
    assert_eq!(value["data"]["allowedTools"], json!(["ue_project_info"]));
    assert_eq!(
        *fixture.asks.lock().unwrap(),
        [AskCall {
            model_profile_id: "0123456789abcdef0123456789abcdef".into(),
            question: "Summarize the open project".into(),
            project_id: "generation-1".into(),
            allowed_tools: vec![ReadTool::UnrealProjectInfo]
        }]
    );
    assert!(output.stderr.contains("ask: dispatching to native host"));
    assert!(!output.stdout.contains("credential"));
}

#[tokio::test]
async fn ask_rejects_missing_or_malformed_model_id_before_connecting() {
    let fixture = Fixture {
        auth_failed: true,
        ..Fixture::default()
    };
    for args in [
        argv(&["ask", "question"]),
        argv(&["ask", "question", "--model", "not-a-profile"]),
    ] {
        let output = run(&args, Path::new(""), &fixture).await;
        assert_eq!(decode(&output.stdout)["error"]["code"], "INVALID_ARGUMENT");
        assert!(fixture.asks.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn ask_reports_unregistered_model_without_dispatch() {
    let fixture = Fixture {
        generation: Some("generation-1"),
        model_error: Some(HostError::ModelUnavailable),
        ..Fixture::default()
    };
    let output = run(
        &argv(&[
            "ask",
            "question",
            "--model",
            "0123456789abcdef0123456789abcdef",
            "--endpoint",
            "fixture-private-endpoint",
        ]),
        Path::new(""),
        &fixture,
    )
    .await;
    assert_eq!(decode(&output.stdout)["error"]["code"], "MODEL_UNAVAILABLE");
    assert_eq!(decode(&output.stdout)["error"]["execution"], "not_started");
}

#[test]
fn engine_ask_allowlists_are_closed_and_never_include_screenshot_tools() {
    assert_eq!(
        ReadTool::allowlist_for(Engine::Unreal),
        [ReadTool::UnrealProjectInfo]
    );
    assert_eq!(
        ReadTool::allowlist_for(Engine::Unity),
        [ReadTool::UnityProjectInfo, ReadTool::UnityActiveScene]
    );
    assert_eq!(
        ReadTool::allowlist_for(Engine::Godot),
        [ReadTool::GodotProjectInfo, ReadTool::GodotSceneTree]
    );
}

#[tokio::test]
async fn forbidden_names_payloads_and_unknown_capabilities_fail_without_dispatch() {
    let fixture = Fixture {
        auth_failed: true,
        ..Fixture::default()
    };
    for (request, expected) in [
        (vec!["tools", "call", "shell"], "TOOL_UNAVAILABLE"),
        (
            vec!["tools", "call", "ue_project_info", "--allow-write"],
            "RISK_NOT_SUPPORTED",
        ),
        (
            vec![
                "tools",
                "call",
                "ue_project_info",
                "--args",
                "{\"private-value\":42}",
            ],
            "INVALID_ARGUMENT",
        ),
        (
            vec![
                "tools",
                "call",
                "ue_project_info",
                "--args",
                "fixture-bearer",
            ],
            "INVALID_ARGUMENT",
        ),
        (vec!["doctor"], "AUTH_FAILED"),
    ] {
        let mut args = argv(&request);
        args.extend(argv(&["--endpoint", "fixture-private-endpoint"]));
        let output = run(&args, Path::new(""), &fixture).await;
        let error = &decode(&output.stdout)["error"];
        assert_eq!(error["code"], expected);
        assert_eq!(error["execution"], "not_started");
        assert!(!format!("{}{}", output.stdout, output.stderr).contains("fixture-"));
    }
    let fixture = Fixture {
        bad_contract: true,
        ..Fixture::default()
    };
    let output = run(
        &argv(&["projects", "list", "--endpoint=fixture-private-endpoint"]),
        Path::new(""),
        &fixture,
    )
    .await;
    assert_eq!(
        decode(&output.stdout)["error"]["code"],
        "INCOMPATIBLE_SERVER"
    );
    assert!(fixture.calls.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn one_total_deadline_covers_connection_and_queries_with_dispatch_state() {
    for (ms, expected, calls) in [(400, "not_started", 0), (300, "unknown", 1)] {
        let fixture = Fixture {
            delay: Duration::from_millis(ms),
            generation: Some("generation-1"),
            ..Fixture::default()
        };
        let start = tokio::time::Instant::now();
        let output = run(
            &argv(&[
                "tools",
                "call",
                "ue_project_info",
                "--endpoint=fixture-private-endpoint",
                "--timeout",
                "1",
            ]),
            Path::new(""),
            &fixture,
        )
        .await;
        assert_eq!(start.elapsed(), Duration::from_secs(1));
        assert_eq!(output.exit_code, 7);
        assert_eq!(decode(&output.stdout)["error"]["execution"], expected);
        assert_eq!(fixture.calls.lock().unwrap().len(), calls);
    }
}

#[tokio::test]
async fn a_local_project_hint_pins_a_generation_and_never_falls_back_to_another_project() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let online_path = root.join("online project");
    let offline_path = root.join("offline project");
    let subdirectory = online_path.join("Content");
    fs::create_dir_all(&subdirectory).unwrap();
    fs::create_dir_all(&offline_path).unwrap();
    fs::write(online_path.join("Demo.uproject"), "{}").unwrap();
    fs::write(offline_path.join("Offline.uproject"), "{}").unwrap();
    fs::write(root.join("query.json"), b"\xef\xbb\xbf{}\n").unwrap();
    let fixture = Fixture {
        projects: Some(vec![OnlineProject {
            engine: Engine::Unreal,
            name: "Fixture".into(),
            path: online_path.clone(),
            connection_id: "generation-1".into(),
        }]),
        ..Fixture::default()
    };
    let command = [
        "tools",
        "call",
        "ue_project_info",
        "--endpoint=fixture-private-endpoint",
    ];
    let mut explicit = argv(&command);
    explicit.extend(argv(&["--project", "offline project/Offline.uproject"]));
    let failure = run(&explicit, &root, &fixture).await;
    assert_eq!(
        decode(&failure.stdout)["error"]["code"],
        "PROJECT_NOT_CONNECTED"
    );
    assert!(fixture.calls.lock().unwrap().is_empty());

    let mut from_cwd = argv(&command);
    from_cwd.extend(argv(&["--args-file", "../../query.json"]));
    let success = run(&from_cwd, &subdirectory, &fixture).await;
    assert_eq!(success.exit_code, 0);
    assert_eq!(decode(&success.stdout)["project"]["name"], "Fixture");

    fs::write(online_path.join("Other.uproject"), "{}").unwrap();
    let mut ambiguous = argv(&command);
    ambiguous.extend(argv(&["--project", "online project/Other.uproject"]));
    let failure = run(&ambiguous, &root, &fixture).await;
    assert_eq!(
        decode(&failure.stdout)["error"]["code"],
        "PROJECT_AMBIGUOUS"
    );
    assert_eq!(fixture.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_lost_read_is_unknown_and_is_never_retried() {
    let fixture = Fixture {
        generation: Some("generation-1"),
        call_error: Some(HostError::Unavailable),
        ..Fixture::default()
    };
    let output = run(
        &argv(&[
            "tools",
            "call",
            "ue_project_info",
            "--endpoint=fixture-private-endpoint",
        ]),
        Path::new(""),
        &fixture,
    )
    .await;
    assert_eq!(output.exit_code, 4);
    assert_eq!(decode(&output.stdout)["error"]["execution"], "unknown");
    assert_eq!(fixture.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn endpoint_override_precedes_native_discovery() {
    let args = ["projects".into(), "list".into(), "--json".into()];
    let missing = run(&args, Path::new("."), &Fixture::default()).await;
    assert_eq!(decode(&missing.stdout)["error"]["code"], "CONFIG_MISSING");

    let candidate = "fixture-private-endpoint";
    let fixture = Fixture {
        default_endpoint: Some(candidate),
        ..Fixture::default()
    };
    let result = run(&args, Path::new("."), &fixture).await;
    assert_eq!(result.exit_code, 0);

    let fixture = Fixture {
        default_endpoint: Some("should-not-be-used"),
        ..Fixture::default()
    };
    let mut args = args.to_vec();
    args.extend(["--endpoint".into(), candidate.into()]);
    let result = run(&args, Path::new("."), &fixture).await;
    assert_eq!(result.exit_code, 0);
}
