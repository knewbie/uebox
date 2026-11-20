//! A synchronous, in-process client for the opt-in Godot editor addon.
//! Every call targets one loopback endpoint, project, and editor instance.

mod rename;

pub use rename::{NodeRenameOutcome, PreparedNodeRename};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fmt,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const MAX_REQUEST_BYTES: usize = 8192;
const MAX_RESPONSE_BYTES: usize = 1_048_576;
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub enum Error {
    InvalidTarget(String),
    Timeout,
    Disconnected,
    Protocol(String),
    Remote { code: String, message: String },
    Entropy(String),
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget(message) => write!(f, "invalid target: {message}"),
            Self::Timeout => write!(f, "Godot editor request timed out"),
            Self::Disconnected => write!(f, "Godot editor disconnected or is unavailable"),
            Self::Protocol(message) => write!(f, "Godot protocol error: {message}"),
            Self::Remote { code, message } => {
                write!(f, "Godot rejected request ({code}): {message}")
            }
            Self::Entropy(message) => write!(f, "cannot generate editor credentials: {message}"),
            Self::Io(error) => write!(f, "Godot connection error: {error}"),
        }
    }
}
impl std::error::Error for Error {}

/// Fresh, per-editor credentials. Keep the token private; do not log or store it in a project.
pub struct Credentials {
    pub instance_id: String,
    pub token: String,
}

impl Credentials {
    pub fn generate() -> Result<Self, Error> {
        let mut bytes = [0u8; 48];
        getrandom::getrandom(&mut bytes).map_err(|error| Error::Entropy(error.to_string()))?;
        Ok(Self {
            instance_id: hex(&bytes[..16]),
            token: hex(&bytes[16..]),
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(DIGITS[(byte >> 4) as usize] as char);
        value.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    value
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// An exact target, including its canonical project directory and per-instance secret.
/// It cannot address a remote host, an unspecified port, or an unconfigured project.
pub struct Target {
    address: SocketAddr,
    project_path: String,
    instance_id: String,
    token: String,
    timeout: Duration,
}

impl Target {
    pub fn new(
        address: SocketAddr,
        project_dir: impl AsRef<Path>,
        instance_id: impl Into<String>,
        token: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, Error> {
        if address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) || address.port() == 0 {
            return Err(Error::InvalidTarget(
                "address must be 127.0.0.1 with a nonzero port".into(),
            ));
        }
        if timeout.is_zero() || Instant::now().checked_add(timeout).is_none() {
            return Err(Error::InvalidTarget(
                "timeout must be positive and representable".into(),
            ));
        }
        let instance_id = instance_id.into();
        let token = token.into();
        if !valid_hex(&instance_id, 32) || !valid_hex(&token, 64) {
            return Err(Error::InvalidTarget(
                "instance ID or token has invalid format".into(),
            ));
        }
        let project_dir = std::fs::canonicalize(project_dir)
            .map_err(|_| Error::InvalidTarget("project directory does not exist".into()))?;
        if !project_dir.is_dir() || !project_dir.join("project.godot").is_file() {
            return Err(Error::InvalidTarget(
                "directory does not contain project.godot".into(),
            ));
        }
        let path = project_dir
            .to_str()
            .ok_or_else(|| Error::InvalidTarget("project path is not UTF-8".into()))?;
        // Godot normalizes backslashes even on POSIX, where they may be a literal
        // filename character. Such a path cannot be represented unambiguously.
        #[cfg(not(windows))]
        if path.contains('\\') {
            return Err(Error::InvalidTarget(
                "project path contains a literal backslash".into(),
            ));
        }
        #[cfg(unix)]
        ensure_no_lossy_alias(&project_dir)?;
        #[cfg(not(windows))]
        let mut project_path = path.to_owned();
        #[cfg(windows)]
        let mut project_path = path.replace('\\', "/");
        #[cfg(windows)]
        {
            if let Some(path) = project_path.strip_prefix("//?/UNC/") {
                project_path = format!("//{path}");
            } else if let Some(path) = project_path.strip_prefix("//?/") {
                project_path = path.to_owned();
            }
        }
        while project_path.len() > 1 && project_path.ends_with('/') {
            project_path.pop();
        }
        if project_path.len() > 4096 {
            return Err(Error::InvalidTarget("project path is too long".into()));
        }
        Ok(Self {
            address,
            project_path,
            instance_id,
            token,
            timeout,
        })
    }
}

pub struct GodotClient {
    target: Target,
}

#[derive(Debug, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    pub godot_version: String,
}
#[derive(Debug, Deserialize)]
pub struct SceneNode {
    pub path: String,
    pub name: String,
    pub class: String,
}
#[derive(Debug, Deserialize)]
pub struct SceneTree {
    #[serde(deserialize_with = "required_optional_string")]
    pub scene_path: Option<String>,
    pub nodes: Vec<SceneNode>,
    pub truncated: bool,
}

fn required_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)
}

#[derive(Serialize)]
struct Request<'a> {
    version: u8,
    request_id: String,
    token: &'a str,
    target: Identity<'a>,
    command: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}
