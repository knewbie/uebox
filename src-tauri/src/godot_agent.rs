//! Fixed-target, read-only Godot Agent preview. Nothing here discovers editors.
use crate::chat_history::{finish_recorded_run, start_recorded_run};
use crate::godot::{GodotSessions, SessionHandle};
use crate::settings::{AgentModel, AgentModelSource};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tauri::{Manager, Runtime};
use uebox_agent_core::{
    AgentSession, Cancellation, Engine, Error, ModelEvent, ReadOnlyTool, RunTarget, ToolDefinition,
    ToolFuture, ToolRegistry,
};
use uebox_agent_godot_tools::GodotToolBinding;
use uebox_engine_core::{
    ConnectionToken, EditorSelection, EngineKind, EngineRegistry, ProjectIdentity,
};
use uebox_godot_session::SessionState;

const MAX_QUEUED_TEXT: usize = 1024 * 1024;

#[derive(Default)]
pub(crate) struct AgentHost {
    registry: Arc<Mutex<EngineRegistry>>,
    state: Mutex<HostState>,
}
#[derive(Default)]
struct HostState {
    targets: HashMap<String, Target>,
    agents: HashMap<String, Agent>,
}
#[derive(Clone)]
struct Target {
    token: ConnectionToken,
    selection: EditorSelection,
    handle: SessionHandle,
}
struct Agent {
    owner: String,
    target_id: String,
    session: Option<Arc<AgentSession>>,
    credential: Arc<Mutex<Option<String>>>,
    run: Option<ActiveRun>,
    retired: bool,
    closed: bool,
}

impl Agent {
    fn run_is_active(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.snapshot().run_id.is_some())
    }

    fn retain_after_retirement(&mut self) -> bool {
        if !self.retired || self.run_is_active() {
            return true;
        }
        // The terminal poll needs its receipt, but no longer needs the provider or history.
        self.session = None;
        !self.closed && self.run.is_some()
    }
}

struct ActiveRun {
    id: u64,
    cancellation: Cancellation,
    cancelled: bool,
    result: Option<Result<(), &'static str>>,
    fragments: Vec<String>,
    queued_bytes: usize,
    forced_error: Option<&'static str>,
}

fn queue_text(run: &mut ActiveRun, text: String) {
    if run.cancelled {
        return;
    }
    if run.queued_bytes.saturating_add(text.len()) > MAX_QUEUED_TEXT {
        run.cancelled = true;
        run.forced_error = Some("agent_output_buffer_limit");
        run.fragments.clear();
        run.queued_bytes = 0;
        run.cancellation.cancel();
    } else {
        run.queued_bytes += text.len();
        run.fragments.push(text);
    }
}

/// Observe terminal session state before a failed read can become a new model step.
struct TrackedTool {
    inner: Arc<dyn ReadOnlyTool>,
    handle: SessionHandle,
    host: Weak<AgentHost>,
    target_id: String,
}

impl ReadOnlyTool for TrackedTool {
    fn execute(&self, target: RunTarget, arguments: serde_json::Value) -> ToolFuture {
        let work = self.inner.execute(target, arguments);
        let handle = Arc::clone(&self.handle);
        let host = self.host.clone();
        let target_id = self.target_id.clone();
        Box::pin(async move {
            let result = work.await;
            // A contending status/read call can hold this mutex just after the tool
            // failed. Wait for the state so a terminal read cannot trigger another model step.
            let terminal = tokio::task::spawn_blocking(move || match handle.lock() {
                Ok(mut guard) => guard
                    .as_mut()
                    .is_none_or(|session| !matches!(session.status(), Ok(SessionState::Ready))),
                Err(_) => true,
            })
            .await
            .unwrap_or(true);
            if terminal {
                if let Some(host) = host.upgrade() {
                    host.retire(&target_id);
                }
            }
            result
        })
    }
}

