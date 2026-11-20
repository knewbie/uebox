//! Read-only Agent tools bound to one already-authorized Unity connection.
//!
//! The host must associate the supplied core connection token with this exact
//! client. Coplay cannot authenticate an instance or prove that association.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, TryLockError};
use uebox_agent_core::{
    Engine, Error, ReadOnlyTool, RunTarget, ToolDefinition, ToolError, ToolFuture, ToolRegistry,
};
use uebox_engine_core::{
    ConnectionToken, EditorSelection, EngineKind, EngineRegistry, ProjectIdentity,
};
use uebox_engine_unity::{GameObjectNameOutcome, PreparedGameObjectName, UnityClient};

type Validator = dyn Fn(&RunTarget) -> bool + Send + Sync;
type Claims = Mutex<HashSet<u64>>;
static CONNECTION_CLAIMS: OnceLock<Claims> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindError {
    InvalidSelection,
    ProjectMismatch,
    AlreadyBound,
    RegistryUnavailable,
    WorkerFailed,
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidSelection => "Unity selection or connection token is not current",
            Self::ProjectMismatch => "Unity client and selected project do not match",
            Self::AlreadyBound => "Unity connection token is already bound",
            Self::RegistryUnavailable => "engine registry is unavailable",
            Self::WorkerFailed => "Unity binding worker failed",
        })
    }
}

impl std::error::Error for BindError {}

/// Owns one connected client. Clones and tools share that exact connection.
#[derive(Clone)]
pub struct UnityTools {
    inner: Arc<Binding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeState {
    Ready,
    Busy,
    Closed,
}

// Field drop order keeps the host reservation until after the socket closes.
struct BoundClient {
    client: UnityClient,
    _socket_guard: Box<dyn Send>,
}

type ClientSlot = Arc<tokio::sync::Mutex<Option<BoundClient>>>;

struct Binding {
    target: RunTarget,
    registry: Arc<Mutex<EngineRegistry>>,
    selection: EditorSelection,
    token: ConnectionToken,
    client: ClientSlot,
    usable: AtomicBool,
}

impl UnityTools {
    /// Consume an existing explicitly authorized client and bind it to its core
    /// token and selection. The host must have issued that token for this very
    /// client. This does not create a connection or obtain user authorization.
    /// Filesystem verification of the selected project runs on a blocking worker.
    pub async fn new(
        client: UnityClient,
        registry: Arc<Mutex<EngineRegistry>>,
        selection: EditorSelection,
        token: ConnectionToken,
    ) -> Result<Self, BindError> {
        Self::new_with_socket_guard(client, registry, selection, token, ()).await
    }

    /// Transfer a host reservation to the socket and its blocking workers.
    pub async fn new_with_socket_guard(
        client: UnityClient,
        registry: Arc<Mutex<EngineRegistry>>,
        selection: EditorSelection,
        token: ConnectionToken,
        socket_guard: impl Send + 'static,
    ) -> Result<Self, BindError> {
        let client = BoundClient {
            client,
            _socket_guard: Box::new(socket_guard),
        };
        tokio::task::spawn_blocking(move || Self::bind(client, registry, selection, token))
            .await
            .map_err(|_| BindError::WorkerFailed)?
    }