#[derive(Serialize)]
struct Identity<'a> {
    project_path: &'a str,
    instance_id: &'a str,
}
#[derive(Deserialize)]
struct ResponseIdentity {
    project_path: String,
    instance_id: String,
}
#[derive(Deserialize)]
struct RemoteError {
    code: String,
    message: String,
}
#[derive(Deserialize)]
struct Response {
    version: u8,
    request_id: String,
    identity: Option<ResponseIdentity>,
    result: Option<serde_json::Value>,
    error: Option<RemoteError>,
}

impl GodotClient {
    pub fn new(target: Target) -> Self {
        Self { target }
    }
    /// Check exact occurrences of this connection's token or instance ID.
    /// Intended for trusted Rust callers before exposing editor-authored text;
    /// this does not detect encoded or split values and is not authentication.
    pub fn contains_private_bridge_values(&self, text: &str) -> bool {
        text.contains(self.target.token.as_str()) || text.contains(self.target.instance_id.as_str())
    }
    pub fn project_info(&self) -> Result<ProjectInfo, Error> {
        self.query("project.info")
    }
    pub fn scene_tree(&self) -> Result<SceneTree, Error> {
        self.query("scene.tree")
    }

    fn query<T: DeserializeOwned>(&self, command: &'static str) -> Result<T, Error> {
        self.query_with(command, None, &mut false)
    }

    fn query_with<T: DeserializeOwned>(
        &self,
        command: &'static str,
        params: Option<serde_json::Value>,
        dispatched: &mut bool,
    ) -> Result<T, Error> {
        #[cfg(unix)]
        ensure_no_lossy_alias(Path::new(&self.target.project_path))?;
        let deadline = Instant::now()
            .checked_add(self.target.timeout)
            .ok_or_else(|| Error::InvalidTarget("timeout is not representable".into()))?;
        let request_id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed).to_string();
        let request = Request {
            version: 1,
            request_id: request_id.clone(),
            token: &self.target.token,
            target: Identity {
                project_path: &self.target.project_path,
                instance_id: &self.target.instance_id,
            },
            command,
            params,
        };
        let mut bytes = serde_json::to_vec(&request)
            .map_err(|_| Error::Protocol("cannot encode request".into()))?;
        bytes.push(b'\n');
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(Error::InvalidTarget("request exceeds size limit".into()));
        }
        let mut stream = TcpStream::connect_timeout(&self.target.address, remaining(deadline)?)
            .map_err(map_io)?;
        stream
            .set_write_timeout(Some(remaining(deadline)?))
            .map_err(|error| map_socket_io(&stream, error))?;
        *dispatched = true;
        stream
            .write_all(&bytes)
            .map_err(|error| map_socket_io(&stream, error))?;
        let mut response_bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            stream
                .set_read_timeout(Some(remaining(deadline)?))
                .map_err(|error| map_socket_io(&stream, error))?;
            let count = stream
                .read(&mut chunk)
                .map_err(|error| map_socket_io(&stream, error))?;
            if count == 0 {
                return Err(Error::Disconnected);
            }
            if let Some(end) = chunk[..count].iter().position(|byte| *byte == b'\n') {
                if response_bytes.len() + end > MAX_RESPONSE_BYTES {
                    return Err(Error::Protocol("response exceeds size limit".into()));
                }
                response_bytes.extend_from_slice(&chunk[..end]);
                break;
            }
            response_bytes.extend_from_slice(&chunk[..count]);
            if response_bytes.len() > MAX_RESPONSE_BYTES {
                return Err(Error::Protocol("response exceeds size limit".into()));
            }
        }
        let response: Response = serde_json::from_slice(&response_bytes)
            .map_err(|_| Error::Protocol("invalid response envelope".into()))?;
        if response.version != 1 || response.request_id != request_id {
            return Err(Error::Protocol(
                "response version or request ID mismatch".into(),
            ));
        }
        match (response.result, response.error) {
            (None, Some(error)) => Err(Error::Remote {
                code: error.code,
                message: error.message,
            }),
            (Some(result), None) => {
                let identity = response
                    .identity
                    .ok_or_else(|| Error::Protocol("response identity missing".into()))?;
                if identity.instance_id != self.target.instance_id
                    || identity.project_path != self.target.project_path
                {
                    return Err(Error::Protocol("response identity mismatch".into()));
                }
                #[cfg(unix)]
                ensure_no_lossy_alias(Path::new(&self.target.project_path))?;
                serde_json::from_value(result)
                    .map_err(|_| Error::Protocol("invalid result payload".into()))
            }
            _ => Err(Error::Protocol(
                "response must contain either result or error".into(),
            )),
        }
    }
}

