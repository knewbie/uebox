use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use uebox_agent_core::{
    Engine, Error, ReadOnlyTool, RunTarget, ToolDefinition, ToolError, ToolFuture, ToolRegistry,
};
use uebox_engine_core::{
    ConnectionToken, EditorSelection, EngineKind, EngineRegistry, InteractionMode, ProjectIdentity,
};
use uebox_engine_unreal::{InstanceSummary, SelectedInstance, UnrealBridge};

static NEXT_PUBLIC_INSTANCE: AtomicU64 = AtomicU64::new(0);

// Peer paths are data, not filesystem access instructions. Compare only
// explicit spellings of the project identity that the host has already
// approved. In particular, never canonicalize a peer-provided path.
fn project_path_matches(project: &ProjectIdentity, peer: &str) -> bool {
    let Some(approved) = project.root().to_str() else {
        return false;
    };
    #[cfg(windows)]
    {
        windows_path_matches(approved, peer)
    }
    #[cfg(not(windows))]
    {
        unix_path_matches(approved, peer)
    }
}

#[cfg(any(not(windows), test))]
fn unix_path_matches(approved: &str, peer: &str) -> bool {
    if !approved.starts_with('/') || peer.len() > approved.len().saturating_add(1) {
        return false;
    }
    if approved == "/" {
        return peer == "/";
    }
    peer == approved || peer.strip_suffix('/') == Some(approved)
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowsPathKind {
    Drive,
    Unc,
}

#[cfg(any(windows, test))]
struct WindowsPath<'a> {
    kind: WindowsPathKind,
    verbatim: bool,
    body: &'a str,
}

#[cfg(any(windows, test))]
fn windows_path_parts(path: &str) -> Option<WindowsPath<'_>> {
    fn is_separator(byte: u8) -> bool {
        matches!(byte, b'\\' | b'/')
    }
    fn is_absolute_drive(path: &str) -> bool {
        let bytes = path.as_bytes();
        bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && is_separator(bytes[2])
    }
    fn is_valid_unc(path: &str) -> bool {
        let mut components = path.split(['\\', '/']);
        let server = components.next().unwrap_or("");
        let share = components.next().unwrap_or("");
        !server.is_empty()
            && !share.is_empty()
            && !matches!(server, "." | ".." | "?")
            && !matches!(share, "." | "..")
    }

    let (kind, verbatim, body) = if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        (WindowsPathKind::Unc, true, rest)
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        (WindowsPathKind::Drive, true, rest)
    } else if let Some(rest) = path.strip_prefix(r"\\").or_else(|| path.strip_prefix("//")) {
        (WindowsPathKind::Unc, false, rest)
    } else {
        (WindowsPathKind::Drive, false, path)
    };
    let valid_body = match kind {
        WindowsPathKind::Drive => is_absolute_drive(body),
        WindowsPathKind::Unc => is_valid_unc(body),
    };
    (valid_body && (!verbatim || !body.contains('/'))).then_some(WindowsPath {
        kind,
        verbatim,
        body,
    })
}

#[cfg(any(windows, test))]
fn windows_component_can_cross_namespace(component: &str) -> bool {
    if component.starts_with(' ')
        || component.ends_with(['.', ' '])
        || component
            .bytes()
            .any(|byte| byte < b' ' || b"<>:\"|?*".contains(&byte))
    {
        return false;
    }

    // Win32 can recognize a DOS device even with an extension or spaces before it.
    let stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem)
        .trim_end_matches(' ');
    if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "CLOCK$"]
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
    {
        return false;
    }
    !stem
        .strip_suffix(|character: char| {
            character.is_ascii_digit() || matches!(character, '¹' | '²' | '³')
        })
        .is_some_and(|prefix| {
            prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT")
        })
}