    fn bind(
        client: BoundClient,
        registry: Arc<Mutex<EngineRegistry>>,
        selection: EditorSelection,
        token: ConnectionToken,
    ) -> Result<Self, BindError> {
        if selection.project().engine() != EngineKind::Unity
            || selection.generation() != token.generation()
        {
            return Err(BindError::InvalidSelection);
        }
        // Only the caller-selected path is resolved, never a path from the peer.
        let actual = ProjectIdentity::new(
            EngineKind::Unity,
            &client.client.target().expected_project_root,
        )
        .map_err(|_| BindError::ProjectMismatch)?;
        if &actual != selection.project()
            || !reported_project_matches(
                client
                    .client
                    .last_observed_project_info()
                    .project_root
                    .as_str(),
                selection.project(),
            )
        {
            return Err(BindError::ProjectMismatch);
        }
        let generation = selection.generation();
        let target = RunTarget::new(
            Engine::Unity,
            format!("unity-project-{generation}"),
            format!("unity-instance-{generation}"),
            generation,
        )
        .map_err(|_| BindError::InvalidSelection)?;

        let registry_guard = registry
            .lock()
            .map_err(|_| BindError::RegistryUnavailable)?;
        if !selection_matches(&registry_guard, &selection, &token) {
            return Err(BindError::InvalidSelection);
        }
        let mut claims = CONNECTION_CLAIMS
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .map_err(|_| BindError::RegistryUnavailable)?;
        if claims.contains(&token.generation()) {
            return Err(BindError::AlreadyBound);
        }
        let inner = Arc::new(Binding {
            target,
            registry: registry.clone(),
            selection,
            token,
            client: Arc::new(tokio::sync::Mutex::new(Some(client))),
            usable: AtomicBool::new(true),
        });
        claims.insert(inner.token.generation());
        drop(claims);
        drop(registry_guard);
        Ok(Self { inner })
    }

    pub fn project_root(&self) -> &std::path::Path {
        self.inner.selection.project().root()
    }

    pub fn run_target(&self) -> RunTarget {
        self.inner.target.clone()
    }

    /// Terminal state is monotonic; a live socket still needs a successful probe or read.
    pub fn is_retired(&self) -> bool {
        !self.inner.usable.load(Ordering::Acquire)
    }

    /// Prepare the exact selected GameObject through the held socket.
    pub async fn prepare_game_object_name(
        &self,
        target: &RunTarget,
        name: String,
    ) -> Result<PreparedGameObjectName, ToolError> {
        let binding = Arc::clone(&self.inner);
        let mut slot = binding
            .client
            .clone()
            .try_lock_owned()
            .map_err(|_| ToolError)?;
        let target = target.clone();
        tokio::task::spawn_blocking(move || {
            if !binding.current_on_worker(&target) {
                return Err(ToolError);
            }
            let owned = slot.as_mut().ok_or(ToolError)?;
            let prepared = owned
                .client
                .prepare_selected_game_object_name(&name)
                .map_err(|_| ToolError)?;
            if binding.current_on_worker(&target) {
                Ok(prepared)
            } else {
                Err(ToolError)
            }
        })
        .await
        .map_err(|_| ToolError)?
    }

    /// Apply on the originally bound client. The socket guard remains live if
    /// the calling assistant stops waiting during this blocking operation.
    pub async fn apply_game_object_name(
        &self,
        target: &RunTarget,
        prepared: PreparedGameObjectName,
    ) -> GameObjectNameOutcome {
        let binding = Arc::clone(&self.inner);
        let Ok(mut slot) = binding.client.clone().try_lock_owned() else {
            return GameObjectNameOutcome::NotExecuted {
                reason: "target_busy",
            };
        };
        let target = target.clone();
        let result = tokio::task::spawn_blocking(move || {
            if !binding.current_on_worker(&target) {
                return GameObjectNameOutcome::NotExecuted {
                    reason: "target_changed",
                };
            }
            let Some(owned) = slot.as_mut() else {
                return GameObjectNameOutcome::NotExecuted {
                    reason: "bridge_unavailable",
                };
            };
            let result = owned.client.apply_selected_game_object_name(prepared);
            if result == GameObjectNameOutcome::Unknown {
                binding.retire();
                slot.take();
            }
            result
        })
        .await;
        match result {
            Ok(outcome) => outcome,
            Err(_) => {
                self.close().await;
                GameObjectNameOutcome::Unknown
            }
        }
    }

    /// Probe this socket without queueing behind a blocking read or opening another connection.
    pub async fn probe(&self) -> ProbeState {
        if self.is_retired() {
            return ProbeState::Closed;
        }
        let Ok(mut slot) = self.inner.client.clone().try_lock_owned() else {
            return ProbeState::Busy;
        };
        let binding = self.inner.clone();
        let result = tokio::task::spawn_blocking(move || {
            let ready = binding.current_on_worker(&binding.target)
                && slot
                    .as_mut()
                    .is_some_and(|owned| owned.client.ping().is_ok())
                && binding.current_on_worker(&binding.target);
            if ready {
                ProbeState::Ready
            } else {
                binding.retire();
                slot.take();
                ProbeState::Closed
            }
        })
        .await;
        match result {
            Ok(state) => state,
            Err(_) => {
                self.close().await;
                ProbeState::Closed
            }
        }
    }

