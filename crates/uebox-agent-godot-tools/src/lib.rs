//! Read-only Agent tools for one explicitly connected Godot session.
//!
//! The host owns launch and registry lifecycle. No discovery, launch, reconnect,
//! generic command, credential or environment access is provided here.

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use uebox_agent_core::{
    Engine, ReadOnlyTool, RunTarget, ToolDefinition, ToolError, ToolFuture, ToolRegistry,
};
use uebox_engine_core::{EditorSelection, EngineKind, EngineRegistry};
use uebox_godot_session::{GodotSession, SessionState};

/// The same release-aware handle used by the Godot host session manager.
pub type GodotSessionHandle = Arc<Mutex<Option<GodotSession>>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindError {
    WrongEngine,
    StaleSelection,
    SessionMismatch,
    SessionNotReady,
    Busy,
    InvalidTarget,
}
impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Godot tool binding failed: {self:?}")
    }
}
impl std::error::Error for BindError {}

struct Binding {
    registry: Arc<Mutex<EngineRegistry>>,
    selection: EditorSelection,
    session: GodotSessionHandle,
    target: RunTarget,
    gate: Arc<tokio::sync::Mutex<()>>,
}

/// An immutable selection and its original shared session. Never serializable.
pub struct GodotToolBinding {
    inner: Arc<Binding>,
}

impl GodotToolBinding {
    /// Bind only a live core selection to the already-connected exact session.
    /// A contended or poisoned lock fails closed; this method never starts Godot.
    pub fn new(
        registry: Arc<Mutex<EngineRegistry>>,
        selection: EditorSelection,
        session: GodotSessionHandle,
    ) -> Result<Arc<Self>, BindError> {
        if selection.project().engine() != EngineKind::Godot {
            return Err(BindError::WrongEngine);
        }
        {
            let registry_guard = registry.try_lock().map_err(|_| BindError::Busy)?;
            registry_guard
                .resolve_selection(&selection)
                .map_err(|_| BindError::StaleSelection)?;
        }
        {
            let mut guard = session.try_lock().map_err(|_| BindError::Busy)?;
            let editor = guard.as_mut().ok_or(BindError::SessionNotReady)?;
            if editor.project_path() != selection.project().root()
                || editor.instance_id() != selection.instance_id()
            {
                return Err(BindError::SessionMismatch);
            }
            if editor.status().ok() != Some(SessionState::Ready) {
                return Err(BindError::SessionNotReady);
            }
        }
        // Agent core includes RunTarget in model-facing tool receipts. Use an
        // opaque per-connection project ID rather than leaking the local path.
        let target = RunTarget::new(
            Engine::Godot,
            format!("godot-project-{}", selection.generation()),
            format!("godot-instance-{}", selection.generation()),
            selection.generation(),
        )
        .map_err(|_| BindError::InvalidTarget)?;
        Ok(Arc::new(Self {
            inner: Arc::new(Binding {
                registry,
                selection,
                session,
                target,
                gate: Arc::new(tokio::sync::Mutex::new(())),
            }),
        }))
    }

    pub fn target(&self) -> RunTarget {
        self.inner.target.clone()
    }

    /// Nonblocking Agent validator; dispatch additionally validates the actual session.
    pub fn validator(&self) -> Arc<dyn Fn(&RunTarget) -> bool + Send + Sync> {
        let inner = self.inner.clone();
        Arc::new(move |target| inner.validator_current(target))
    }

    pub fn project_info_tool(&self) -> Arc<dyn ReadOnlyTool> {
        Arc::new(GodotRead {
            inner: self.inner.clone(),
            operation: Operation::Project,
        })
    }
    pub fn scene_tree_tool(&self) -> Arc<dyn ReadOnlyTool> {
        Arc::new(GodotRead {
            inner: self.inner.clone(),
            operation: Operation::Scene,
        })
    }

