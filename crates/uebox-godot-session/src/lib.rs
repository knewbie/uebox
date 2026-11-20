//! Explicit, synchronous management of a selected Godot project editor.
//!
//! The existing bridge is protocol v1: it sends the secret over loopback and
//! cannot authenticate the server or revoke itself. See the README before use.

use std::{
    fmt, fs,
    io::{self, Read},
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};
use uebox_engine_godot::{Credentials, Error as BridgeError, GodotClient, Target};
pub use uebox_engine_godot::{
    NodeRenameOutcome, PreparedNodeRename, ProjectInfo, SceneNode, SceneTree,
};

const MAX_QUERY_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_STARTUP_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorMode {
    Interactive,
    Headless,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExitInfo {
    pub success: bool,
    pub code: Option<i32>,
    /// Unix termination signal, when available. A nonzero exit code alone does not prove a crash.
    pub signal: Option<i32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionState {
    Starting,
    Ready,
    Disconnected,
    StartupExpired,
    Exited(ExitInfo),
}

/// Input from a trusted native host, never a WebView or model command.
pub struct LaunchOptions {
    executable: PathBuf,
    project: PathBuf,
    query_timeout: Duration,
    startup_timeout: Duration,
    mode: EditorMode,
    legacy_acknowledged: bool,
    isolation_home: Option<PathBuf>,
}

impl LaunchOptions {
    pub fn new(executable: impl AsRef<Path>, project: impl AsRef<Path>) -> Self {
        Self {
            executable: executable.as_ref().to_owned(),
            project: project.as_ref().to_owned(),
            query_timeout: Duration::from_secs(3),
            startup_timeout: Duration::from_secs(60),
            mode: EditorMode::Interactive,
            legacy_acknowledged: false,
            isolation_home: None,
        }
    }
    pub fn query_timeout(mut self, timeout: Duration) -> Self {
        self.query_timeout = timeout;
        self
    }
    pub fn startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }
    pub fn editor_mode(mut self, mode: EditorMode) -> Self {
        self.mode = mode;
        self
    }
    /// Use existing private directories for the editor's config, cache and temp files.
    pub fn isolate_editor_paths(mut self, home: impl AsRef<Path>) -> Self {
        self.isolation_home = Some(home.as_ref().to_owned());
        self
    }
    /// Explicitly accept the unauthenticated, plaintext v1 local-transport risk.
    /// This does not make the protocol suitable for public production use.
    pub fn acknowledge_legacy_protocol_risks(mut self) -> Self {
        self.legacy_acknowledged = true;
        self
    }
}

/// Only fixed categories are returned; untrusted peer messages are never echoed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionError {
    InvalidExecutable,
    InvalidProject,
    InvalidTimeout,
    InvalidIsolation,
    LegacyProtocolNotAcknowledged,
    Entropy,
    PortUnavailable(io::ErrorKind),
    Spawn(io::ErrorKind),
    ProcessStatus(io::ErrorKind),
    InvalidTarget,
    EditorExited(ExitInfo),
    StartupTimeout,
    NotReady,
    AlreadyReady,
    SessionClosed,
    Disconnected,
    Timeout,
    Protocol,
    Unauthorized,
    WrongTarget,
    ResponseTooLarge,
    /// An editor result contained this session's literal token or instance ID.
    PrivateValueInResult,
    RemoteRejected,
    Network(io::ErrorKind),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Godot session error: {self:?}")
    }
}
impl std::error::Error for SessionError {}

/// Owns one editor handle and one private v1 client. Dropping it does not kill
/// the editor and cannot revoke the v1 addon. It is not a serializable DTO.
pub struct GodotSession {
    child: Option<Child>,
    client: Option<GodotClient>,
    project_path: PathBuf,
    instance_id: String,
    state: SessionState,
    startup_deadline: Instant,
}

trait CheckedSessionResult {
    fn contains_private_values(&self, session: &GodotSession) -> bool;
}

impl CheckedSessionResult for ProjectInfo {
    fn contains_private_values(&self, session: &GodotSession) -> bool {
        let ProjectInfo {
            name,
            godot_version,
        } = self;
        session.contains_private_bridge_values(name)
            || session.contains_private_bridge_values(godot_version)
    }
}

