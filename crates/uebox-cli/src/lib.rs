//! Native Unreal Box console contract.

mod args;
mod host;
mod output;
mod project;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod unix;

use std::{
    collections::HashSet,
    fs::{self, File},
    future::Future,
    io::{self, Read},
    path::{Path, PathBuf},
};

use serde_json::{json, Value};
use tokio::{
    sync::oneshot,
    time::{timeout_at, Instant},
};

use args::{Arguments, Command, Options};
use output::Fault;

pub use host::{
    Capabilities, Connector, Dispatch, Engine, HostError, HostFuture, HostSession, OnlineProject,
    ReadTool, ToolReply, UnavailableConnector, CLI_CONTRACT_VERSION,
};
pub use output::RunOutput;

const MAX_ARGS_BYTES: usize = 1024 * 1024;

/// Run a console invocation against an explicit connector. Never discovers an Electron profile.
/// Call within a Tokio runtime with its timer enabled. Results and diagnostics are returned separately.
pub async fn run(argv: &[String], cwd: &Path, connector: &dyn Connector) -> RunOutput {
    let started = Instant::now();
    let options = match args::parse(argv) {
        Ok(options) => options,
        Err(fault) => return fault.output(String::new()),
    };
    let deadline = Deadline(started + options.timeout);
    let mut stderr = String::new();
    match options.command {
        Command::Help => {
            let help = args::help(options.english);
            if options.json {
                output::success(json!({ "help": help }), None, &[], stderr)
            } else {
                RunOutput {
                    exit_code: 0,
                    stdout: help,
                    stderr,
                }
            }
        }
        Command::Version => {
            let version = env!("CARGO_PKG_VERSION");
            if options.json {
                output::success(json!({ "version": version }), None, &[], stderr)
            } else {
                RunOutput {
                    exit_code: 0,
                    stdout: format!("{version}\n"),
                    stderr,
                }
            }
        }
        _ => match execute(options, cwd, connector, deadline, &mut stderr).await {
            Ok(outcome) => output::success(
                outcome.data,
                outcome.project.as_ref(),
                &outcome.warnings,
                stderr,
            ),
            Err(fault) => fault.output(stderr),
        },
    }
}

#[derive(Clone, Copy)]
struct Deadline(Instant);

impl Deadline {
    async fn within<T>(
        self,
        work: impl Future<Output = Result<T, Fault>>,
        timeout: Fault,
    ) -> Result<T, Fault> {
        if Instant::now() >= self.0 {
            return Err(timeout);
        }
        let result = timeout_at(self.0, work).await.map_err(|_| timeout)?;
        if Instant::now() >= self.0 {
            return Err(timeout);
        }
        result
    }

    async fn host<T>(self, work: impl Future<Output = Result<T, HostError>>) -> Result<T, Fault> {
        self.within(
            async { work.await.map_err(|error| Fault::host(error, false)) },
            Fault::TimeoutBeforeDispatch,
        )
        .await
    }

    async fn local<T: Send + 'static>(
        self,
        work: impl FnOnce() -> Result<T, Fault> + Send + 'static,
    ) -> Result<T, Fault> {
        self.within(
            blocking(work, Fault::InvalidArgument),
            Fault::TimeoutBeforeDispatch,
        )
        .await
    }

    async fn read_tool(
        self,
        host: &dyn HostSession,
        tool: ReadTool,
        project: &OnlineProject,
    ) -> Result<ToolReply, Fault> {
        let dispatch = Dispatch::default();
        let receipt = self
            .within(
                async { Ok(host.call(tool, project, &dispatch).await) },
                Fault::TimeoutBeforeDispatch,
            )
            .await;
        let dispatched = dispatch.is_dispatched();
        match receipt {
            Ok(result) => result.map_err(|error| Fault::host(error, dispatched)),
            Err(_) if dispatched => Err(Fault::TimeoutAfterDispatch),
            Err(error) => Err(error),
        }
    }
}

