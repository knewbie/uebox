use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::Runtime;
use uebox_godot_session::{
    GodotSession, LaunchOptions, ProjectInfo, SceneTree, SessionError, SessionState,
};

pub(crate) type SessionHandle = Arc<Mutex<Option<GodotSession>>>;
type SessionMap = Arc<Mutex<HashMap<String, OwnedSession>>>;

struct OwnedSession {
    owner: String,
    handle: SessionHandle,
}

#[derive(Clone, Default)]
pub(crate) struct GodotSessions {
    sessions: SessionMap,
    pub(crate) agent: Arc<crate::godot_agent::AgentHost>,
    // The real-editor test uses an isolated home and headless mode. Neither is a UI argument.
    #[cfg(test)]
    test_home: Option<std::path::PathBuf>,
}

impl GodotSessions {
    pub(crate) fn destroy_window(&self, owner: &str) {
        let removed = {
            let mut sessions = self
                .sessions
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let ids: Vec<_> = sessions
                .iter()
                .filter(|(_, entry)| entry.owner == owner)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| sessions.remove(&id))
                .collect::<Vec<_>>()
        };
        self.agent.destroy_window(owner);
        tauri::async_runtime::spawn_blocking(move || drop(removed));
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LaunchedSession {
    session_id: String,
    state: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectSnapshot {
    name: String,
    godot_version: String,
}

impl From<ProjectInfo> for ProjectSnapshot {
    fn from(value: ProjectInfo) -> Self {
        Self {
            name: value.name,
            godot_version: value.godot_version,
        }
    }
}

#[derive(Serialize)]
struct SceneNodeSnapshot {
    path: String,
    name: String,
    class: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SceneSnapshot {
    scene_path: Option<String>,
    nodes: Vec<SceneNodeSnapshot>,
    truncated: bool,
}

impl From<SceneTree> for SceneSnapshot {
    fn from(value: SceneTree) -> Self {
        Self {
            scene_path: value.scene_path,
            nodes: value
                .nodes
                .into_iter()
                .map(|node| SceneNodeSnapshot {
                    path: node.path,
                    name: node.name,
                    class: node.class,
                })
                .collect(),
            truncated: value.truncated,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExitSnapshot {
    success: bool,
    code: Option<i32>,
    signal: Option<i32>,
}

#[derive(Serialize)]
pub(crate) struct StatusSnapshot {
    state: &'static str,
    exit: Option<ExitSnapshot>,
}

impl From<SessionState> for StatusSnapshot {
    fn from(value: SessionState) -> Self {
        let (state, exit) = match value {
            SessionState::Starting => ("starting", None),
            SessionState::Ready => ("ready", None),
            SessionState::Disconnected => ("disconnected", None),
            SessionState::StartupExpired => ("startup_expired", None),
            SessionState::Exited(info) => (
                "exited",
                Some(ExitSnapshot {
                    success: info.success,
                    code: info.code,
                    signal: info.signal,
                }),
            ),
        };
        Self { state, exit }
    }
}

fn new_session_id() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|_| "Unable to create a Godot session ID".to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn lookup(sessions: &SessionMap, id: &str, owner: &str) -> Result<SessionHandle, String> {
    sessions
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(id)
        .filter(|entry| entry.owner == owner)
        .map(|entry| Arc::clone(&entry.handle))
        .ok_or_else(|| "Unknown Godot session".to_string())
}

async fn query<T: Send + 'static>(
    sessions: tauri::State<'_, GodotSessions>,
    id: String,
    owner: String,
    operation: impl FnOnce(&mut GodotSession) -> Result<T, SessionError> + Send + 'static,
) -> Result<T, String> {
    let agent = Arc::clone(&sessions.agent);
    let sessions = Arc::clone(&sessions.sessions);
    tauri::async_runtime::spawn_blocking(move || {
        let handle = lookup(&sessions, &id, &owner)?;
        let mut guard = handle.lock().unwrap_or_else(|error| error.into_inner());
        let session = guard
            .as_mut()
            .ok_or_else(|| "Unknown Godot session".to_string())?;
        let result = operation(session).map_err(|error| error.to_string());
        let terminal = !matches!(
            session.status(),
            Ok(SessionState::Starting | SessionState::Ready)
        );
        drop(guard);
        if terminal {
            agent.retire(&id);
        }
        result
    })
    .await
    .map_err(|error| format!("Godot worker failed: {error}"))?
}

#[tauri::command]
pub(crate) async fn launch_godot<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    executable: String,
    project: String,
    sessions: tauri::State<'_, GodotSessions>,
) -> Result<LaunchedSession, String> {
    let sessions_map = Arc::clone(&sessions.sessions);
    let owner = window.label().to_owned();
    #[cfg(test)]
    let test_home = sessions.test_home.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut id = new_session_id()?;
        let options = LaunchOptions::new(executable, project).acknowledge_legacy_protocol_risks();
        #[cfg(test)]
        let options = if let Some(home) = test_home {
            options.editor_mode(uebox_godot_session::EditorMode::Headless).isolate_editor_paths(home)
        } else { options };
        let session = GodotSession::launch(options).map_err(|error| error.to_string())?;
        let mut map = sessions_map.lock().unwrap_or_else(|error| error.into_inner());
        while map.contains_key(&id) {
            id = new_session_id().map_err(|_| "Godot editor started, but session registration failed; the editor was left running".to_string())?;
        }
        map.insert(id.clone(), OwnedSession { owner, handle: Arc::new(Mutex::new(Some(session))) });
        Ok(LaunchedSession { session_id: id, state: "starting" })
    }).await.map_err(|error| format!("Godot launch worker failed; the editor may have started and was not closed: {error}"))?
}

#[tauri::command]
pub(crate) async fn connect_godot<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    session_id: String,
    sessions: tauri::State<'_, GodotSessions>,
) -> Result<ProjectSnapshot, String> {
    let map = Arc::clone(&sessions.sessions);
    let agent = Arc::clone(&sessions.agent);
    let id = session_id.clone();
    let owner = window.label().to_owned();
    let (snapshot, project, instance) = query(sessions, session_id, owner.clone(), |session| {
        let snapshot = session.connect_once()?.into();
        Ok((
            snapshot,
            session.project_path().to_path_buf(),
            session.instance_id().to_owned(),
        ))
    })
    .await?;
    tauri::async_runtime::spawn_blocking(move || {
        let project =
            uebox_engine_core::ProjectIdentity::new(uebox_engine_core::EngineKind::Godot, project)
                .map_err(|_| "target_unavailable".to_string())?;
        let handle = lookup(&map, &id, &owner)?;
        {
            let mut guard = handle.lock().unwrap_or_else(|error| error.into_inner());
            if !guard
                .as_mut()
                .is_some_and(|session| matches!(session.status(), Ok(SessionState::Ready)))
            {
                return Err("target_unavailable".to_string());
            }
        }
        let guard = map.lock().unwrap_or_else(|error| error.into_inner());
        if !guard
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(&current.handle, &handle))
        {
            return Err("Unknown Godot session".to_string());
        }
        agent.register(&id, owner, handle, project, instance)?;
        Ok(snapshot)
    })
    .await
    .map_err(|_| "Godot registration worker failed".to_string())?
}