    /// Retire before waiting. The reservation stays with the actual socket until it closes.
    pub async fn close(&self) {
        self.inner.retire();
        self.inner.client.lock().await.take();
    }

    /// Construct the only two supported tool declarations for this target.
    pub fn tool_registry(&self) -> Result<ToolRegistry, Error> {
        let mut registry = ToolRegistry::new(self.run_target());
        let schema = json!({"type":"object","properties":{},"additionalProperties":false});
        registry.register(
            Engine::Unity,
            ToolDefinition::new(
                "unity_project_info",
                "Read the selected Unity project",
                schema.clone(),
            ),
            self.project_info_tool(),
        )?;
        registry.register(
            Engine::Unity,
            ToolDefinition::new(
                "unity_active_scene",
                "Read the selected Unity active scene",
                schema,
            ),
            self.active_scene_tool(),
        )?;
        Ok(registry)
    }

    /// A nonblocking core validator. It is a binding check, not a network
    /// liveness probe; the live read itself reports an EOF or other failure.
    pub fn validator(&self) -> Arc<Validator> {
        let inner = self.inner.clone();
        Arc::new(move |target| inner.current_nonblocking(target))
    }

    pub fn project_info_tool(&self) -> Arc<dyn ReadOnlyTool> {
        Arc::new(UnityRead {
            inner: self.inner.clone(),
            operation: Operation::Project,
        })
    }

    pub fn active_scene_tool(&self) -> Arc<dyn ReadOnlyTool> {
        Arc::new(UnityRead {
            inner: self.inner.clone(),
            operation: Operation::Scene,
        })
    }
}

// The Unity client's snapshot was checked during its original connection.
// Requiring its spelling to match the selected canonical root catches a
// selected symlink retargeted between connection and binding. This is a
// conservative string check: never resolve a path supplied by the editor.
fn reported_project_matches(reported: &str, project: &ProjectIdentity) -> bool {
    #[cfg(windows)]
    {
        project
            .root()
            .to_str()
            .is_some_and(|root| windows_local_paths_equal(root, reported))
    }
    #[cfg(not(windows))]
    {
        project.root() == std::path::Path::new(reported)
    }
}

#[cfg(any(windows, test))]
fn windows_local_paths_equal(left: &str, right: &str) -> bool {
    fn local(root: &str) -> Option<(String, bool)> {
        let (root, verbatim) = match root.strip_prefix(r"\\?\") {
            Some(root) => (root, true),
            None => (root, false),
        };
        let bytes = root.as_bytes();
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || (bytes[2] != b'\\' && (verbatim || bytes[2] != b'/'))
            || root.contains('\0')
            || (verbatim && root.contains('/'))
        {
            return None;
        }
        let root = if verbatim {
            root.to_owned()
        } else {
            root.replace('/', "\\")
        };
        if root[3..].split('\\').any(|part| part == "..") {
            return None;
        }
        Some((root, verbatim))
    }
    let (Some((left, left_verbatim)), Some((right, right_verbatim))) = (local(left), local(right))
    else {
        return false;
    };
    if left != right {
        return false;
    }
    if left_verbatim == right_verbatim {
        return true;
    }
    let rest = &left[3..];
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
            let upper = part
                .split('.')
                .next()
                .unwrap_or("")
                .trim_end_matches(' ')
                .to_ascii_uppercase();
            if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
                return false;
            }
            let bytes = upper.as_bytes();
            !(bytes.len() == 4
                && (bytes.starts_with(b"COM") || bytes.starts_with(b"LPT"))
                && (b'1'..=b'9').contains(&bytes[3]))
        })
}