// A blocked filesystem or stdin cannot hold the runtime open after the command deadline.
async fn blocking<T: Send + 'static, E: Copy + Send + 'static>(
    work: impl FnOnce() -> Result<T, E> + Send + 'static,
    failure: E,
) -> Result<T, E> {
    let (send, recv) = oneshot::channel();
    std::thread::Builder::new()
        .name("uebox-input".into())
        .spawn(move || {
            let _ = send.send(work());
        })
        .map_err(|_| failure)?;
    recv.await.map_err(|_| failure)?
}

struct Outcome {
    data: Value,
    project: Option<OnlineProject>,
    warnings: Vec<&'static str>,
}

impl Outcome {
    fn with_data(data: Value) -> Self {
        Self {
            data,
            project: None,
            warnings: Vec::new(),
        }
    }
}

async fn execute(
    options: Options,
    cwd: &Path,
    connector: &dyn Connector,
    deadline: Deadline,
    stderr: &mut String,
) -> Result<Outcome, Fault> {
    let command = options.command;
    let command = match command {
        Command::ToolsCall {
            tool,
            project,
            input,
        } => {
            validate_input(input, cwd, deadline).await?;
            Command::ToolsCall {
                tool,
                project,
                input: Arguments::Empty,
            }
        }
        command => command,
    };
    let endpoint = options
        .endpoint
        .or_else(|| connector.default_endpoint())
        .ok_or(Fault::ConfigMissing)?;
    let doctor = matches!(command, Command::Doctor { .. });
    let host = deadline
        .host(async { connector.connect(&endpoint).await })
        .await?;
    if doctor {
        stderr.push_str("connection: ok\n");
    }
    let capabilities = deadline.host(async { host.capabilities().await }).await?;
    if capabilities.cli_contract_version != CLI_CONTRACT_VERSION
        || !capabilities.project_targeting
        || !capabilities.public_results
    {
        return Err(Fault::IncompatibleServer);
    }
    if doctor {
        stderr.push_str("contract: ok\n");
    }

    match command {
        Command::Projects => {
            let projects = online(host.as_ref(), deadline).await?;
            let mut outcome =
                Outcome::with_data(json!({ "projects": projects, "count": projects.len() }));
            if projects.is_empty() {
                outcome.warnings.push("No editors are connected.");
            }
            Ok(outcome)
        }
        Command::Doctor { project } => {
            doctor_report(host.as_ref(), project, cwd, deadline, stderr).await
        }
        Command::ToolsList { search } => {
            let tools = catalog(host.as_ref(), deadline).await?;
            let search = search.unwrap_or_default().to_lowercase();
            let returned: Vec<_> = tools
                .iter()
                .copied()
                .filter(|tool| {
                    tool.name().contains(&search)
                        || tool.engine().name().contains(&search)
                        || tool.description().to_lowercase().contains(&search)
                })
                .map(tool_brief)
                .collect();
            Ok(Outcome::with_data(
                json!({"tools": returned, "returnedCount": returned.len(), "exposedByHost": tools.len(), "outOfScope": 0, "writeToolCount": 0}),
            ))
        }
        Command::ToolsShow(tool) => {
            require_tool(host.as_ref(), deadline, tool).await?;
            Ok(Outcome::with_data(json!({
                "name": tool.name(), "description": tool.description(), "namespace": tool.engine().name(),
                "risk": "safe", "projectScoped": true, "requiresAllowWrite": false,
                "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
                "projectRequirement": "A single online project is required."
            })))
        }
        Command::ToolsCall { tool, project, .. } => {
            require_tool(host.as_ref(), deadline, tool).await?;
            let projects = online(host.as_ref(), deadline).await?;
            let hint = local_hint(project, cwd, deadline).await?;
            let selected = project::select(&projects, Some(tool.engine()), hint)?;
            let reply = deadline.read_tool(host.as_ref(), tool, selected).await?;
            let mut outcome = Outcome::with_data(
                json!({ "structured": reply.structured, "content": reply.content }),
            );
            outcome.project = Some(selected.clone());
            Ok(outcome)
        }
        Command::Help | Command::Version => {
            unreachable!("offline commands were handled before host dispatch")
        }
    }
}