impl CheckedSessionResult for SceneTree {
    fn contains_private_values(&self, session: &GodotSession) -> bool {
        let SceneTree {
            scene_path,
            nodes,
            truncated: _,
        } = self;
        scene_path
            .as_deref()
            .is_some_and(|path| session.contains_private_bridge_values(path))
            || nodes.iter().any(|node| {
                let SceneNode { path, name, class } = node;
                session.contains_private_bridge_values(path)
                    || session.contains_private_bridge_values(name)
                    || session.contains_private_bridge_values(class)
            })
    }
}

impl CheckedSessionResult for PreparedNodeRename {
    fn contains_private_values(&self, session: &GodotSession) -> bool {
        [
            Some(self.node_path()),
            Some(self.old_name()),
            Some(self.new_name()),
            self.scene_path(),
        ]
        .into_iter()
        .flatten()
        .any(|value| session.contains_private_bridge_values(value))
    }
}

impl GodotSession {
    /// Starts exactly the selected binary and project. Does not connect yet.
    pub fn launch(options: LaunchOptions) -> Result<Self, SessionError> {
        if options.query_timeout.is_zero()
            || options.query_timeout > MAX_QUERY_TIMEOUT
            || options.startup_timeout.is_zero()
            || options.startup_timeout > MAX_STARTUP_TIMEOUT
        {
            return Err(SessionError::InvalidTimeout);
        }
        if !options.project.is_absolute() {
            return Err(SessionError::InvalidProject);
        }
        let project =
            fs::canonicalize(&options.project).map_err(|_| SessionError::InvalidProject)?;
        if !project.is_dir() || !project.join("project.godot").is_file() {
            return Err(SessionError::InvalidProject);
        }
        let isolation_home = match options.isolation_home {
            Some(home) if home.is_absolute() => {
                let home = fs::canonicalize(home).map_err(|_| SessionError::InvalidIsolation)?;
                if !home.is_dir() {
                    return Err(SessionError::InvalidIsolation);
                }
                Some(host_path(&home))
            }
            Some(_) => return Err(SessionError::InvalidIsolation),
            None => None,
        };
        if !options.executable.is_absolute() {
            return Err(SessionError::InvalidExecutable);
        }
        let executable =
            fs::canonicalize(&options.executable).map_err(|_| SessionError::InvalidExecutable)?;
        if !is_native_executable(&executable) {
            return Err(SessionError::InvalidExecutable);
        }
        if !options.legacy_acknowledged {
            return Err(SessionError::LegacyProtocolNotAcknowledged);
        }
        let credentials = Credentials::generate().map_err(|_| SessionError::Entropy)?;
        let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .map_err(|error| SessionError::PortUnavailable(error.kind()))?;
        let address = reservation
            .local_addr()
            .map_err(|error| SessionError::PortUnavailable(error.kind()))?;
        let lsp_reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .map_err(|error| SessionError::PortUnavailable(error.kind()))?;
        let dap_reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .map_err(|error| SessionError::PortUnavailable(error.kind()))?;
        let lsp_port = lsp_reservation
            .local_addr()
            .map_err(|error| SessionError::PortUnavailable(error.kind()))?
            .port();
        let dap_port = dap_reservation
            .local_addr()
            .map_err(|error| SessionError::PortUnavailable(error.kind()))?
            .port();
        let target = Target::new(
            address,
            &project,
            credentials.instance_id.clone(),
            credentials.token.clone(),
            options.query_timeout,
        )
        .map_err(|_| SessionError::InvalidTarget)?;
        let mut command = Command::new(&executable);
        if options.mode == EditorMode::Headless {
            command.arg("--headless");
        }
        command
            .arg("--editor")
            .arg("--path")
            .arg(host_path(&project))
            .arg("--lsp-port")
            .arg(lsp_port.to_string())
            .arg("--dap-port")
            .arg(dap_port.to_string());
        command
            .current_dir(&project)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_environment(&mut command);
        if let Some(home) = isolation_home {
            command
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .env("APPDATA", home.join("appdata"))
                .env("LOCALAPPDATA", home.join("localappdata"))
                .env("XDG_CONFIG_HOME", home.join("config"))
                .env("XDG_DATA_HOME", home.join("data"))
                .env("XDG_CACHE_HOME", home.join("cache"))
                .env("TMPDIR", home.join("temp"))
                .env("TMP", home.join("temp"))
                .env("TEMP", home.join("temp"));
        }
        command
            .env("UEBOX_GODOT_PORT", address.port().to_string())
            .env("UEBOX_GODOT_INSTANCE_ID", &credentials.instance_id)
            .env("UEBOX_GODOT_TOKEN", &credentials.token);
        // Godot TCPServer cannot inherit this listener. The release/bind race
        // is a known v1 risk; the acknowledging caller must trust the host.
        drop(reservation);
        drop(lsp_reservation);
        drop(dap_reservation);
        let child = command
            .spawn()
            .map_err(|error| SessionError::Spawn(error.kind()))?;
        drop(command);
        let startup_deadline = Instant::now() + options.startup_timeout;
        Ok(Self {
            child: Some(child),
            client: Some(GodotClient::new(target)),
            project_path: project,
            instance_id: credentials.instance_id,
            state: SessionState::Starting,
            startup_deadline,
        })
    }