fn selection_matches(
    registry: &EngineRegistry,
    selection: &EditorSelection,
    token: &ConnectionToken,
) -> bool {
    if selection.project().engine() != EngineKind::Unity
        || token.generation() != selection.generation()
    {
        return false;
    }
    let Ok(session) = registry.resolve_selection(selection) else {
        return false;
    };
    let Ok(exact) = registry.select_exact(token, selection.project(), false) else {
        return false;
    };
    session.project() == selection.project()
        && session.instance_id() == selection.instance_id()
        && session.generation() == selection.generation()
        && exact.project() == selection.project()
        && exact.instance_id() == selection.instance_id()
        && exact.generation() == selection.generation()
}

impl Binding {
    fn current_nonblocking(&self, target: &RunTarget) -> bool {
        if &self.target != target || !self.usable.load(Ordering::Acquire) {
            return false;
        }
        let Ok(registry) = self.registry.try_lock() else {
            return false;
        };
        let current = selection_matches(&registry, &self.selection, &self.token);
        drop(registry);
        if !current && self.usable.swap(false, Ordering::AcqRel) {
            // A stale binding must not keep its original socket alive merely
            // because Agent core stopped before calling the tool. Cleanup waits
            // for any in-flight worker to release the client first.
            defer_cleanup(
                self.client.clone(),
                self.registry.clone(),
                self.token.clone(),
            );
        }
        current
    }

    fn current_on_worker(&self, target: &RunTarget) -> bool {
        if &self.target != target || !self.usable.load(Ordering::Acquire) {
            return false;
        }
        self.registry
            .lock()
            .ok()
            .is_some_and(|registry| selection_matches(&registry, &self.selection, &self.token))
    }

    fn retire(&self) {
        self.usable.store(false, Ordering::Release);
        // A stale token cannot close a newer generation. Recover a poisoned
        // registry only for this conservative cleanup, never for validation.
        self.registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .close_connection(&self.token);
    }
}

impl Drop for Binding {
    fn drop(&mut self) {
        self.usable.store(false, Ordering::Release);
        let generation = self.token.generation();
        if let Ok(mut client) = self.client.try_lock() {
            // Close the transport before making its registry token reusable.
            client.take();
        } else {
            defer_cleanup(
                self.client.clone(),
                self.registry.clone(),
                self.token.clone(),
            );
            return;
        }
        match self.registry.try_lock() {
            Ok(mut registry) => {
                registry.close_connection(&self.token);
                release_claim(generation);
            }
            Err(TryLockError::Poisoned(poisoned)) => {
                poisoned.into_inner().close_connection(&self.token);
                release_claim(generation);
            }
            Err(TryLockError::WouldBlock) => {
                // Drop must not wait on a host holding the registry lock. Keep
                // the claim reserved until a separate thread retires the token.
                defer_cleanup(
                    self.client.clone(),
                    self.registry.clone(),
                    self.token.clone(),
                );
            }
        }
    }
}

fn defer_cleanup(client: ClientSlot, registry: Arc<Mutex<EngineRegistry>>, token: ConnectionToken) {
    let _ = std::thread::Builder::new()
        .name("unity-token-cleanup".into())
        .spawn(move || {
            client.blocking_lock().take();
            registry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .close_connection(&token);
            release_claim(token.generation());
        });
}

fn release_claim(generation: u64) {
    if let Some(claims) = CONNECTION_CLAIMS.get() {
        claims
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&generation);
    }
}

// Scene.path is a Unity asset path, not a native filesystem path. This is
// purely lexical: it cannot establish the peer's identity or prove where a
// package's underlying files live, and must never resolve the peer's path.
fn valid_scene_path(path: &str) -> bool {
    if path.is_empty() {
        return true; // A new, unsaved scene has no asset path yet.
    }
    if path
        .chars()
        .any(|character| character.is_control() || character == '\\' || character == ':')
    {
        return false;
    }
    let mut components = path.split('/');
    let minimum_remaining = match components.next() {
        Some("Assets") => 1,
        Some("Packages") => 2, // Package name and an asset below it.
        _ => return false,
    };
    let mut count = 0;
    for component in components {
        if component.is_empty() || component == "." || component == ".." {
            return false;
        }
        count += 1;
    }
    count >= minimum_remaining
}

#[derive(Clone, Copy)]
enum Operation {
    Project,
    Scene,
}

struct UnityRead {
    inner: Arc<Binding>,
    operation: Operation,
}

