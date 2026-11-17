//! In-process listener for the existing UnrealAgentLink WebSocket protocol.
//!
//! Call UnrealBridge::start in a Tokio runtime, list identified instances,
//! then explicitly select one before issuing the read-only project query.
//! A selection is tied to a particular connection and project identity; it
//! never follows a reconnect or automatically retries a request.

use std::{
    collections::HashMap,
    error::Error,
    fmt, io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
    time::{interval_at, sleep_until, timeout, timeout_at, Instant, MissedTickBehavior},
};
use tokio_tungstenite::{
    accept_hdr_async_with_config,
    tungstenite::{
        handshake::server::{Callback, ErrorResponse, Request, Response},
        http::header::ORIGIN,
        protocol::WebSocketConfig,
        Message,
    },
};

/// The address used by the bundled UnrealAgentLink plugin.
pub const DEFAULT_ADDR: &str = "127.0.0.1:17860";
/// Maximum size of an incoming WebSocket message and its individual frames.
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const OUTBOUND_QUEUE_CAPACITY: usize = 64;
// IDs remain distinct when a listener is restarted in the same host process.
static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(0);

/// A live, identified project. A non-interactive project is reported honestly;
/// it can only be queried if the caller explicitly selects it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InstanceSummary {
    /// Opaque selection credential, rotated when the project identity changes.
    pub connection_id: String,
    pub project_name: String,
    pub project_path: String,
    pub interactive: bool,
}

/// Failures are serializable for a Tauri command to return without a sidecar.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum BridgeError {
    UnknownInstance,
    Disconnected,
    StaleSelection,
    Timeout,
    Remote {
        code: Option<i64>,
        message: String,
        result: Value,
    },
    InvalidResponse,
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownInstance => write!(f, "instance is not connected and identified"),
            Self::Disconnected => write!(f, "instance disconnected"),
            Self::StaleSelection => write!(f, "the selected project changed; select it again"),
            Self::Timeout => write!(f, "request timed out; its outcome is unknown"),
            Self::Remote { code, message, .. } => {
                write!(f, "UnrealAgentLink error")?;
                if let Some(code) = code {
                    write!(f, " {code}")?;
                }
                write!(f, ": {message}")
            }
            Self::InvalidResponse => write!(f, "invalid project info response"),
        }
    }
}

impl Error for BridgeError {}

type Reply = oneshot::Sender<Result<Value, BridgeError>>;

struct ConnectionState {
    alive: bool,
    project: Option<InstanceSummary>,
    selection_id: String,
    generation: u64,
    pending: HashMap<String, Reply>,
}

enum Outbound {
    Request { id: String, text: String },
    Ping,
}

struct Connection {
    id: String,
    state: Mutex<ConnectionState>,
    outbound: mpsc::Sender<Outbound>,
}

#[derive(Default)]
struct Shared {
    connections: Mutex<HashMap<String, Arc<Connection>>>,
    next_request: AtomicU64,
}

/// Timing configuration for socket liveness. Defaults are 15s/45s/45s.
#[derive(Clone, Copy, Debug)]
pub struct LivenessConfig {
    /// Interval between WebSocket pings. Must be positive and less than the inactivity timeout.
    pub ping_interval: Duration,
    /// Maximum time without an inbound WebSocket message or control frame.
    pub inactivity_timeout: Duration,
    /// Time allowed to announce a project, independent of heartbeat or pong.
    pub registration_timeout: Duration,
}

impl Default for LivenessConfig {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(15),
            inactivity_timeout: Duration::from_secs(45),
            registration_timeout: Duration::from_secs(45),
        }
    }
}

impl LivenessConfig {
    fn validate(self) -> io::Result<()> {
        let now = Instant::now();
        if self.ping_interval.is_zero()
            || self.inactivity_timeout.is_zero()
            || self.registration_timeout.is_zero()
            || self.ping_interval >= self.inactivity_timeout
            || now.checked_add(self.ping_interval).is_none()
            || now.checked_add(self.inactivity_timeout).is_none()
            || now.checked_add(self.registration_timeout).is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "liveness durations must be positive and ping interval less than inactivity timeout",
            ));
        }
        Ok(())
    }
}