#[cfg(unix)]
fn ensure_no_lossy_alias(project_dir: &Path) -> Result<(), Error> {
    use std::{
        os::unix::ffi::OsStrExt,
        path::{Component, PathBuf},
    };

    // Godot can normalize a literal '\\' in a different cwd into '/'. Explore
    // existing paths that would normalize to this target and fail closed on an
    // alias, an unreadable path, or an excessive number of candidates.
    let mut prefix = PathBuf::from("/");
    let mut inspected = 0usize;
    for part in project_dir
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
    {
        let entries = std::fs::read_dir(&prefix).map_err(|_| {
            Error::InvalidTarget("cannot safely inspect project path aliases".into())
        })?;
        for entry in entries {
            inspected += 1;
            if inspected > 16_384 {
                return Err(Error::InvalidTarget(
                    "cannot safely inspect project path aliases".into(),
                ));
            }
            let entry = entry.map_err(|_| {
                Error::InvalidTarget("cannot safely inspect project path aliases".into())
            })?;
            let name = entry.file_name();
            if !name.as_bytes().contains(&b'\\') {
                continue;
            }
            let path = entry.path();
            let text = path.to_str().ok_or_else(|| {
                Error::InvalidTarget("cannot safely inspect project path aliases".into())
            })?;
            let normalized = text.replace('\\', "/");
            let mut candidate = PathBuf::from("/");
            for component in Path::new(&normalized).components() {
                match component {
                    Component::Normal(value) => candidate.push(value),
                    Component::ParentDir => {
                        candidate.pop();
                    }
                    _ => {}
                }
            }
            if project_dir.starts_with(&candidate) {
                match std::fs::metadata(path) {
                    Ok(metadata) if metadata.is_dir() => {
                        return Err(Error::InvalidTarget(
                            "project path has an ambiguous backslash alias".into(),
                        ))
                    }
                    Ok(_) => {}
                    Err(_) => {
                        return Err(Error::InvalidTarget(
                            "cannot safely inspect project path aliases".into(),
                        ))
                    }
                }
            }
        }
        prefix.push(part);
    }
    Ok(())
}

fn remaining(deadline: Instant) -> Result<Duration, Error> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(Error::Timeout)
}
fn map_socket_io(stream: &TcpStream, error: io::Error) -> Error {
    // macOS can report EINVAL for a socket option after a peer resets the connection.
    if error.kind() == io::ErrorKind::InvalidInput && stream.peer_addr().is_err() {
        Error::Disconnected
    } else {
        map_io(error)
    }
}
fn map_io(error: io::Error) -> Error {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Error::Timeout,
        io::ErrorKind::UnexpectedEof
        | io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::NotConnected => Error::Disconnected,
        _ => Error::Io(error),
    }
}