impl AgentHost {
    /// Called only while the host session map is locked and the exact handle is still present.
    pub(crate) fn register(
        &self,
        id: &str,
        handle: SessionHandle,
        project: ProjectIdentity,
        instance: String,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.targets.contains_key(id) {
            return Err("target_already_registered".into());
        }
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let token = registry
            .open_connection(EngineKind::Godot, instance)
            .map_err(|_| "target_unavailable")?;
        let selection = match registry
            .identify(&token, project.clone(), false)
            .and_then(|_| registry.select_exact(&token, &project, false))
        {
            Ok(selection) => selection,
            Err(_) => {
                registry.close_connection(&token);
                return Err("target_unavailable".into());
            }
        };
        state.targets.insert(
            id.to_owned(),
            Target {
                token,
                selection,
                handle,
            },
        );
        Ok(())
    }

    /// Retire before any caller waits for the underlying session mutex or editor IO.
    pub(crate) fn retire(&self, target_id: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(target) = state.targets.remove(target_id) {
            self.registry
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .close_connection(&target.token);
        }
        for agent in state
            .agents
            .values_mut()
            .filter(|agent| agent.target_id == target_id)
        {
            agent.retired = true;
            agent
                .credential
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(run) = &mut agent.run {
                run.cancelled = true;
                run.fragments.clear();
                run.queued_bytes = 0;
                run.cancellation.cancel();
            }
        }
        state
            .agents
            .retain(|_, agent| agent.retain_after_retirement());
    }

    fn close_agent(&self, id: &str, owner: &str) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let agent = state
            .agents
            .get_mut(id)
            .filter(|agent| agent.owner == owner && !agent.closed)
            .ok_or("agent_unavailable")?;
        agent.retired = true;
        agent.closed = true;
        agent
            .credential
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(run) = &mut agent.run {
            run.cancelled = true;
            run.fragments.clear();
            run.cancellation.cancel();
        }
        state
            .agents
            .retain(|_, agent| agent.retain_after_retirement());
        Ok(())
    }
}

fn error_code(error: Error) -> &'static str {
    match error {
        Error::InvalidConfig => "invalid_model_configuration",
        Error::Network => "model_network_error",
        Error::HttpStatus => "model_http_error",
        Error::Protocol => "model_protocol_error",
        Error::OutputLimit => "model_output_limit",
        Error::Provider => "model_provider_error",
        Error::Cancelled => "cancelled",
        Error::Busy => "agent_busy",
        Error::TargetChanged => "target_unavailable",
        Error::StepLimit => "agent_step_limit",
        Error::ContextLimit => "agent_context_limit",
        Error::CredentialUnavailable => "credential_unavailable",
        _ => "agent_error",
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreatedAgent {
    agent_id: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StartedRun {
    agent_id: String,
    run_id: u64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunUpdate {
    agent_id: String,
    run_id: u64,
    phase: &'static str,
    fragments: Vec<String>,
    error: Option<&'static str>,
}

fn id() -> Result<String, String> {
    let mut bytes = [0; 16];
    getrandom::getrandom(&mut bytes).map_err(|_| "agent_unavailable")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn agent_mut<'a>(
    state: &'a mut HostState,
    agent_id: &str,
    owner: &str,
) -> Result<&'a mut Agent, String> {
    state
        .agents
        .get_mut(agent_id)
        .filter(|agent| agent.owner == owner && !agent.retired)
        .ok_or_else(|| "agent_unavailable".to_string())
}

#[tauri::command]
pub(crate) async fn create_godot_agent<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    session_id: String,
    endpoint: String,
    model: String,
    credential: Option<String>,
) -> Result<CreatedAgent, String> {
    let source = AgentModelSource::Direct {
        endpoint,
        model,
        credential,
    };
    create_godot_with_model(window, session_id, source).await
}

#[tauri::command]
pub(crate) async fn create_godot_agent_from_profile<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    session_id: String,
    profile_id: Option<String>,
) -> Result<CreatedAgent, String> {
    let source = AgentModelSource::Saved { profile_id };
    create_godot_with_model(window, session_id, source).await
}