#[tauri::command]
pub(crate) async fn query_godot_project<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    session_id: String,
    sessions: tauri::State<'_, GodotSessions>,
) -> Result<ProjectSnapshot, String> {
    query(sessions, session_id, window.label().to_owned(), |session| {
        session.project_info().map(Into::into)
    })
    .await
}

#[tauri::command]
pub(crate) async fn query_godot_scene<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    session_id: String,
    sessions: tauri::State<'_, GodotSessions>,
) -> Result<SceneSnapshot, String> {
    query(sessions, session_id, window.label().to_owned(), |session| {
        session.scene_tree().map(Into::into)
    })
    .await
}

#[tauri::command]
pub(crate) async fn status_godot_session<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    session_id: String,
    sessions: tauri::State<'_, GodotSessions>,
) -> Result<StatusSnapshot, String> {
    query(sessions, session_id, window.label().to_owned(), |session| {
        session.status().map(Into::into)
    })
    .await
}

#[tauri::command]
pub(crate) async fn release_godot_session<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    session_id: String,
    sessions: tauri::State<'_, GodotSessions>,
) -> Result<(), String> {
    let handle = {
        let mut map = sessions
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !map
            .get(&session_id)
            .is_some_and(|entry| entry.owner == window.label())
        {
            return Err("Unknown Godot session".into());
        }
        map.remove(&session_id)
            .expect("entry checked under lock")
            .handle
    };
    sessions.agent.retire(&session_id);
    tauri::async_runtime::spawn_blocking(move || {
        let session = handle
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .ok_or_else(|| "Unknown Godot session".to_string())?;
        // GodotSession's drop releases management and reaps later; it never kills the editor.
        drop(session);
        Ok(())
    })
    .await
    .map_err(|error| format!("Godot release worker failed: {error}"))?
}

#[cfg(all(test, feature = "godot-real-editor-test", unix))]
#[path = "godot_real_test.rs"]
mod real_editor_test;
