//! In-process client for the Coplay Unity editor's TCP bridge.
//!
//! An explicit loopback endpoint and expected project root are required. The editor
//! bridge accepts one client: connecting can disconnect a previous client. The
//! protocol does not authenticate its peer, and its project path is not a secret.
//!
//! A historical snapshot must not masquerade as a live identity:
//!
//! ```compile_fail
//! use uebox_engine_unity::UnityClient;
//! fn incorrectly_assume_online(client: &UnityClient) {
//!     let _ = client.identity();
//! }
//! ```

mod rename;
pub use rename::{GameObjectNameOutcome, PreparedGameObjectName};

use serde_json::{json, Value};
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const WELCOME: &[u8] = b"WELCOME UNITY-MCP 1 FRAMING=1\n";
const MAX_WELCOME_BYTES: usize = 128;
const EDITOR_MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum BridgeError {
    InvalidTarget(String),
    InvalidOptions(String),
    AuthorizationRequired,
    Timeout(&'static str),
    Io(&'static str, io::Error),
    InvalidHandshake,
    FrameSize { size: u64, max: usize },
    InvalidResponse(String),
    Remote(String),
    ProjectMismatch { expected: String, actual: String },
    Disconnected,
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget(reason) => write!(f, "invalid Unity target: {reason}"),
            Self::InvalidOptions(reason) => write!(f, "invalid Unity bridge options: {reason}"),
            Self::AuthorizationRequired => write!(
                f,
                "explicit takeover authorization is required before connecting to Unity"
            ),
            Self::Timeout(stage) => write!(f, "Unity bridge timed out during {stage}"),
            Self::Io(stage, error) => write!(f, "Unity bridge {stage}: {error}"),
            Self::InvalidHandshake => write!(f, "invalid Unity bridge handshake"),
            Self::FrameSize { size, max } => {
                write!(f, "invalid Unity frame size {size} (maximum {max})")
            }
            Self::InvalidResponse(reason) => write!(f, "invalid Unity response: {reason}"),
            Self::Remote(reason) => write!(f, "Unity editor returned an error: {reason}"),
            Self::ProjectMismatch { expected, actual } => write!(
                f,
                "Unity project mismatch: expected {expected}, received {actual}"
            ),
            Self::Disconnected => write!(f, "Unity connection is closed"),
        }
    }
}

impl std::error::Error for BridgeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(_, error) => Some(error),
            _ => None,
        }
    }
}

/// A specific editor endpoint and the project that must answer on it.
#[derive(Debug, Clone)]
pub struct Target {
    pub address: SocketAddr,
    pub expected_project_root: String,
}

/// A one-use authorization for the precise target contained within it.
/// Construct this only after the user explicitly approves replacing a possible
/// existing client. The protocol cannot check for that client without connecting.
///
/// ```compile_fail
/// use uebox_engine_unity::{AuthorizedTarget, BridgeOptions, UnityClient};
/// fn cannot_reuse_approval(approval: AuthorizedTarget) {
///     let _ = UnityClient::connect_authorized(approval, BridgeOptions::default());
///     let _ = UnityClient::connect_authorized(approval, BridgeOptions::default());
/// }
/// ```
///
/// ```compile_fail
/// use std::net::SocketAddr;
/// use uebox_engine_unity::AuthorizedTarget;
/// fn cannot_retarget(mut approval: AuthorizedTarget, address: SocketAddr) {
///     approval.0.address = address;
/// }
/// ```
#[derive(Debug)]
pub struct AuthorizedTarget(Target);

impl Target {
    pub fn new(
        address: SocketAddr,
        expected_project_root: impl Into<String>,
    ) -> Result<Self, BridgeError> {
        let expected_project_root = expected_project_root.into();
        if !address.ip().is_loopback() || address.port() == 0 {
            return Err(BridgeError::InvalidTarget(
                "a loopback address with a nonzero port is required".into(),
            ));
        }
        if !valid_project_root(&expected_project_root) {
            return Err(BridgeError::InvalidTarget(
                "project root must be absolute and contain no parent traversal".into(),
            ));
        }
        Ok(Self {
            address,
            expected_project_root,
        })
    }