async fn create_godot_with_model<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    session_id: String,
    source: AgentModelSource,
) -> Result<CreatedAgent, String> {
    let owner = window.label().to_owned();
    let host = Arc::clone(&window.state::<GodotSessions>().agent);
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let target = {
            let state = host.state.lock().unwrap_or_else(|e| e.into_inner());
            state.targets.get(&session_id).cloned().ok_or("target_unavailable")?
        };
        let binding = match GodotToolBinding::new(Arc::clone(&host.registry), target.selection.clone(), Arc::clone(&target.handle)) {
            Ok(binding) => binding,
            Err(_) => {
                let terminal = match target.handle.try_lock() {
                    Ok(mut guard) => guard.as_mut().is_none_or(|session| !matches!(session.status(), Ok(SessionState::Ready))),
                    Err(std::sync::TryLockError::WouldBlock) => false,
                    Err(std::sync::TryLockError::Poisoned(_)) => true,
                };
                if terminal { host.retire(&session_id); }
                return Err("target_unavailable".into());
            }
        };
        let AgentModel { provider, credential } = source.resolve_in_worker(&app, error_code)?;
        let mut registry = ToolRegistry::new(binding.target());
        let schema = serde_json::json!({"type":"object","properties":{},"additionalProperties":false});
        for (name, description, inner) in [
            ("godot_project_info", "Read the selected Godot editor project", binding.project_info_tool()),
            ("godot_scene_tree", "Read the selected Godot editor's current scene tree", binding.scene_tree_tool()),
        ] {
            registry.register(Engine::Godot, ToolDefinition::new(name, description, schema.clone()), Arc::new(TrackedTool {
                inner,
                handle: Arc::clone(&target.handle),
                host: Arc::downgrade(&host),
                target_id: session_id.clone(),
            })).map_err(error_code)?;
        }
        let session = AgentSession::new_with_system_prompt(provider, registry, binding.validator(), 6,
            "You are a read-only assistant for the selected Godot editor. Use only the supplied read-only tools. Do not claim to change the project.")
            .map_err(error_code)?;
        let mut state = host.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.targets.get(&session_id).is_some_and(|current| current.token == target.token) {
            return Err("target_unavailable".into());
        }
        if state.agents.values().any(|agent| agent.target_id == session_id) { return Err("agent_already_exists".into()); }
        let mut agent_id = id()?;
        while state.agents.contains_key(&agent_id) { agent_id = id()?; }
        state.agents.insert(agent_id.clone(), Agent { owner, target_id: session_id, session: Some(Arc::new(session)), credential, run: None, retired: false, closed: false });
        Ok(CreatedAgent { agent_id })
    }).await.map_err(|_| "agent_worker_error".to_string())?
}