    pub fn project_path(&self) -> &Path {
        &self.project_path
    }
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Check this session's exact bridge token and instance ID without exposing
    /// either value. Returns true if the client is unavailable so callers fail
    /// closed; encoded or split values are outside this check's scope.
    pub fn contains_private_bridge_values(&self, text: &str) -> bool {
        match &self.client {
            Some(client) => client.contains_private_bridge_values(text),
            None => true,
        }
    }

    /// Polls only the tracked Child. It never sends a signal or reconnects.
    pub fn status(&mut self) -> Result<SessionState, SessionError> {
        if let Some(child) = self.child.as_mut() {
            if let Some(status) = child
                .try_wait()
                .map_err(|error| SessionError::ProcessStatus(error.kind()))?
            {
                self.state = SessionState::Exited(exit_info(status));
                self.client = None;
            }
        }
        if self.state == SessionState::Starting && Instant::now() >= self.startup_deadline {
            self.state = SessionState::StartupExpired;
            self.client = None;
        }
        Ok(self.state.clone())
    }

    /// Makes one explicit initial connection attempt. No polling or auto-reconnect.
    pub fn connect_once(&mut self) -> Result<ProjectInfo, SessionError> {
        match self.status()? {
            SessionState::Starting => {}
            SessionState::Ready => return Err(SessionError::AlreadyReady),
            SessionState::StartupExpired => return Err(SessionError::StartupTimeout),
            SessionState::Exited(info) => return Err(SessionError::EditorExited(info)),
            SessionState::Disconnected => return Err(SessionError::SessionClosed),
        }
        let result = self
            .client
            .as_ref()
            .expect("starting client")
            .project_info();
        self.finish_query(result, true)
    }

    pub fn project_info(&mut self) -> Result<ProjectInfo, SessionError> {
        self.ensure_ready()?;
        let result = self.client.as_ref().expect("ready client").project_info();
        self.finish_query(result, false)
    }

    pub fn scene_tree(&mut self) -> Result<SceneTree, SessionError> {
        self.ensure_ready()?;
        let result = self.client.as_ref().expect("ready client").scene_tree();
        self.finish_query(result, false)
    }

    /// Bind a read-only preview to this managed editor's exact session.
    pub fn prepare_node_rename(
        &mut self,
        path: &str,
        name: &str,
    ) -> Result<PreparedNodeRename, SessionError> {
        self.ensure_ready()?;
        let result = self
            .client
            .as_ref()
            .expect("ready client")
            .prepare_node_rename(path, name);
        self.finish_query(result, false)
    }

    /// Hand off only an already approved, corresponding editor ticket.
    pub fn apply_node_rename(&mut self, prepared: PreparedNodeRename) -> NodeRenameOutcome {
        if self.ensure_ready().is_err() {
            return NodeRenameOutcome::NotExecuted {
                reason: "bridge_unavailable",
            };
        }
        let result = self
            .client
            .as_ref()
            .expect("ready client")
            .apply_node_rename(prepared);
        if result == NodeRenameOutcome::Unknown {
            self.state = SessionState::Disconnected;
            self.client = None;
        }
        result
    }