/// A loopback-only listener. Dropping it also initiates shutdown.
pub struct UnrealBridge {
    shared: Arc<Shared>,
    address: SocketAddr,
    stop: watch::Sender<bool>,
    listener_task: Option<JoinHandle<()>>,
}

/// Explicit authorization to query one identified project on one connection.
#[derive(Clone)]
pub struct SelectedInstance {
    shared: Arc<Shared>,
    connection: Arc<Connection>,
    generation: u64,
}

impl UnrealBridge {
    /// Listen on the plugin's standard address, 127.0.0.1:17860.
    pub async fn start() -> io::Result<Self> {
        Self::bind(17860).await
    }

    /// Listen on 127.0.0.1 at the specified port. Port 0 requests a temporary
    /// OS-assigned port, useful for isolated protocol tests.
    pub async fn bind(port: u16) -> io::Result<Self> {
        Self::bind_with_liveness(port, LivenessConfig::default()).await
    }

    /// Bind to IPv4 loopback with explicit liveness timings. Invalid durations return InvalidInput.
    pub async fn bind_with_liveness(port: u16, config: LivenessConfig) -> io::Result<Self> {
        config.validate()?;
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).await?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared::default());
        let (stop, mut stopped) = watch::channel(false);
        let task_shared = Arc::clone(&shared);
        let listener_task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    changed = stopped.changed() => {
                        if changed.is_err() || *stopped.borrow() {
                            break;
                        }
                    }
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => {
                            clients.spawn(handle_client(stream, Arc::clone(&task_shared), config));
                        }
                        Err(_) => break,
                    },
                    _ = clients.join_next(), if !clients.is_empty() => {}
                }
            }
            // Abort handshakes and sockets, then wait for their cleanup guards.
            clients.abort_all();
            while clients.join_next().await.is_some() {}
        });
        Ok(Self {
            shared,
            address,
            stop,
            listener_task: Some(listener_task),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// Snapshot only projects identified by a valid project.info event.
    pub fn instances(&self) -> Vec<InstanceSummary> {
        let connections: Vec<_> = lock(&self.shared.connections).values().cloned().collect();
        let mut projects: Vec<_> = connections
            .iter()
            .filter_map(|connection| {
                let state = lock(&connection.state);
                if state.alive {
                    state.project.clone()
                } else {
                    None
                }
            })
            .collect();
        projects.sort_by(|a, b| a.connection_id.cmp(&b.connection_id));
        projects
    }

    /// Select using the credential from a current project snapshot. Never auto-select a project.
    pub fn select_instance(&self, id: &str) -> Result<SelectedInstance, BridgeError> {
        let connections: Vec<_> = lock(&self.shared.connections).values().cloned().collect();
        for connection in connections {
            let state = lock(&connection.state);
            if state.alive
                && state
                    .project
                    .as_ref()
                    .is_some_and(|project| project.connection_id == id)
            {
                let generation = state.generation;
                drop(state);
                return Ok(SelectedInstance {
                    shared: Arc::clone(&self.shared),
                    connection,
                    generation,
                });
            }
        }
        Err(BridgeError::UnknownInstance)
    }

    /// Close the listener and all connections, failing outstanding requests.
    pub async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = self.listener_task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for UnrealBridge {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

impl SelectedInstance {
    /// Query exactly the selected connection with the plugin's project.info
    /// method. Timeouts and disconnects are not retried automatically.
    pub async fn get_project_info(&self, deadline: Duration) -> Result<Value, BridgeError> {
        let id = format!(
            "req-{}",
            self.shared.next_request.fetch_add(1, Ordering::Relaxed)
        );
        let (reply, received) = oneshot::channel();
        {
            let mut state = lock(&self.connection.state);
            if !state.alive {
                return Err(BridgeError::Disconnected);
            }
            if state.generation != self.generation || state.project.is_none() {
                return Err(BridgeError::StaleSelection);
            }
            state.pending.insert(id.clone(), reply);
        }

        // The guard removes the waiter on timeout, cancellation, or send
        // failure, even if the caller drops this future.
        let _pending = PendingGuard {
            connection: Arc::clone(&self.connection),
            id: id.clone(),
        };
        let text = json!({
            "ver": "1.0",
            "type": "req",
            "id": id,
            "method": "project.info",
            "params": {},
            "time": unix_millis()
        })
        .to_string();
        let outgoing = Outbound::Request { id, text };

        match timeout(deadline, async {
            self.connection
                .outbound
                .send(outgoing)
                .await
                .map_err(|_| BridgeError::Disconnected)?;
            received.await.unwrap_or(Err(BridgeError::Disconnected))
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(BridgeError::Timeout),
        }
    }
}

struct PendingGuard {
    connection: Arc<Connection>,
    id: String,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        lock(&self.connection.state).pending.remove(&self.id);
    }
}

struct ConnectionGuard {
    shared: Arc<Shared>,
    connection: Arc<Connection>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        let pending = {
            let mut state = lock(&self.connection.state);
            state.alive = false;
            state.project = None;
            std::mem::take(&mut state.pending)
        };
        lock(&self.shared.connections).remove(&self.connection.id);
        fail_pending(pending, BridgeError::Disconnected);
    }
}

async fn handle_client(stream: TcpStream, shared: Arc<Shared>, liveness: LivenessConfig) {
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let accepted = timeout(
        HANDSHAKE_TIMEOUT,
        accept_hdr_async_with_config(stream, OriginCheck, Some(config)),
    )
    .await;
    let mut socket = match accepted {
        Ok(Ok(socket)) => socket,
        _ => return,
    };

    let connected_at = Instant::now();
    let id = format!("ue-{}", NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed));
    let (outbound, mut commands) = mpsc::channel::<Outbound>(OUTBOUND_QUEUE_CAPACITY);
    let connection = Arc::new(Connection {
        id: id.clone(),
        state: Mutex::new(ConnectionState {
            alive: true,
            project: None,
            selection_id: id.clone(),
            generation: 0,
            pending: HashMap::new(),
        }),
        outbound,
    });
    lock(&shared.connections).insert(id.clone(), Arc::clone(&connection));
    let _guard = ConnectionGuard {
        shared,
        connection: Arc::clone(&connection),
    };

    // Match the existing Electron server's informational welcome event.
    let welcome = json!({
        "ver":"1.0",
        "type":"evt",
        "method":"system.connected",
        "payload":{"connectionId":id},
        "time":unix_millis()
    });
    let welcome_deadline = connected_at
        + liveness
            .inactivity_timeout
            .min(liveness.registration_timeout);
    if !matches!(
        timeout_at(
            welcome_deadline,
            socket.send(Message::Text(welcome.to_string().into()))
        )
        .await,
        Ok(Ok(()))
    ) {
        return;
    }

    let (activity_tx, activity_rx) = watch::channel(connected_at);
    let (mut writer, mut reader) = socket.split();
    let write_connection = Arc::clone(&connection);
    let writing = async move {
        while let Some(command) = commands.recv().await {
            let message = match command {
                Outbound::Request { id, text } => {
                    // A queued request may have timed out or been cancelled.
                    if !lock(&write_connection.state).pending.contains_key(&id) {
                        continue;
                    }
                    Message::Text(text.into())
                }
                Outbound::Ping => Message::Ping(Vec::new().into()),
            };
            if writer.send(message).await.is_err() {
                break;
            }
            // Let liveness deadlines progress even while the queue stays busy.
            tokio::task::yield_now().await;
        }
    };
    let reading = async {
        while let Some(message) = reader.next().await {
            match message {
                Ok(Message::Text(text)) => {
                    if !handle_message(&connection, &text) {
                        break;
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
            // Match the original server: inbound messages and pong are activity.
            let _ = activity_tx.send(Instant::now());
            // A continuous stream must not starve the registration deadline.
            tokio::task::yield_now().await;
        }
    };
    let tracking = track_liveness(Arc::clone(&connection), activity_rx, connected_at, liveness);
    tokio::select! {
        _ = writing => {}
        _ = reading => {}
        _ = tracking => {}
    }
}

/// Stop a socket that does not send anything or fails to identify a project.
/// Registration has its own deadline, even if an unidentified peer sends pong.
async fn track_liveness(
    connection: Arc<Connection>,
    mut activity: watch::Receiver<Instant>,
    connected_at: Instant,
    config: LivenessConfig,
) {
    let mut last_activity = connected_at;
    let mut registration_deadline = connected_at + config.registration_timeout;
    let mut registered = false;
    let mut ping = interval_at(connected_at + config.ping_interval, config.ping_interval);
    ping.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        // If activity and a deadline become ready together, apply the already
        // received activity before deciding whether the socket is stale.
        match activity.has_changed() {
            Ok(true) => {
                last_activity = *activity.borrow_and_update();
                let identified = lock(&connection.state).project.is_some();
                if registered && !identified {
                    registration_deadline = last_activity + config.registration_timeout;
                }
                registered = identified;
            }
            Ok(false) => {}
            Err(_) => break,
        }

        let idle_deadline = last_activity + config.inactivity_timeout;
        let now = Instant::now();
        if now >= idle_deadline || (!registered && now >= registration_deadline) {
            break;
        }
        tokio::select! {
            _ = sleep_until(idle_deadline) => {}
            _ = sleep_until(registration_deadline), if !registered => {}
            _ = ping.tick() => {
                // Do not let a full outgoing queue delay the liveness deadline.
                let _ = connection.outbound.try_send(Outbound::Ping);
            }
            changed = activity.changed() => {
                if changed.is_err() {
                    break;
                }
                // changed() marks the value seen, so apply it before the next
                // deadline comparison, even if the timeout is also ready.
                last_activity = *activity.borrow_and_update();
                let identified = lock(&connection.state).project.is_some();
                if registered && !identified {
                    registration_deadline = last_activity + config.registration_timeout;
                }
                registered = identified;
            }
        }
    }
}

struct OriginCheck;

// Tungstenite fixes this callback's response type in its public trait.
impl Callback for OriginCheck {
    fn on_request(self, request: &Request, response: Response) -> Result<Response, ErrorResponse> {
        let mut origins = request.headers().get_all(ORIGIN).iter();
        let allowed = match origins.next() {
            None => true,
            Some(value) => value.as_bytes() == b"http://127.0.0.1" && origins.next().is_none(),
        };
        if allowed {
            Ok(response)
        } else {
            Err(Response::builder()
                .status(403)
                .body(Some("Forbidden".to_owned()))
                .expect("valid fixed HTTP response"))
        }
    }
}

/// Returns false when the connection should be closed.
fn handle_message(connection: &Arc<Connection>, text: &str) -> bool {
    let Ok(message) = serde_json::from_str::<Value>(text) else {
        return true;
    };
    match (message["type"].as_str(), message["method"].as_str()) {
        (Some("evt"), Some("project.info")) => {
            update_project(connection, &message["payload"]);
        }
        (Some("evt"), Some("project.closed")) => return false,
        (Some("res"), _) => {
            if let Some(id) = message["id"].as_str() {
                let reply = lock(&connection.state).pending.remove(id);
                if let Some(reply) = reply {
                    let _ = reply.send(decode_response(&message));
                }
            }
        }
        _ => {}
    }
    true
}

fn update_project(connection: &Connection, payload: &Value) {
    let name = payload["projectName"]
        .as_str()
        .filter(|s| !s.trim().is_empty());
    let path = payload["projectPath"]
        .as_str()
        .filter(|s| !s.trim().is_empty());
    let interactive = match payload.get("interactive") {
        None => Some(true),
        Some(value) => value.as_bool(),
    };
    let project = match (name, path, interactive) {
        (Some(name), Some(path), Some(interactive)) => Some((name, path, interactive)),
        _ => None,
    };

    let invalidated = {
        let mut state = lock(&connection.state);
        let changed = state.project.as_ref().is_some_and(|old| {
            project.as_ref().is_none_or(|(name, path, _)| {
                old.project_name != *name || old.project_path != *path
            })
        });
        let pending = if changed {
            state.generation += 1;
            // A list snapshot authorizes this identity, not the socket's next project.
            state.selection_id = format!("ue-{}", NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed));
            std::mem::take(&mut state.pending)
        } else {
            HashMap::new()
        };
        state.project = project.map(|(name, path, interactive)| InstanceSummary {
            connection_id: state.selection_id.clone(),
            project_name: name.to_owned(),
            project_path: path.to_owned(),
            interactive,
        });
        pending
    };
    fail_pending(invalidated, BridgeError::StaleSelection);
}

fn decode_response(message: &Value) -> Result<Value, BridgeError> {
    let result = ["result", "data", "payload"]
        .iter()
        .filter_map(|key| message.get(key))
        .find(|value| !value.is_null())
        .cloned()
        .unwrap_or(Value::Null);
    let code = message.get("code").and_then(Value::as_i64);
    let is_error_code = message
        .get("code")
        .and_then(Value::as_f64)
        .is_some_and(|code| code >= 400.0);
    let is_failure = result.get("ok") == Some(&Value::Bool(false))
        || result.get("success") == Some(&Value::Bool(false))
        || message.get("success") == Some(&Value::Bool(false));
    if is_error_code || is_failure {
        let detail = result
            .get("error")
            .or_else(|| result.get("message"))
            .or_else(|| message.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("UnrealAgentLink returned an error")
            .to_owned();
        return Err(BridgeError::Remote {
            code,
            message: detail,
            result,
        });
    }
    if !result.is_object() {
        return Err(BridgeError::InvalidResponse);
    }
    Ok(result)
}

fn fail_pending(pending: HashMap<String, Reply>, error: BridgeError) {
    for reply in pending.into_values() {
        let _ = reply.send(Err(error.clone()));
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[tokio::test]
    async fn timeout_and_cancellation_remove_pending_waiters() {
        let (outbound, mut outgoing) = mpsc::channel(4);
        let connection = Arc::new(Connection {
            id: "test-connection".to_owned(),
            state: Mutex::new(ConnectionState {
                alive: true,
                project: Some(InstanceSummary {
                    connection_id: "test-connection".to_owned(),
                    project_name: "test".to_owned(),
                    project_path: "/test".to_owned(),
                    interactive: true,
                }),
                selection_id: "test-connection".to_owned(),
                generation: 0,
                pending: HashMap::new(),
            }),
            outbound,
        });
        let selected = SelectedInstance {
            shared: Arc::new(Shared::default()),
            connection: Arc::clone(&connection),
            generation: 0,
        };

        let timed = tokio::spawn({
            let selected = selected.clone();
            async move { selected.get_project_info(Duration::from_millis(100)).await }
        });
        outgoing.recv().await.unwrap();
        assert_eq!(lock(&connection.state).pending.len(), 1);
        assert_eq!(timed.await.unwrap(), Err(BridgeError::Timeout));
        assert!(lock(&connection.state).pending.is_empty());

        let cancelled =
            tokio::spawn(async move { selected.get_project_info(Duration::from_secs(10)).await });
        outgoing.recv().await.unwrap();
        assert_eq!(lock(&connection.state).pending.len(), 1);
        cancelled.abort();
        assert!(cancelled.await.unwrap_err().is_cancelled());
        assert!(lock(&connection.state).pending.is_empty());
    }
}