#[tauri::command]
pub(crate) async fn start_godot_agent<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    agent_id: String,
    text: String,
    save_history: Option<bool>,
    sessions: tauri::State<'_, GodotSessions>,
) -> Result<StartedRun, String> {
    if text.trim().is_empty() || text.len() > 64 * 1024 {
        return Err("invalid_message".into());
    }
    let owner = window.label().to_owned();
    let host = Arc::clone(&sessions.agent);
    let (target_id, handle) = {
        let mut state = host.state.lock().unwrap_or_else(|e| e.into_inner());
        let target_id = agent_mut(&mut state, &agent_id, &owner)?.target_id.clone();
        let handle = state
            .targets
            .get(&target_id)
            .ok_or("target_unavailable")?
            .handle
            .clone();
        (target_id, handle)
    };
    let check_handle = Arc::clone(&handle);
    let status = tauri::async_runtime::spawn_blocking(move || match check_handle.try_lock() {
        Ok(mut guard) => {
            if guard
                .as_mut()
                .is_some_and(|session| matches!(session.status(), Ok(SessionState::Ready)))
            {
                "ready"
            } else {
                "terminal"
            }
        }
        Err(std::sync::TryLockError::Poisoned(_)) => "terminal",
        Err(std::sync::TryLockError::WouldBlock) => "busy",
    })
    .await
    .map_err(|_| "agent_worker_error")?;
    if status != "ready" {
        if status == "terminal" {
            host.retire(&target_id);
        }
        return Err(if status == "busy" {
            "agent_busy"
        } else {
            "target_unavailable"
        }
        .into());
    }
    let (run, recording, target_id, handle) = {
        let mut state = host.state.lock().unwrap_or_else(|e| e.into_inner());
        let target_id = agent_mut(&mut state, &agent_id, &owner)?.target_id.clone();
        let handle = state
            .targets
            .get(&target_id)
            .ok_or("target_unavailable")?
            .handle
            .clone();
        let agent = agent_mut(&mut state, &agent_id, &owner)?;
        if agent.run.as_ref().is_some_and(|run| run.result.is_none()) {
            return Err("agent_busy".into());
        }
        let session = agent.session.as_ref().ok_or("agent_unavailable")?;
        let (run, recording) = start_recorded_run(
            window.app_handle(),
            session,
            &agent_id,
            uebox_chat_history::Engine::Godot,
            text,
            save_history.unwrap_or(false),
            error_code,
        )?;
        agent.run = Some(ActiveRun {
            id: run.id(),
            cancellation: run.cancellation(),
            cancelled: false,
            result: None,
            fragments: Vec::new(),
            queued_bytes: 0,
            forced_error: None,
        });
        (run, recording, target_id, handle)
    };
    let run_id = run.id();
    let task_host = Arc::clone(&host);
    let task_agent_id = agent_id.clone();
    tauri::async_runtime::spawn(async move {
        let callback_host = Arc::clone(&task_host);
        let callback_id = task_agent_id.clone();
        let outcome = run
            .execute(move |event| {
                let ModelEvent::TextDelta(text) = event;
                let mut state = callback_host
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(run) = state
                    .agents
                    .get_mut(&callback_id)
                    .and_then(|agent| agent.run.as_mut())
                    .filter(|run| run.id == run_id && !run.cancelled)
                {
                    queue_text(run, text);
                }
            })
            .await;
        let result = finish_recorded_run(recording, outcome, error_code).await;
        let mut state = task_host.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(run) = state
            .agents
            .get_mut(&task_agent_id)
            .and_then(|agent| agent.run.as_mut())
            .filter(|run| run.id == run_id)
        {
            run.result = Some(result);
        }
    });
    let monitor_host = Arc::clone(&host);
    let monitor_id = agent_id.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let retired = {
                let state = monitor_host.state.lock().unwrap_or_else(|e| e.into_inner());
                state
                    .agents
                    .get(&monitor_id)
                    .filter(|agent| {
                        agent.run.as_ref().is_some_and(|run| run.id == run_id)
                            && agent.run_is_active()
                    })
                    .map(|agent| agent.retired)
            };
            match retired {
                None => break,
                Some(true) => continue, // Keep ownership until an admitted tool finishes.
                Some(false) => {}
            }
            let handle = Arc::clone(&handle);
            let terminal = tauri::async_runtime::spawn_blocking(move || match handle.try_lock() {
                Ok(mut guard) => guard
                    .as_mut()
                    .is_none_or(|session| !matches!(session.status(), Ok(SessionState::Ready))),
                Err(std::sync::TryLockError::WouldBlock) => false,
                Err(std::sync::TryLockError::Poisoned(_)) => true,
            })
            .await
            .unwrap_or(true);
            if terminal {
                monitor_host.retire(&target_id);
            }
        }
        let mut state = monitor_host.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .agents
            .retain(|_, agent| agent.retain_after_retirement());
    });
    Ok(StartedRun { agent_id, run_id })
}