    /// Record the caller's explicit approval for this exact target. Creating the
    /// value does not itself obtain consent from the user or contact the editor.
    pub fn authorize_takeover(self) -> AuthorizedTarget {
        AuthorizedTarget(self)
    }
}

/// Limits apply to each handshake or request, including all partial reads/writes.
#[derive(Debug, Clone, Copy)]
pub struct BridgeOptions {
    pub io_timeout: Duration,
    pub max_frame_bytes: usize,
}

impl Default for BridgeOptions {
    fn default() -> Self {
        Self {
            io_timeout: Duration::from_secs(5),
            max_frame_bytes: 1024 * 1024,
        }
    }
}

impl BridgeOptions {
    fn validate(self) -> Result<(), BridgeError> {
        if self.io_timeout.is_zero() || self.io_timeout > MAX_TIMEOUT {
            return Err(BridgeError::InvalidOptions(
                "timeout must be greater than zero and at most 30 seconds".into(),
            ));
        }
        if self.max_frame_bytes == 0 || self.max_frame_bytes > EDITOR_MAX_FRAME_BYTES {
            return Err(BridgeError::InvalidOptions(
                "frame limit must be between 1 and 64 MiB".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInfo {
    pub project_root: String,
    pub project_name: String,
    pub unity_version: String,
    pub assets_path: String,
    pub platform: String,
    pub render_pipeline: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveScene {
    pub name: String,
    pub path: String,
    pub build_index: i64,
    pub is_dirty: bool,
    pub is_loaded: bool,
    pub root_count: u64,
}

/// One project-checked connection. Coplay cannot prove a process identity, even
/// across two connections to the same endpoint and project.
static CONNECTION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub struct UnityClient {
    connection_id: u64,
    target: Target,
    canonical_project_root: PathBuf,
    options: BridgeOptions,
    identity: ProjectInfo,
    stream: Option<TcpStream>,
}

impl UnityClient {
    /// Legacy entry point: an address and project do not authorize evicting a
    /// current client, so this always returns without opening a socket.
    pub fn connect(_target: Target, _options: BridgeOptions) -> Result<Self, BridgeError> {
        Err(BridgeError::AuthorizationRequired)
    }

    /// Consumes explicit takeover authorization, connects to exactly that
    /// target and checks the welcome and project. It never reconnects.
    pub fn connect_authorized(
        authorized: AuthorizedTarget,
        options: BridgeOptions,
    ) -> Result<Self, BridgeError> {
        let AuthorizedTarget(target) = authorized;
        // Revalidate even if a caller constructed the public Target fields directly.
        let target = Target::new(target.address, target.expected_project_root)?;
        options.validate()?;
        #[cfg(windows)]
        if windows_local_path(&target.expected_project_root).is_none() {
            return Err(BridgeError::InvalidTarget(
                "Windows project root must be a local drive path".into(),
            ));
        }
        let project_root = std::fs::canonicalize(&target.expected_project_root).map_err(|_| {
            BridgeError::InvalidTarget("project root is not an accessible directory".into())
        })?;
        #[cfg(windows)]
        if project_root.to_str().and_then(windows_local_path).is_none() {
            return Err(BridgeError::InvalidTarget(
                "Windows project root must resolve to a local drive path".into(),
            ));
        }
        if !project_root.is_dir() {
            return Err(BridgeError::InvalidTarget(
                "project root is not an accessible directory".into(),
            ));
        }
        let mut stream = TcpStream::connect_timeout(&target.address, options.io_timeout)
            .map_err(|error| io_error("connect", error))?;
        read_welcome(&mut stream, options.io_timeout)?;
        let identity = query_project(&mut stream, &target, &project_root, options)?;
        Ok(Self {
            connection_id: CONNECTION_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            target,
            canonical_project_root: project_root,
            options,
            identity,
            stream: Some(stream),
        })
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    /// Historical data only: this snapshot does not imply a live connection.
    pub fn last_observed_project_info(&self) -> &ProjectInfo {
        &self.identity
    }

    /// Checks the existing connection with Coplay's framed ping. This neither
    /// reconnects nor proves which editor process is at the other end.
    pub fn ping(&mut self) -> Result<(), BridgeError> {
        let mut stream = self.stream.take().ok_or(BridgeError::Disconnected)?;
        let result = (|| {
            let response = exchange_json(&mut stream, b"ping", self.options)?;
            match response.get("status").and_then(Value::as_str) {
                Some("error") => Err(BridgeError::Remote(
                    response
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown editor error")
                        .to_owned(),
                )),
                Some("success")
                    if response
                        .get("result")
                        .and_then(|value| value.get("message"))
                        .and_then(Value::as_str)
                        == Some("pong") =>
                {
                    Ok(())
                }
                _ => Err(BridgeError::InvalidResponse("invalid ping response".into())),
            }
        })();
        if result.is_ok() {
            self.stream = Some(stream);
        }
        result
    }

    /// Re-reads project information and checks it still matches the selected target.
    pub fn project_info(&mut self) -> Result<ProjectInfo, BridgeError> {
        let mut stream = self.stream.take().ok_or(BridgeError::Disconnected)?;
        let result = query_project(
            &mut stream,
            &self.target,
            &self.canonical_project_root,
            self.options,
        );
        if let Ok(identity) = &result {
            self.identity = identity.clone();
            self.stream = Some(stream);
        }
        result
    }

    /// Reads the active scene after checking project identity on the same connection.
    /// A failed request closes the stream, so a delayed reply cannot be mistaken for
    /// a later response. Reconnection must be explicit.
    pub fn active_scene(&mut self) -> Result<ActiveScene, BridgeError> {
        let mut stream = self.stream.take().ok_or(BridgeError::Disconnected)?;
        let result = (|| {
            let identity = query_project(
                &mut stream,
                &self.target,
                &self.canonical_project_root,
                self.options,
            )?;
            let data = query_data(
                &mut stream,
                json!({"type":"manage_scene","params":{"action":"get_active"}}),
                self.options,
            )?;
            let scene = parse_scene(&data)?;
            Ok((identity, scene))
        })();
        match result {
            Ok((identity, scene)) => {
                self.identity = identity;
                self.stream = Some(stream);
                Ok(scene)
            }
            Err(error) => Err(error),
        }
    }
}

fn valid_project_root(root: &str) -> bool {
    let path = Path::new(root);
    !root.is_empty()
        && path.is_absolute()
        && !path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
}

fn same_project(expected: &str, canonical_expected: &Path, actual: &str) -> bool {
    if !valid_project_root(actual) {
        return false;
    }
    #[cfg(windows)]
    {
        // This closure returns only the result obtained from the selected path
        // before connecting. Never resolve a path supplied by the peer.
        windows_project_matches(expected, actual, |_| Ok(canonical_expected.to_owned()))
    }
    #[cfg(not(windows))]
    {
        let actual = Path::new(actual);
        Path::new(expected) == actual || canonical_expected == actual
    }
}

#[cfg(any(windows, test))]
struct WindowsLocalPath {
    path: String,
    verbatim: bool,
}

// Only drive-rooted paths are representable here. In particular, do not let a
// peer's UNC, device, or other namespace path reach any filesystem resolver.
#[cfg(any(windows, test))]
fn windows_local_path(root: &str) -> Option<WindowsLocalPath> {
    let (path, verbatim) = match root.strip_prefix(r"\\?\") {
        Some(path) => (path, true),
        None => (root, false),
    };
    let bytes = path.as_bytes();
    if bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || (bytes[2] != b'\\' && (verbatim || bytes[2] != b'/'))
        || path.contains('\0')
        || (verbatim && path.contains('/'))
    {
        return None;
    }
    let path = if verbatim {
        path.to_owned()
    } else {
        path.replace('/', "\\")
    };
    if path[3..].split('\\').any(|part| part == "..") {
        return None;
    }
    Some(WindowsLocalPath { path, verbatim })
}

#[cfg(any(windows, test))]
fn windows_paths_equal(left: &WindowsLocalPath, right: &WindowsLocalPath) -> bool {
    if left.path != right.path {
        return false;
    }
    if left.verbatim == right.verbatim {
        return true;
    }
    // Win32 and verbatim namespaces interpret some names differently. Only
    // equate the two representations for a deliberately conservative subset.
    let rest = &left.path[3..];
    rest.is_empty()
        || rest.split('\\').all(|part| {
            if part.is_empty()
                || part.ends_with(['.', ' '])
                || !part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
            {
                return false;
            }
            let name = part.split('.').next().unwrap_or("").trim_end_matches(' ');
            let upper = name.to_ascii_uppercase();
            if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
                return false;
            }
            let bytes = upper.as_bytes();
            !(bytes.len() == 4
                && (bytes.starts_with(b"COM") || bytes.starts_with(b"LPT"))
                && (b'1'..=b'9').contains(&bytes[3]))
        })
}

#[cfg(any(windows, test))]
fn windows_project_matches<R>(expected: &str, actual: &str, resolve: R) -> bool
where
    R: Fn(&str) -> io::Result<std::path::PathBuf>,
{
    let (Some(selected), Some(actual)) = (windows_local_path(expected), windows_local_path(actual))
    else {
        return false;
    };
    let Ok(canonical) = resolve(expected) else {
        return false;
    };
    let Some(canonical) = canonical.to_str().and_then(windows_local_path) else {
        return false;
    };
    windows_paths_equal(&actual, &selected) || windows_paths_equal(&actual, &canonical)
}

fn query_project(
    stream: &mut TcpStream,
    target: &Target,
    canonical_project_root: &Path,
    options: BridgeOptions,
) -> Result<ProjectInfo, BridgeError> {
    let data = query_data(
        stream,
        json!({"type":"get_project_info","params":{}}),
        options,
    )?;
    let info = ProjectInfo {
        project_root: required_string(&data, "projectRoot")?,
        project_name: required_string(&data, "projectName")?,
        unity_version: required_string(&data, "unityVersion")?,
        assets_path: required_string(&data, "assetsPath")?,
        platform: required_string(&data, "platform")?,
        render_pipeline: required_string(&data, "renderPipeline")?,
    };
    if !same_project(
        &target.expected_project_root,
        canonical_project_root,
        &info.project_root,
    ) {
        return Err(BridgeError::ProjectMismatch {
            expected: target.expected_project_root.clone(),
            actual: info.project_root,
        });
    }
    Ok(info)
}

fn parse_scene(data: &Value) -> Result<ActiveScene, BridgeError> {
    Ok(ActiveScene {
        name: required_string(data, "name")?,
        path: required_string(data, "path")?,
        build_index: data["buildIndex"]
            .as_i64()
            .ok_or_else(|| invalid_field("buildIndex"))?,
        is_dirty: data["isDirty"]
            .as_bool()
            .ok_or_else(|| invalid_field("isDirty"))?,
        is_loaded: data["isLoaded"]
            .as_bool()
            .ok_or_else(|| invalid_field("isLoaded"))?,
        root_count: data["rootCount"]
            .as_u64()
            .ok_or_else(|| invalid_field("rootCount"))?,
    })
}

fn required_string(data: &Value, field: &str) -> Result<String, BridgeError> {
    data.get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid_field(field))
}

fn invalid_field(field: &str) -> BridgeError {
    BridgeError::InvalidResponse(format!("missing or invalid {field}"))
}

fn query_data(
    stream: &mut TcpStream,
    command: Value,
    options: BridgeOptions,
) -> Result<Value, BridgeError> {
    let payload = serde_json::to_vec(&command)
        .map_err(|error| BridgeError::InvalidResponse(error.to_string()))?;
    let response = exchange_json(stream, &payload, options)?;
    match response.get("status").and_then(Value::as_str) {
        Some("error") => {
            return Err(BridgeError::Remote(
                response
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown editor error")
                    .to_owned(),
            ))
        }
        Some("success") => {}
        _ => return Err(invalid_field("status")),
    }
    let result = response
        .get("result")
        .ok_or_else(|| invalid_field("result"))?;
    match result.get("success").and_then(Value::as_bool) {
        Some(false) => Err(BridgeError::Remote(
            result
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown editor error")
                .to_owned(),
        )),
        Some(true) => result
            .get("data")
            .filter(|data| data.is_object())
            .cloned()
            .ok_or_else(|| invalid_field("result.data")),
        None => Err(invalid_field("result.success")),
    }
}

fn exchange_json(
    stream: &mut TcpStream,
    payload: &[u8],
    options: BridgeOptions,
) -> Result<Value, BridgeError> {
    let deadline = Instant::now() + options.io_timeout;
    write_frame(stream, payload, options.max_frame_bytes, deadline)?;
    let reply = read_frame(stream, options.max_frame_bytes, deadline)?;
    serde_json::from_slice(&reply).map_err(|error| BridgeError::InvalidResponse(error.to_string()))
}

fn read_welcome(stream: &mut TcpStream, timeout: Duration) -> Result<(), BridgeError> {
    let deadline = Instant::now() + timeout;
    let mut welcome = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        read_exact_until(stream, &mut byte, deadline, "handshake")?;
        welcome.push(byte[0]);
        if byte[0] == b'\n' {
            return if welcome == WELCOME {
                Ok(())
            } else {
                Err(BridgeError::InvalidHandshake)
            };
        }
        if welcome.len() >= MAX_WELCOME_BYTES {
            return Err(BridgeError::InvalidHandshake);
        }
    }
}

fn write_frame(
    stream: &mut TcpStream,
    payload: &[u8],
    max: usize,
    deadline: Instant,
) -> Result<(), BridgeError> {
    if payload.is_empty() || payload.len() > max {
        return Err(BridgeError::FrameSize {
            size: payload.len() as u64,
            max,
        });
    }
    write_all_until(stream, &(payload.len() as u64).to_be_bytes(), deadline)?;
    write_all_until(stream, payload, deadline)
}

fn read_frame(
    stream: &mut TcpStream,
    max: usize,
    deadline: Instant,
) -> Result<Vec<u8>, BridgeError> {
    let mut header = [0_u8; 8];
    read_exact_until(stream, &mut header, deadline, "response")?;
    let size = u64::from_be_bytes(header);
    if size == 0 || size > max as u64 {
        return Err(BridgeError::FrameSize { size, max });
    }
    let mut payload = vec![0; size as usize];
    read_exact_until(stream, &mut payload, deadline, "response")?;
    Ok(payload)
}

fn remaining(deadline: Instant, stage: &'static str) -> Result<Duration, BridgeError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or(BridgeError::Timeout(stage))
}

fn read_exact_until(
    stream: &mut TcpStream,
    mut buffer: &mut [u8],
    deadline: Instant,
    stage: &'static str,
) -> Result<(), BridgeError> {
    while !buffer.is_empty() {
        stream
            .set_read_timeout(Some(remaining(deadline, stage)?))
            .map_err(|error| io_error(stage, error))?;
        match stream.read(buffer) {
            Ok(0) => return Err(BridgeError::Disconnected),
            Ok(count) => buffer = &mut buffer[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_error(stage, error)),
        }
    }
    Ok(())
}

fn write_all_until(
    stream: &mut TcpStream,
    mut buffer: &[u8],
    deadline: Instant,
) -> Result<(), BridgeError> {
    while !buffer.is_empty() {
        stream
            .set_write_timeout(Some(remaining(deadline, "request")?))
            .map_err(|error| io_error("request", error))?;
        match stream.write(buffer) {
            Ok(0) => return Err(BridgeError::Disconnected),
            Ok(count) => buffer = &buffer[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_error("request", error)),
        }
    }
    Ok(())
}

fn io_error(stage: &'static str, error: io::Error) -> BridgeError {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => BridgeError::Timeout(stage),
        io::ErrorKind::UnexpectedEof
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::NotConnected
        | io::ErrorKind::WriteZero => BridgeError::Disconnected,
        _ => BridgeError::Io(stage, error),
    }
}

#[cfg(test)]
mod windows_identity_tests {
    use super::*;
    use std::cell::Cell;
    use std::path::PathBuf;

    fn resolve(path: &str) -> io::Result<PathBuf> {
        match path {
            r"C:\work\Foo" => Ok(PathBuf::from(r"\\?\C:\work\Foo")),
            r"C:\work\foo" => Ok(PathBuf::from(r"\\?\C:\work\foo")),
            r"\\?\C:\work\Project" | "C:/work/Project" => Ok(PathBuf::from(r"\\?\C:\work\Project")),
            _ => Err(io::Error::new(io::ErrorKind::NotFound, "unresolved")),
        }
    }

    #[test]
    fn distinct_case_sensitive_windows_directories_are_not_the_same_project() {
        assert!(!windows_project_matches(
            r"C:\work\Foo",
            r"C:\work\foo",
            resolve
        ));
    }

    #[test]
    fn extended_and_ordinary_windows_paths_can_resolve_to_the_same_directory() {
        assert!(windows_project_matches(
            r"\\?\C:\work\Project",
            "C:/work/Project",
            resolve
        ));
        assert!(windows_project_matches(
            r"C:\links\Chosen",
            r"C:\work\Project",
            |selected| {
                assert_eq!(selected, r"C:\links\Chosen");
                Ok(PathBuf::from(r"\\?\C:\work\Project"))
            }
        ));
    }

    #[test]
    fn unresolved_windows_directory_is_rejected_even_when_paths_are_identical() {
        assert!(!windows_project_matches(
            r"C:\work\missing",
            r"C:\work\missing",
            resolve
        ));
    }

    #[test]
    fn untrusted_windows_network_and_device_paths_never_reach_the_resolver() {
        for actual in [
            r"\\server\share\project",
            r"\\server/share\project",
            "//server/share/project",
            r"\\?\UNC\server\share\project",
            r"\\?\unc\server\share\project",
            r"\\.\UNC\server\share\project",
            r"\\?\GLOBALROOT\Device\Mup\server\share\project",
            r"\\?\C:relative",
            r"C:relative",
        ] {
            let resolver_calls = Cell::new(0);
            let matches = windows_project_matches(r"C:\work\Project", actual, |_| {
                resolver_calls.set(resolver_calls.get() + 1);
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "trap"))
            });
            assert!(!matches, "unexpected match for {actual:?}");
            assert_eq!(
                resolver_calls.get(),
                0,
                "resolved untrusted path {actual:?}"
            );
        }
    }

    #[test]
    fn untrusted_local_windows_path_is_never_resolved() {
        let actual = r"C:\work\Other";
        let matches = windows_project_matches(r"C:\work\Project", actual, |path| {
            assert_ne!(path, actual, "resolved untrusted editor path");
            Ok(PathBuf::from(r"\\?\C:\work\Project"))
        });
        assert!(!matches);
    }

    #[test]
    fn selected_unc_is_rejected_before_resolution() {
        let path = r"\\server\share\project";
        let resolver_calls = Cell::new(0);
        let matches = windows_project_matches(path, path, |_| {
            resolver_calls.set(resolver_calls.get() + 1);
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "trap"))
        });
        assert!(!matches);
        assert_eq!(resolver_calls.get(), 0);
    }

    #[test]
    fn selected_drive_resolving_to_unc_is_not_a_local_identity() {
        let matches = windows_project_matches(r"C:\work\Project", r"C:\work\Project", |_| {
            Ok(PathBuf::from(r"\\?\UNC\server\share\project"))
        });
        assert!(!matches);
    }

    #[test]
    fn win32_and_verbatim_names_with_ambiguous_semantics_are_not_equated() {
        for name in ["Project.", "Project ", "CON", "COM1.txt"] {
            let actual = format!(r"C:\work\{name}");
            let extended = format!(r"\\?\C:\work\{name}");
            let matches =
                windows_project_matches(&extended, &actual, |_| Ok(PathBuf::from(&extended)));
            assert!(!matches, "ambiguous cross-prefix match for {name:?}");
        }
    }

    #[test]
    fn selected_directory_matches_its_saved_canonical_path_but_not_another_directory() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let first = root.join("Project A");
        let canonical = std::fs::canonicalize(&first).unwrap();
        let second = root.join("Other");
        assert!(same_project(
            first.to_str().unwrap(),
            &canonical,
            canonical.to_str().unwrap()
        ));
        assert!(same_project(
            first.to_str().unwrap(),
            &canonical,
            first.to_str().unwrap()
        ));
        assert!(!same_project(
            first.to_str().unwrap(),
            &canonical,
            second.to_str().unwrap()
        ));
    }
}