    /// Build a registry for this exact target with only the two read-only tools.
    pub fn tool_registry(&self) -> Result<ToolRegistry, uebox_agent_core::Error> {
        let mut registry = ToolRegistry::new(self.target());
        let schema = json!({"type":"object","properties":{},"additionalProperties":false});
        registry.register(
            Engine::Godot,
            ToolDefinition::new(
                "godot_project_info",
                "Read the selected Godot editor project",
                schema.clone(),
            ),
            self.project_info_tool(),
        )?;
        registry.register(
            Engine::Godot,
            ToolDefinition::new(
                "godot_scene_tree",
                "Read the selected Godot editor's current scene tree",
                schema,
            ),
            self.scene_tree_tool(),
        )?;
        Ok(registry)
    }
}

impl Binding {
    fn current(&self, target: &RunTarget) -> bool {
        if target != &self.target || target.engine() != Engine::Godot {
            return false;
        }
        let Ok(registry) = self.registry.try_lock() else {
            return false;
        };
        registry.resolve_selection(&self.selection).is_ok()
    }
    fn validator_current(&self, target: &RunTarget) -> bool {
        if !self.current(target) {
            return false;
        }
        match self.session.try_lock() {
            Ok(mut guard) => guard
                .as_mut()
                .is_some_and(|session| self.same_ready_session(session)),
            Err(std::sync::TryLockError::WouldBlock) => true,
            Err(std::sync::TryLockError::Poisoned(_)) => false,
        }
    }
    fn same_ready_session(&self, session: &mut GodotSession) -> bool {
        session.project_path() == self.selection.project().root()
            && session.instance_id() == self.selection.instance_id()
            && session.status().ok() == Some(SessionState::Ready)
    }
}

#[derive(Clone, Copy)]
enum Operation {
    Project,
    Scene,
}
struct GodotRead {
    inner: Arc<Binding>,
    operation: Operation,
}
impl ReadOnlyTool for GodotRead {
    fn execute(&self, target: RunTarget, arguments: Value) -> ToolFuture {
        let inner = self.inner.clone();
        let operation = self.operation;
        Box::pin(async move {
            if arguments != json!({}) || !inner.current(&target) {
                return Err(ToolError);
            }
            let permit = inner.gate.clone().lock_owned().await;
            if !inner.current(&target) {
                return Err(ToolError);
            }
            // The permit and session mutex are owned by the blocking closure, not
            // by the awaiting future. Dropping/cancelling the future cannot free
            // the physical session while the TCP operation is still running.
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let mut guard = inner.session.lock().map_err(|_| ToolError)?;
                let session = guard.as_mut().ok_or(ToolError)?;
                if !inner.current(&target) || !inner.same_ready_session(session) { return Err(ToolError); }
                let value = match operation {
                    Operation::Project => {
                        let project = session.project_info().map_err(|_| ToolError)?;
                        json!({"name":project.name,"godot_version":project.godot_version})
                    }
                    Operation::Scene => {
                        let scene = session.scene_tree().map_err(|_| ToolError)?;
                        let nodes: Vec<_> = scene.nodes.into_iter().map(|node| json!({"path":node.path,"name":node.name,"class":node.class})).collect();
                        json!({"scene_path":scene.scene_path,"nodes":nodes,"truncated":scene.truncated})
                    }
                };
                if !inner.current(&target) || !inner.same_ready_session(session) { return Err(ToolError); }
                if contains_sensitive_value(&value, session) { return Err(ToolError); }
                Ok(value)
            }).await.map_err(|_| ToolError)?
        })
    }
}

// The session only exposes a boolean exact-match check, never its token.
fn contains_sensitive_value(value: &Value, session: &GodotSession) -> bool {
    match value {
        Value::String(text) => session.contains_private_bridge_values(text),
        Value::Array(items) => items
            .iter()
            .any(|item| contains_sensitive_value(item, session)),
        Value::Object(entries) => entries.iter().any(|(key, value)| {
            session.contains_private_bridge_values(key) || contains_sensitive_value(value, session)
        }),
        _ => false,
    }
}