#[tauri::command]
pub(crate) fn poll_godot_agent<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    agent_id: String,
    run_id: u64,
    sessions: tauri::State<'_, GodotSessions>,
) -> Result<RunUpdate, String> {
    let mut state = sessions
        .agent
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let agent = state
        .agents
        .get_mut(&agent_id)
        .filter(|agent| agent.owner == window.label() && !agent.closed)
        .ok_or("agent_unavailable")?;
    let retired = agent.retired;
    let in_flight = agent.run_is_active();
    let run = agent
        .run
        .as_mut()
        .filter(|run| run.id == run_id)
        .ok_or("run_unavailable")?;
    let phase = match &run.result {
        Some(_) if in_flight => "draining",
        Some(_) if retired || run.forced_error.is_some() => "failed",
        Some(_) if run.cancelled => "cancelled",
        Some(Ok(())) => "completed",
        Some(Err(_)) => "failed",
        None if run.cancelled => "cancelling",
        _ => "running",
    };
    let error = if retired {
        Some("target_unavailable")
    } else if let Some(error) = run.forced_error {
        Some(error)
    } else if phase == "failed" {
        run.result
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .copied()
    } else {
        None
    };
    run.queued_bytes = 0;
    let fragments = std::mem::take(&mut run.fragments);
    if retired && !in_flight && run.result.is_some() {
        state.agents.remove(&agent_id);
    }
    Ok(RunUpdate {
        agent_id,
        run_id,
        phase,
        fragments,
        error,
    })
}

#[tauri::command]
pub(crate) fn cancel_godot_agent<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    agent_id: String,
    run_id: u64,
    sessions: tauri::State<'_, GodotSessions>,
) -> Result<(), String> {
    let mut state = sessions
        .agent
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let agent = agent_mut(&mut state, &agent_id, window.label())?;
    let in_flight = agent.run_is_active();
    let run = agent
        .run
        .as_mut()
        .filter(|run| run.id == run_id)
        .ok_or("run_unavailable")?;
    if !in_flight {
        return Ok(());
    }
    run.cancelled = true;
    run.fragments.clear();
    run.queued_bytes = 0;
    run.cancellation.cancel();
    Ok(())
}

#[tauri::command]
pub(crate) fn close_godot_agent<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    agent_id: String,
    sessions: tauri::State<'_, GodotSessions>,
) -> Result<(), String> {
    sessions.agent.close_agent(&agent_id, window.label())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uebox_agent_core::{OpenAiProvider, ProviderConfig};

    #[test]
    fn output_backpressure_cancels_and_reports_a_real_limit_error() {
        let mut run = ActiveRun {
            id: 1,
            cancellation: Cancellation::new(),
            cancelled: false,
            result: None,
            fragments: Vec::new(),
            queued_bytes: 0,
            forced_error: None,
        };
        queue_text(&mut run, "x".repeat(MAX_QUEUED_TEXT));
        assert!(!run.cancellation.is_cancelled());
        queue_text(&mut run, "y".into());
        assert!(run.cancellation.is_cancelled());
        assert_eq!(run.forced_error, Some("agent_output_buffer_limit"));
        assert!(run.fragments.is_empty());
    }

    #[test]
    fn retiring_an_idle_target_releases_the_agent_even_if_no_one_polls_again() {
        let host = AgentHost::default();
        let provider =
            OpenAiProvider::new(ProviderConfig::new("http://127.0.0.1:9/fixture", "fixture"))
                .unwrap();
        let target = RunTarget::new(Engine::Godot, "project", "editor", 1).unwrap();
        let session = Arc::new(
            AgentSession::new(provider, ToolRegistry::new(target), Arc::new(|_| true), 1).unwrap(),
        );
        let weak = Arc::downgrade(&session);
        host.state.lock().unwrap().agents.insert(
            "agent".into(),
            Agent {
                owner: "main".into(),
                target_id: "target".into(),
                session: Some(session),
                credential: Arc::new(Mutex::new(Some("fixture".into()))),
                retired: false,
                closed: false,
                run: Some(ActiveRun {
                    id: 1,
                    cancellation: Cancellation::new(),
                    cancelled: false,
                    result: Some(Ok(())),
                    fragments: vec!["unpolled reply".into()],
                    queued_bytes: 14,
                    forced_error: None,
                }),
            },
        );
        host.retire("target");
        assert!(weak.upgrade().is_none());
    }
}