impl ReadOnlyTool for UnityRead {
    fn execute(&self, target: RunTarget, arguments: Value) -> ToolFuture {
        let inner = self.inner.clone();
        let operation = self.operation;
        Box::pin(async move {
            if target != inner.target
                || !arguments
                    .as_object()
                    .is_some_and(|object| object.is_empty())
                || !inner.usable.load(Ordering::Acquire)
            {
                return Err(ToolError);
            }
            // Fail busy rather than queue a read that could start after the
            // Agent run was cancelled. The worker owns the guard until I/O ends.
            let mut client = inner
                .client
                .clone()
                .try_lock_owned()
                .map_err(|_| ToolError)?;
            let worker_inner = inner.clone();
            let worker = tokio::task::spawn_blocking(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if !worker_inner.current_on_worker(&target) {
                        return Err(ToolError);
                    }
                    let bridge = &mut client.as_mut().ok_or(ToolError)?.client;
                    let value = match operation {
                        Operation::Project => {
                            let project = bridge.project_info().map_err(|_| ToolError)?;
                            if !reported_project_matches(
                                &project.project_root,
                                worker_inner.selection.project(),
                            ) {
                                return Err(ToolError);
                            }
                            json!({"project_name":project.project_name,"unity_version":project.unity_version,
                                "platform":project.platform,"render_pipeline":project.render_pipeline})
                        }
                        Operation::Scene => {
                            // The client checks its original selected path; also
                            // compare the live reply with the pinned core project
                            // before asking for a scene on that same connection.
                            let project = bridge.project_info().map_err(|_| ToolError)?;
                            if !reported_project_matches(
                                &project.project_root,
                                worker_inner.selection.project(),
                            ) || !worker_inner.current_on_worker(&target)
                            {
                                return Err(ToolError);
                            }
                            let scene = bridge.active_scene().map_err(|_| ToolError)?;
                            if !reported_project_matches(
                                &bridge.last_observed_project_info().project_root,
                                worker_inner.selection.project(),
                            ) || !valid_scene_path(&scene.path)
                            {
                                return Err(ToolError);
                            }
                            json!({"name":scene.name,"path":scene.path,"build_index":scene.build_index,
                                "is_dirty":scene.is_dirty,"is_loaded":scene.is_loaded,"root_count":scene.root_count})
                        }
                    };
                    if !worker_inner.current_on_worker(&target) {
                        return Err(ToolError);
                    }
                    Ok(value)
                }));
                let result = match result {
                    Ok(Ok(value)) => Ok(value),
                    _ => {
                        worker_inner.retire();
                        client.take();
                        Err(ToolError)
                    }
                };
                drop(client);
                result
            });
            match worker.await {
                Ok(result) => result,
                Err(_) => {
                    // The worker has stopped and its guard has been dropped.
                    // Mark unusable even if the runtime interrupted the task.
                    inner.usable.store(false, Ordering::Release);
                    defer_cleanup(
                        inner.client.clone(),
                        inner.registry.clone(),
                        inner.token.clone(),
                    );
                    Err(ToolError)
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::windows_local_paths_equal;

    #[test]
    fn windows_project_snapshot_comparison_is_conservative_and_never_resolves_paths() {
        assert!(windows_local_paths_equal(
            r"\\?\C:\work\Project A",
            "C:/work/Project A"
        ));
        assert!(windows_local_paths_equal(
            r"\\?\C:\work\Project A",
            r"\\?\C:\work\Project A"
        ));
        for (left, right) in [
            (r"\\?\C:\work\Foo", r"C:\work\foo"),
            (r"\\?\C:\work\Project.", r"C:\work\Project."),
            (r"\\?\C:\work\COM1.txt", r"C:\work\COM1.txt"),
            (r"\\?\C:\work\项目", r"C:\work\项目"),
            (r"\\?\C:\work\Project", r"\\server\share\Project"),
            (r"\\?\C:\work\Project", r"\\?\UNC\server\share\Project"),
        ] {
            assert!(
                !windows_local_paths_equal(left, right),
                "accepted {left:?}, {right:?}"
            );
        }
    }
}