async fn doctor_report(
    host: &dyn HostSession,
    explicit: Option<PathBuf>,
    cwd: &Path,
    deadline: Deadline,
    stderr: &mut String,
) -> Result<Outcome, Fault> {
    let tools = catalog(host, deadline).await?;
    if tools.is_empty() {
        return Err(Fault::ToolUnavailable);
    }
    stderr.push_str("tools: ok\n");
    let projects = online(host, deadline).await?;
    if projects.is_empty() {
        return Err(Fault::ProjectNotConnected);
    }
    stderr.push_str("projects: ok\n");
    let has_explicit = explicit.is_some();
    let hint = local_hint(explicit, cwd, deadline).await?;
    let selection = project::select(&projects, None, hint);
    if has_explicit {
        selection?;
    }
    let target = selection
        .ok()
        .map(|p| json!({"name": p.name, "path": p.path}));
    let mut outcome = Outcome::with_data(json!({
        "checks": [
            {"name": "connection", "status": "ok"}, {"name": "contract", "status": "ok"},
            {"name": "tools", "status": "ok"}, {"name": "projects", "status": "ok"}
        ],
        "projects": projects.iter().map(|p| json!({"name": p.name, "path": p.path})).collect::<Vec<_>>(),
        "target": target
    }));
    if target.is_none() {
        outcome
            .warnings
            .push("Specify an online project with --project.");
    }
    Ok(outcome)
}

async fn catalog(host: &dyn HostSession, deadline: Deadline) -> Result<Vec<ReadTool>, Fault> {
    let mut seen = HashSet::new();
    let mut tools = deadline.host(async { host.tools().await }).await?;
    tools.retain(|tool| seen.insert(*tool));
    Ok(tools)
}

fn tool_brief(tool: ReadTool) -> Value {
    json!({ "name": tool.name(), "brief": tool.description(), "namespace": tool.engine().name(),
        "risk": "safe", "projectScoped": true, "mutatesProject": false })
}

async fn require_tool(
    host: &dyn HostSession,
    deadline: Deadline,
    tool: ReadTool,
) -> Result<(), Fault> {
    if catalog(host, deadline).await?.contains(&tool) {
        Ok(())
    } else {
        Err(Fault::ToolUnavailable)
    }
}

async fn online(host: &dyn HostSession, deadline: Deadline) -> Result<Vec<OnlineProject>, Fault> {
    let projects = deadline.host(async { host.projects().await }).await?;
    project::validate(&projects)?;
    Ok(projects)
}

async fn local_hint(
    explicit: Option<PathBuf>,
    cwd: &Path,
    deadline: Deadline,
) -> Result<Option<(Engine, PathBuf)>, Fault> {
    if explicit.is_none() && !cwd.is_absolute() {
        return Ok(None);
    }
    let cwd = cwd.to_owned();
    deadline
        .local(move || project::local_hint(explicit, cwd))
        .await
}

async fn validate_input(input: Arguments, cwd: &Path, deadline: Deadline) -> Result<(), Fault> {
    match input {
        Arguments::Empty => Ok(()),
        Arguments::Inline(text) => empty_object(text.as_bytes()),
        Arguments::File(path) if path == Path::new("-") => {
            deadline.local(|| read_args(io::stdin().lock())).await
        }
        Arguments::File(path) => {
            let path = cwd.join(path);
            deadline
                .local(move || {
                    if !fs::metadata(&path)
                        .map_err(|_| Fault::InvalidArgument)?
                        .is_file()
                    {
                        return Err(Fault::InvalidArgument);
                    }
                    read_args(File::open(path).map_err(|_| Fault::InvalidArgument)?)
                })
                .await
        }
    }
}

fn read_args(read: impl Read) -> Result<(), Fault> {
    let mut buffer = Vec::new();
    read.take(MAX_ARGS_BYTES as u64 + 1)
        .read_to_end(&mut buffer)
        .map_err(|_| Fault::InvalidArgument)?;
    empty_object(&buffer)
}

fn empty_object(bytes: &[u8]) -> Result<(), Fault> {
    if bytes.len() > MAX_ARGS_BYTES {
        return Err(Fault::InvalidArgument);
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(map)) if map.is_empty() => Ok(()),
        _ => Err(Fault::InvalidArgument),
    }
}

#[cfg(windows)]
pub mod windows;