    fn ensure_ready(&mut self) -> Result<(), SessionError> {
        match self.status()? {
            SessionState::Ready => Ok(()),
            SessionState::Starting => Err(SessionError::NotReady),
            SessionState::StartupExpired => Err(SessionError::StartupTimeout),
            SessionState::Disconnected => Err(SessionError::SessionClosed),
            SessionState::Exited(info) => Err(SessionError::EditorExited(info)),
        }
    }

    fn finish_query<T: CheckedSessionResult>(
        &mut self,
        result: Result<T, BridgeError>,
        starting: bool,
    ) -> Result<T, SessionError> {
        match self.status()? {
            SessionState::Exited(info) => return Err(SessionError::EditorExited(info)),
            SessionState::StartupExpired => return Err(SessionError::StartupTimeout),
            _ => {}
        }
        match result {
            Ok(value) => {
                if value.contains_private_values(self) {
                    return Err(SessionError::PrivateValueInResult);
                }
                if starting {
                    self.state = SessionState::Ready;
                }
                Ok(value)
            }
            Err(error) => {
                let error = map_bridge_error(error);
                let retryable_start =
                    starting && matches!(error, SessionError::Disconnected | SessionError::Timeout);
                let terminal = matches!(
                    error,
                    SessionError::Disconnected
                        | SessionError::Timeout
                        | SessionError::Protocol
                        | SessionError::Unauthorized
                        | SessionError::WrongTarget
                        | SessionError::InvalidTarget
                        | SessionError::Network(_)
                );
                if !retryable_start && terminal {
                    self.state = SessionState::Disconnected;
                    self.client = None;
                }
                Err(error)
            }
        }
    }

    /// Releases management, returning the original process handle to the caller.
    /// The v1 plugin is not revoked and may still be listening.
    pub fn into_editor_process(mut self) -> Child {
        self.client = None;
        self.child.take().expect("session retains its child")
    }
}

impl Drop for GodotSession {
    fn drop(&mut self) {
        // No kill-on-drop. Reap eventually while this Rust host remains alive.
        if let Some(mut child) = self.child.take() {
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = thread::Builder::new()
                    .name("godot-child-reaper".into())
                    .spawn(move || {
                        let _ = child.wait();
                    });
            }
        }
    }
}

fn exit_info(status: ExitStatus) -> ExitInfo {
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal = None;
    ExitInfo {
        success: status.success(),
        code: status.code(),
        signal,
    }
}

fn map_bridge_error(error: BridgeError) -> SessionError {
    match error {
        BridgeError::InvalidTarget(_) => SessionError::InvalidTarget,
        BridgeError::Timeout => SessionError::Timeout,
        BridgeError::Disconnected => SessionError::Disconnected,
        BridgeError::Protocol(_) => SessionError::Protocol,
        BridgeError::Entropy(_) => SessionError::Entropy,
        BridgeError::Io(error) => SessionError::Network(error.kind()),
        BridgeError::Remote { code, .. } => match code.as_str() {
            "unauthorized" => SessionError::Unauthorized,
            "wrong_target" => SessionError::WrongTarget,
            "response_too_large" => SessionError::ResponseTooLarge,
            _ => SessionError::RemoteRejected,
        },
    }
}

fn configure_environment(command: &mut Command) {
    command.env_clear();
    for name in [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TMPDIR",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XAUTHORITY",
        "XDG_RUNTIME_DIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "DBUS_SESSION_BUS_ADDRESS",
        "SYSTEMROOT",
        "WINDIR",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "APPDATA",
        "LOCALAPPDATA",
        "TMP",
        "TEMP",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

fn host_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(raw) = path.to_str() {
            if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
                return PathBuf::from(format!(r"\\{rest}"));
            }
            if let Some(rest) = raw.strip_prefix(r"\\?\") {
                return PathBuf::from(rest);
            }
        }
    }
    path.to_owned()
}

fn is_native_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !path.is_absolute() || !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    let mut magic = [0u8; 4];
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        matches!(
            magic,
            [0xcf, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xce]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbf, 0xba, 0xfe, 0xca]
        )
    }
    #[cfg(target_os = "linux")]
    {
        magic == [0x7f, b'E', b'L', b'F']
    }
    #[cfg(windows)]
    {
        magic[0..2] == [b'M', b'Z']
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        false
    }
}
