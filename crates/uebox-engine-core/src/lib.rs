//! Engine-agnostic project and editor identities with conservative routing.
//!
//! The registry issues a process-unique token for each connection. Adapters
//! must retain that token for identification and disconnect callbacks; a
//! stale callback or selection cannot affect a replacement connection.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// A token from a discarded registry must never match a new registry in the
// same process, even when an adapter reuses its local instance ID.
static NEXT_CONNECTION_GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EngineKind {
    Unreal,
    Unity,
    Godot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InteractionMode {
    Interactive,
    NonInteractive,
    Unknown,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProjectIdentity {
    engine: EngineKind,
    root: PathBuf,
}

impl ProjectIdentity {
    /// Resolve an existing project directory to a canonical path without
    /// changing its case. Engine-specific project detection belongs to adapters.
    pub fn new(engine: EngineKind, root: impl AsRef<Path>) -> std::io::Result<Self> {
        let root = std::fs::canonicalize(root)?;
        if !root.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "project root must be a directory",
            ));
        }
        Ok(Self { engine, root })
    }

    pub fn engine(&self) -> EngineKind {
        self.engine
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorSession {
    project: ProjectIdentity,
    instance_id: String,
    generation: u64,
    interaction_mode: InteractionMode,
}

impl EditorSession {
    pub fn interaction_mode(&self) -> InteractionMode {
        self.interaction_mode
    }
    pub fn project(&self) -> &ProjectIdentity {
        &self.project
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_interactive(&self) -> bool {
        self.interaction_mode == InteractionMode::Interactive
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryError {
    InvalidInstanceId,
    InstanceBelongsToAnotherProject,
    NotFound,
    Ambiguous,
    StaleConnection,
    AlreadyConnected,
    EngineMismatch,
    GenerationExhausted,
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidInstanceId => "editor instance id is empty",
            Self::InstanceBelongsToAnotherProject => {
                "editor instance already belongs to another project"
            }
            Self::NotFound => "no matching editor is connected",
            Self::Ambiguous => "multiple matching editors are connected",
            Self::StaleConnection => "editor connection is no longer current",
            Self::AlreadyConnected => "editor instance is already connected",
            Self::EngineMismatch => "project and connection use different engines",
            Self::GenerationExhausted => "editor connection generation is exhausted",
        };
        f.write_str(message)
    }
}

impl std::error::Error for RegistryError {}

#[derive(Default)]
pub struct EngineRegistry {
    connections: HashMap<(EngineKind, String), ConnectionEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionToken {
    engine: EngineKind,
    instance_id: String,
    generation: u64,
}

impl ConnectionToken {
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorSelection {
    token: ConnectionToken,
    project: ProjectIdentity,
    require_interactive: bool,
}

impl EditorSelection {
    pub fn project(&self) -> &ProjectIdentity {
        &self.project
    }

    pub fn instance_id(&self) -> &str {
        &self.token.instance_id
    }

    pub fn generation(&self) -> u64 {
        self.token.generation
    }
}

struct ConnectionEntry {
    token: ConnectionToken,
    session: Option<EditorSession>,
}

impl EngineRegistry {
    /// Turn an earlier candidate into a selection only if that exact token,
    /// project and interaction state are still current.
    pub fn select_exact(
        &self,
        token: &ConnectionToken,
        project: &ProjectIdentity,
        require_interactive: bool,
    ) -> Result<EditorSelection, RegistryError> {
        let key = (token.engine, token.instance_id.clone());
        let entry = self
            .connections
            .get(&key)
            .filter(|entry| entry.token == *token)
            .ok_or(RegistryError::StaleConnection)?;
        entry
            .session
            .as_ref()
            .filter(|session| {
                session.project == *project && (!require_interactive || session.is_interactive())
            })
            .ok_or(RegistryError::NotFound)?;
        Ok(EditorSelection {
            token: token.clone(),
            project: project.clone(),
            require_interactive,
        })
    }
    /// Reserve a locally identified connection. No project is selectable until
    /// the adapter identifies this exact connection using the returned token.
    pub fn open_connection(
        &mut self,
        engine: EngineKind,
        instance_id: impl Into<String>,
    ) -> Result<ConnectionToken, RegistryError> {
        let instance_id = instance_id.into();
        if instance_id.trim().is_empty() {
            return Err(RegistryError::InvalidInstanceId);
        }
        let key = (engine, instance_id.clone());
        if self.connections.contains_key(&key) {
            return Err(RegistryError::AlreadyConnected);
        }
        let mut current = NEXT_CONNECTION_GENERATION.load(Ordering::Relaxed);
        let generation = loop {
            let next = current
                .checked_add(1)
                .ok_or(RegistryError::GenerationExhausted)?;
            match NEXT_CONNECTION_GENERATION.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break next,
                Err(observed) => current = observed,
            }
        };
        let token = ConnectionToken {
            engine,
            instance_id,
            generation,
        };
        self.connections.insert(
            key,
            ConnectionEntry {
                token: token.clone(),
                session: None,
            },
        );
        Ok(token)
    }

    /// Identify a pending connection, or refresh the same project. A current
    /// token that announces a different project or engine is retired; its
    /// adapter must also close the underlying transport.
    pub fn identify(
        &mut self,
        token: &ConnectionToken,
        project: ProjectIdentity,
        interactive: bool,
    ) -> Result<(), RegistryError> {
        let mode = if interactive {
            InteractionMode::Interactive
        } else {
            InteractionMode::NonInteractive
        };
        self.identify_with_mode(token, project, mode)
    }

    /// Register an explicitly known interaction mode, including unknown.
    pub fn identify_with_mode(
        &mut self,
        token: &ConnectionToken,
        project: ProjectIdentity,
        mode: InteractionMode,
    ) -> Result<(), RegistryError> {
        let key = (token.engine, token.instance_id.clone());
        if self.connections.get(&key).map(|entry| &entry.token) != Some(token) {
            return Err(RegistryError::StaleConnection);
        }
        if token.engine != project.engine {
            self.connections.remove(&key);
            return Err(RegistryError::EngineMismatch);
        }
        let entry = self
            .connections
            .get_mut(&key)
            .ok_or(RegistryError::StaleConnection)?;
        if entry
            .session
            .as_ref()
            .is_some_and(|session| session.project != project)
        {
            self.connections.remove(&key);
            return Err(RegistryError::InstanceBelongsToAnotherProject);
        }
        entry.session = Some(EditorSession {
            project,
            instance_id: token.instance_id.clone(),
            generation: token.generation,
            interaction_mode: mode,
        });
        Ok(())
    }

    /// A stale disconnect cannot remove a replacement connection.
    pub fn close_connection(&mut self, token: &ConnectionToken) -> bool {
        let key = (token.engine, token.instance_id.clone());
        if self.connections.get(&key).map(|entry| &entry.token) != Some(token) {
            return false;
        }
        self.connections.remove(&key);
        true
    }

    /// Select within one explicit project. Implicit selection includes only
    /// interactive editors and rejects more than one candidate.
    pub fn select(
        &self,
        project: &ProjectIdentity,
        instance_id: Option<&str>,
    ) -> Result<EditorSelection, RegistryError> {
        let mut matches = self.connections.values().filter(|entry| {
            entry.session.as_ref().is_some_and(|session| {
                &session.project == project
                    && match instance_id {
                        Some(id) => session.instance_id == id,
                        None => session.is_interactive(),
                    }
            })
        });
        let entry = matches.next().ok_or(RegistryError::NotFound)?;
        if matches.next().is_some() {
            return Err(RegistryError::Ambiguous);
        }
        Ok(EditorSelection {
            token: entry.token.clone(),
            project: project.clone(),
            require_interactive: instance_id.is_none(),
        })
    }

    /// Revalidate an owned selection immediately before routing an operation.
    pub fn resolve_selection(
        &self,
        selection: &EditorSelection,
    ) -> Result<&EditorSession, RegistryError> {
        let token = &selection.token;
        let key = (token.engine, token.instance_id.clone());
        let entry = self
            .connections
            .get(&key)
            .filter(|entry| entry.token == *token)
            .ok_or(RegistryError::StaleConnection)?;
        entry
            .session
            .as_ref()
            .filter(|session| {
                session.project == selection.project
                    && (!selection.require_interactive || session.is_interactive())
            })
            .ok_or(RegistryError::StaleConnection)
    }
}

#[cfg(test)]
mod tests {
    use super::{EngineKind, EngineRegistry};

    #[test]
    fn disconnected_instance_churn_does_not_accumulate_historical_entries() {
        let mut registry = EngineRegistry::default();
        for index in 0..10_000 {
            let token = registry
                .open_connection(EngineKind::Godot, format!("editor-{index}"))
                .unwrap();
            assert!(registry.close_connection(&token));
        }
        assert!(registry.connections.is_empty());
        assert!(registry.connections.capacity() < 128);
    }
}