#[cfg(any(windows, test))]
fn windows_path_matches(approved: &str, peer: &str) -> bool {
    // An extended UNC prefix adds at most six bytes, plus one trailing slash.
    // Reject huge peer strings before scanning their contents.
    if peer.len() > approved.len().saturating_add(7) {
        return false;
    }
    let Some(approved_path) = windows_path_parts(approved) else {
        return false;
    };
    let Some(peer_path) = windows_path_parts(peer) else {
        return false;
    };
    if approved_path.kind != peer_path.kind {
        return false;
    }
    fn strip_trailing(path: &str) -> &str {
        path.strip_suffix('\\')
            .or_else(|| path.strip_suffix('/'))
            .unwrap_or(path)
    }
    let approved = strip_trailing(approved_path.body);
    let peer = strip_trailing(peer_path.body);
    if approved.len() != peer.len()
        || approved
            .split(['\\', '/'])
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || peer
            .split(['\\', '/'])
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return false;
    }
    let drive = approved_path.kind == WindowsPathKind::Drive;
    // The byte comparison below requires identical components, so only the
    // approved spelling needs this check. Start after the optional drive designator.
    if approved_path.verbatim != peer_path.verbatim
        && !approved
            .split(['\\', '/'])
            .skip(usize::from(drive))
            .all(windows_component_can_cross_namespace)
    {
        return false;
    }
    approved
        .bytes()
        .zip(peer.bytes())
        .enumerate()
        .all(|(index, (left, right))| {
            if drive && index == 0 {
                left.eq_ignore_ascii_case(&right)
            } else {
                let normalize = |byte| if byte == b'/' { b'\\' } else { byte };
                normalize(left) == normalize(right)
            }
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindError {
    Unavailable,
    WrongEngine,
    InvalidDeadline,
    ProjectMismatch,
    InteractionMismatch,
}

struct Candidate {
    bridge: Arc<UnrealBridge>,
    registry: Arc<Mutex<EngineRegistry>>,
    project: ProjectIdentity,
    summary: InstanceSummary,
    token: ConnectionToken,
    public_instance: String,
}

/// A backend-owned candidate displayed to the user. The UE selection credential
/// is private; the agent sees only a separate process-local instance alias.
#[derive(Clone)]
pub struct UnrealCandidate {
    inner: Arc<Candidate>,
}

impl UnrealCandidate {
    pub fn capture(
        bridge: Arc<UnrealBridge>,
        registry: Arc<Mutex<EngineRegistry>>,
        project: ProjectIdentity,
        summary: &InstanceSummary,
        mode: InteractionMode,
    ) -> Result<Self, BindError> {
        if project.engine() != EngineKind::Unreal {
            return Err(BindError::WrongEngine);
        }
        if matches!(
            (mode, summary.interactive),
            (InteractionMode::Interactive, false) | (InteractionMode::NonInteractive, true)
        ) {
            return Err(BindError::InteractionMismatch);
        }
        if !bridge.instances().iter().any(|entry| entry == summary) {
            return Err(BindError::Unavailable);
        }
        if !project_path_matches(&project, &summary.project_path) {
            return Err(BindError::ProjectMismatch);
        }
        let mut previous = NEXT_PUBLIC_INSTANCE.load(Ordering::Relaxed);
        let sequence = loop {
            let next = previous.checked_add(1).ok_or(BindError::Unavailable)?;
            match NEXT_PUBLIC_INSTANCE.compare_exchange_weak(
                previous,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break next,
                Err(observed) => previous = observed,
            }
        };
        let public_instance = format!("ue-instance-{sequence}");
        let token = {
            let mut guard = registry.lock().map_err(|_| BindError::Unavailable)?;
            let token = guard
                .open_connection(EngineKind::Unreal, &public_instance)
                .map_err(|_| BindError::Unavailable)?;
            if guard
                .identify_with_mode(&token, project.clone(), mode)
                .is_err()
            {
                guard.close_connection(&token);
                return Err(BindError::Unavailable);
            }
            token
        };
        if !bridge.instances().iter().any(|entry| entry == summary) {
            if let Ok(mut guard) = registry.lock() {
                guard.close_connection(&token);
            }
            return Err(BindError::Unavailable);
        }
        Ok(Self {
            inner: Arc::new(Candidate {
                bridge,
                registry,
                project,
                summary: summary.clone(),
                token,
                public_instance,
            }),
        })
    }

    pub fn token(&self) -> &ConnectionToken {
        &self.inner.token
    }
    pub fn public_instance_id(&self) -> &str {
        &self.inner.public_instance
    }

    /// The host retires this token when the UE candidate disappears or changes.
    pub fn retire(&self) -> bool {
        self.inner
            .registry
            .lock()
            .map(|mut registry| registry.close_connection(&self.inner.token))
            .unwrap_or(false)
    }
}

struct Binding {
    candidate: Arc<Candidate>,
    target: RunTarget,
    selected: SelectedInstance,
    deadline: Duration,
    selection: EditorSelection,
}

impl Binding {
    fn current(&self, target: &RunTarget) -> bool {
        if target != &self.target
            || target.engine() != Engine::Unreal
            || self.selection.project().engine() != EngineKind::Unreal
            || self.selection.project().root().to_str() != Some(target.project())
            || self.selection.instance_id() != target.instance()
            || self.selection.generation() != target.generation()
        {
            return false;
        }
        self.candidate
            .registry
            .lock()
            .ok()
            .and_then(|registry| {
                registry
                    .resolve_selection(&self.selection)
                    .ok()
                    .map(|editor| {
                        editor.project() == self.selection.project()
                            && editor.instance_id() == target.instance()
                            && editor.generation() == target.generation()
                    })
            })
            .unwrap_or(false)
    }
}

#[derive(Clone)]
pub struct UnrealProjectInfoBinding {
    inner: Arc<Binding>,
}

impl UnrealProjectInfoBinding {
    /// Bind the exact candidate the user selected, retaining its original UE handle.
    pub fn bind_exact(
        candidate: &UnrealCandidate,
        token: &ConnectionToken,
        deadline: Duration,
    ) -> Result<Self, BindError> {
        if deadline.is_zero() || Instant::now().checked_add(deadline).is_none() {
            return Err(BindError::InvalidDeadline);
        }
        let candidate = candidate.inner.clone();
        if token != &candidate.token {
            return Err(BindError::Unavailable);
        }
        let selection = candidate
            .registry
            .lock()
            .map_err(|_| BindError::Unavailable)?
            .select_exact(token, &candidate.project, false)
            .map_err(|_| BindError::Unavailable)?;
        if selection.instance_id() != candidate.public_instance {
            return Err(BindError::Unavailable);
        }
        let selected = candidate
            .bridge
            .select_instance(&candidate.summary.connection_id)
            .map_err(|_| BindError::Unavailable)?;
        if !candidate.bridge.instances().iter().any(|entry| {
            entry.connection_id == candidate.summary.connection_id
                && entry.project_name == candidate.summary.project_name
                && entry.project_path == candidate.summary.project_path
        }) {
            return Err(BindError::Unavailable);
        }
        let target = RunTarget::new(
            Engine::Unreal,
            candidate
                .project
                .root()
                .to_str()
                .ok_or(BindError::Unavailable)?,
            selection.instance_id(),
            selection.generation(),
        )
        .map_err(|_| BindError::Unavailable)?;
        let inner = Arc::new(Binding {
            candidate,
            target,
            selected,
            deadline,
            selection,
        });
        if !inner.current(&inner.target) {
            return Err(BindError::Unavailable);
        }
        Ok(Self { inner })
    }

    pub fn target(&self) -> &RunTarget {
        &self.inner.target
    }
    pub fn tools(&self) -> Result<ToolRegistry, Error> {
        if !self.inner.current(&self.inner.target) {
            return Err(Error::TargetChanged);
        }
        let mut tools = ToolRegistry::new(self.inner.target.clone());
        tools.register(
            Engine::Unreal,
            ToolDefinition::new(
                "ue_project_info",
                "Read project information from the selected Unreal editor.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            self.tool(),
        )?;
        Ok(tools)
    }
    pub fn tool(&self) -> Arc<dyn ReadOnlyTool> {
        Arc::new(ProjectInfoTool {
            inner: self.inner.clone(),
        })
    }
    pub fn target_validator(&self) -> Arc<dyn Fn(&RunTarget) -> bool + Send + Sync> {
        let inner = self.inner.clone();
        Arc::new(move |target| inner.current(target))
    }
}

struct ProjectInfoTool {
    inner: Arc<Binding>,
}
impl ReadOnlyTool for ProjectInfoTool {
    fn execute(&self, target: RunTarget, arguments: Value) -> ToolFuture {
        let inner = self.inner.clone();
        Box::pin(async move {
            if !inner.current(&target) || arguments != json!({}) {
                return Err(ToolError);
            }
            let result = inner
                .selected
                .get_project_info(inner.deadline)
                .await
                .map_err(|_| ToolError)?;
            if !inner.current(&target) {
                return Err(ToolError);
            }
            if result.get("projectName").and_then(Value::as_str)
                != Some(inner.candidate.summary.project_name.as_str())
            {
                return Err(ToolError);
            }
            let path = result
                .get("projectPath")
                .and_then(Value::as_str)
                .ok_or(ToolError)?;
            if !project_path_matches(inner.selection.project(), path) || !inner.current(&target) {
                return Err(ToolError);
            }
            Ok(result)
        })
    }
}

#[cfg(test)]
mod path_tests {
    use super::{unix_path_matches, windows_path_matches};

    #[test]
    fn unix_paths_only_accept_the_authorized_spelling_and_a_trailing_separator() {
        let approved = "/projects/Game";
        for path in [approved, "/projects/Game/"] {
            assert!(unix_path_matches(approved, path), "{path:?}");
        }
        for path in [
            "/projects/Other",
            "/projects/Game/../Game",
            "/projects/Game/.",
            "//projects/Game",
            "projects/Game",
            "",
        ] {
            assert!(!unix_path_matches(approved, path), "{path:?}");
        }
        assert!(unix_path_matches("/", "/"));
        assert!(!unix_path_matches("/", "//"));
    }

    #[test]
    fn windows_local_paths_reject_unc_device_aliases_and_other_projects_without_io() {
        let approved = r"\\?\C:\Projects\Game";
        for path in [
            r"\\?\C:\Projects\Game",
            r"C:\Projects\Game",
            "c:/Projects/Game/",
            r"\\?\C:\Projects\Game\",
        ] {
            assert!(windows_path_matches(approved, path), "{path:?}");
        }
        for path in [
            r"\\other.invalid\share\Game",
            r"\\?\UNC\other.invalid\share\Game",
            r"\\.\C:\Projects\Game",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\Projects\Game",
            r"\\?\C:/Projects/Game",
            r"C:\Projects\Game\..\Game",
            r"C:\Projects\Other",
            r"C:\projects\Game",
            r"C:Projects\Game",
            r"\Projects\Game",
        ] {
            assert!(!windows_path_matches(approved, path), "{path:?}");
        }
        assert!(windows_path_matches(r"\\?\C:\", r"C:\"));
        assert!(!windows_path_matches(r"\\?\C:\", r"C:"));
    }

    #[test]
    fn windows_unc_paths_match_only_the_previously_authorized_share_and_project() {
        let approved = r"\\?\UNC\host\share\Game";
        for path in [approved, r"\\host\share\Game", "//host/share/Game/"] {
            assert!(windows_path_matches(approved, path), "{path:?}");
        }
        for path in [
            r"\\other.invalid\share\Game",
            r"\\host\other-share\Game",
            r"\\host\share\Other",
            r"\\HOST\share\Game",
            r"\\.\host\share\Game",
            r"\\?\GLOBALROOT\Device\host\share\Game",
            r"\\?\UNC\host/share/Game",
        ] {
            assert!(!windows_path_matches(approved, path), "{path:?}");
        }
    }

    #[test]
    fn windows_cross_namespace_rejects_trailing_dots_and_spaces_in_any_component() {
        for (verbatim, ordinary) in [
            (r"\\?\C:\Projects\Game.", r"C:\Projects\Game."),
            (r"\\?\C:\Projects\Game ", r"C:\Projects\Game "),
            (r"\\?\C:\Projects.\Game", r"C:\Projects.\Game"),
            (r"\\?\C:\ Projects\Game", r"C:\ Projects\Game"),
            (
                r"\\?\UNC\fixture.invalid\share\Game.",
                r"\\fixture.invalid\share\Game.",
            ),
            (
                r"\\?\UNC\fixture.invalid\share \Game",
                r"\\fixture.invalid\share \Game",
            ),
        ] {
            assert!(!windows_path_matches(verbatim, ordinary), "{verbatim:?}");
            assert!(!windows_path_matches(ordinary, verbatim), "{ordinary:?}");
        }
    }

    #[test]
    fn windows_cross_namespace_rejects_dos_devices_even_with_extensions() {
        for (verbatim, ordinary) in [
            (r"\\?\C:\Projects\NUL", r"C:\Projects\NUL"),
            (r"\\?\C:\Projects\nul.txt", r"C:\Projects\nul.txt"),
            (r"\\?\C:\Projects\Aux .txt", r"C:\Projects\Aux .txt"),
            (r"\\?\C:\COM1\Game", r"C:\COM1\Game"),
            (r"\\?\C:\COM0\Game", r"C:\COM0\Game"),
            (r"\\?\C:\COM¹\Game", r"C:\COM¹\Game"),
            (r"\\?\C:\LPT².log\Game", r"C:\LPT².log\Game"),
            (r"\\?\C:\CONIN$\Game", r"C:\CONIN$\Game"),
            (r"\\?\C:\CONOUT$\Game", r"C:\CONOUT$\Game"),
            (r"\\?\C:\CLOCK$\Game", r"C:\CLOCK$\Game"),
            (
                r"\\?\UNC\fixture.invalid\share\NUL\Game",
                r"\\fixture.invalid\share\NUL\Game",
            ),
        ] {
            assert!(!windows_path_matches(verbatim, ordinary), "{verbatim:?}");
            assert!(!windows_path_matches(ordinary, verbatim), "{ordinary:?}");
        }
    }

    #[test]
    fn windows_cross_namespace_rejects_ambiguous_win32_characters() {
        for (verbatim, ordinary) in [
            (r"\\?\C:\Projects\Game:stream", r"C:\Projects\Game:stream"),
            (r"\\?\C:\Projects\Game?", r"C:\Projects\Game?"),
            (r"\\?\C:\Projects\Game*", r"C:\Projects\Game*"),
            (r"\\?\C:\Projects\Game|x", r"C:\Projects\Game|x"),
            (
                "\\\\?\\C:\\Projects\\Game\u{1}x",
                "C:\\Projects\\Game\u{1}x",
            ),
        ] {
            assert!(!windows_path_matches(verbatim, ordinary), "{verbatim:?}");
            assert!(!windows_path_matches(ordinary, verbatim), "{ordinary:?}");
        }
    }

    #[test]
    fn windows_same_verbatim_special_names_and_safe_cross_namespace_names_still_match() {
        for path in [
            r"\\?\C:\Projects\Game.",
            r"\\?\C:\Projects\Game ",
            r"\\?\C:\Projects\NUL.txt",
            r"\\?\UNC\fixture.invalid\share\Game.",
            r"\\?\UNC\fixture.invalid\share\COM¹",
        ] {
            assert!(windows_path_matches(path, path), "{path:?}");
        }
        for (verbatim, ordinary) in [
            (r"\\?\C:\Projects\Game", "c:/Projects/Game/"),
            (r"\\?\C:\Projects\.temp", r"C:\Projects\.temp"),
            (r"\\?\C:\Projects\COM10", r"C:\Projects\COM10"),
            (
                r"\\?\UNC\fixture.invalid\share\Game",
                r"\\fixture.invalid\share\Game",
            ),
        ] {
            assert!(windows_path_matches(verbatim, ordinary), "{verbatim:?}");
            assert!(windows_path_matches(ordinary, verbatim), "{ordinary:?}");
        }
    }
}
