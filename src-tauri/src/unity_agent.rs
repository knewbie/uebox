//! One explicitly authorized socket per Unity target. Agent runs never reconnect.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[path = "native_cli/unity_targets.rs"]
mod native_cli_targets;

use crate::chat_history::{finish_recorded_run, start_recorded_run};
use crate::settings::{AgentModel, AgentModelSource};
use crate::unity::{local_target, UnityQueries};
use crate::unity_rename_tool::{register_selected_name, Proposal, Receipt, UnityNameWrites};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tauri::{Manager, Runtime};
use uebox_agent_core::{AgentSession, Cancellation, Error, ModelEvent};
use uebox_agent_unity_tools::{ProbeState, UnityTools};
use uebox_engine_core::{
    ConnectionToken, EngineKind, EngineRegistry, InteractionMode, ProjectIdentity,
};
use uebox_engine_unity::{BridgeOptions, UnityClient};

const MAX_QUEUED_TEXT: usize = 1024 * 1024;
const IDLE_PROBE_INTERVAL: Duration = Duration::from_secs(8);

#[derive(Default)]
pub(crate) struct UnityAgentHost {
    registry: Arc<Mutex<EngineRegistry>>,
    state: Mutex<HostState>,
    name_writes: Arc<UnityNameWrites>,
}

impl UnityAgentHost {
    pub(crate) fn destroy_window(&self, owner: &str) {
        self.name_writes.revoke_owner(owner);
        let removed = {
            let mut state = lock(&self.state);
            let targets: Vec<_> = state
                .targets
                .iter()
                .filter(|(_, target)| target.owner == owner)
                .map(|(id, _)| id.clone())
                .collect();
            let removed: Vec<_> = targets
                .iter()
                .filter_map(|id| state.retire_target(id))
                .collect();
            for agent in state
                .agents
                .values_mut()
                .filter(|agent| agent.owner == owner)
            {
                agent.closed = true;
                agent.stop();
            }
            state.agents.retain(|_, agent| agent.retain());
            removed
        };
        for target in removed {
            tauri::async_runtime::spawn(async move { target.tools.close().await });
        }
    }
}

#[derive(Default)]
struct HostState {
    targets: HashMap<String, Target>,
    agents: HashMap<String, Agent>,
}

struct Target {
    owner: String,
    tools: UnityTools,
    probing: bool,
}

struct ProbeGate<'a> {
    host: &'a UnityAgentHost,
    target_id: &'a str,
}

impl Drop for ProbeGate<'_> {
    fn drop(&mut self) {
        if let Some(target) = lock(&self.host.state).targets.get_mut(self.target_id) {
            target.probing = false;
        }
    }
}

struct Agent {
    owner: String,
    target_id: String,
    session: Option<AgentSession>,
    credential: Arc<Mutex<Option<String>>>,
    run: Option<ActiveRun>,
    retired: bool,
    closed: bool,
}

struct ActiveRun {
    id: u64,
    cancellation: Cancellation,
    result: Option<Result<(), &'static str>>,
    fragments: Vec<String>,
    queued_bytes: usize,
    forced_error: Option<&'static str>,
    cancelled: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl ActiveRun {
    fn cancel(&mut self) {
        self.cancelled = true;
        self.fragments.clear();
        self.queued_bytes = 0;
        self.cancellation.cancel();
    }

    fn push(&mut self, text: String) {
        if self.cancelled {
            return;
        }
        if self.queued_bytes.saturating_add(text.len()) > MAX_QUEUED_TEXT {
            self.forced_error = Some("agent_output_buffer_limit");
            self.cancel();
            return;
        }
        self.queued_bytes += text.len();
        self.fragments.push(text);
    }
}

impl Agent {
    fn in_flight(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.snapshot().run_id.is_some())
    }

    fn stop(&mut self) {
        lock(&self.credential).take();
        if let Some(run) = &mut self.run {
            run.cancel();
        }
    }

    fn retain(&mut self) -> bool {
        if !(self.closed || self.retired) || self.in_flight() {
            return true;
        }
        self.session = None;
        // A retired run can still deliver its terminal receipt without keeping the provider.
        !self.closed && self.run.is_some()
    }
}

impl HostState {
    fn retire_target(&mut self, id: &str) -> Option<Target> {
        let target = self.targets.remove(id)?;
        for agent in self
            .agents
            .values_mut()
            .filter(|agent| agent.target_id == id)
        {
            agent.retired = true;
            agent.stop();
        }
        self.agents.retain(|_, agent| agent.retain());
        Some(target)
    }

    fn live_agent(&mut self, id: &str, owner: &str) -> Result<&mut Agent, String> {
        self.agents
            .get_mut(id)
            .filter(|agent| agent.owner == owner && !agent.retired && !agent.closed)
            .ok_or_else(|| "agent_unavailable".into())
    }
}

// Until UnityTools owns the registration, abandoning attach must retire the exact token.
struct PendingToken {
    registry: Arc<Mutex<EngineRegistry>>,
    token: Option<ConnectionToken>,
}

impl Drop for PendingToken {
    fn drop(&mut self) {
        if let Some(token) = &self.token {
            lock(&self.registry).close_connection(token);
        }
    }
}

fn id() -> Result<String, String> {
    let mut bytes = [0; 16];
    getrandom::getrandom(&mut bytes).map_err(|_| "agent_unavailable")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
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
pub(crate) struct AttachedTarget {
    session_id: String,
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

#[tauri::command]
pub(crate) async fn attach_unity_agent_target<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    project_root: String,
    port: u16,
    allow_client_takeover: Option<bool>,
    queries: tauri::State<'_, UnityQueries>,
) -> Result<AttachedTarget, String> {
    if allow_client_takeover != Some(true) {
        return Err("takeover_required".into());
    }
    let target = local_target(project_root, port).map_err(|_| "target_unavailable")?;
    let reservation = queries.reserve(port).map_err(|_| "target_busy")?;
    let session_id = id()?;
    let instance_id = session_id.clone();
    let host = Arc::clone(&queries.agent);
    let registry = Arc::clone(&host.registry);
    let (client, reservation, selection, mut pending) =
        tauri::async_runtime::spawn_blocking(move || {
            let client = UnityClient::connect_authorized(
                target.authorize_takeover(),
                BridgeOptions::default(),
            )
            .map_err(|_| "target_unavailable")?;
            let project =
                ProjectIdentity::new(EngineKind::Unity, &client.target().expected_project_root)
                    .map_err(|_| "target_unavailable")?;
            let (selection, token) = {
                let mut registry = lock(&registry);
                let token = registry
                    .open_connection(EngineKind::Unity, instance_id)
                    .map_err(|_| "target_unavailable")?;
                let selection = registry
                    .identify_with_mode(&token, project.clone(), InteractionMode::Unknown)
                    .and_then(|_| registry.select_exact(&token, &project, false));
                match selection {
                    Ok(selection) => (selection, token),
                    Err(_) => {
                        registry.close_connection(&token);
                        return Err("target_unavailable");
                    }
                }
            };
            let pending = PendingToken {
                registry,
                token: Some(token),
            };
            Ok((client, reservation, selection, pending))
        })
        .await
        .map_err(|_| "agent_worker_error")??;
    let token = pending.token.as_ref().ok_or("target_unavailable")?.clone();
    let tools = UnityTools::new_with_socket_guard(
        client,
        Arc::clone(&host.registry),
        selection,
        token,
        reservation,
    )
    .await
    .map_err(|_| "target_unavailable")?;
    pending.token = None;
    {
        let mut state = lock(&host.state);
        if state.targets.contains_key(&session_id) {
            return Err("agent_unavailable".into());
        }
        state.targets.insert(
            session_id.clone(),
            Target {
                owner: window.label().to_owned(),
                tools,
                probing: false,
            },
        );
    }
    monitor_target(&host, session_id.clone());
    Ok(AttachedTarget { session_id })
}

fn monitor_target(host: &Arc<UnityAgentHost>, id: String) {
    let weak = Arc::downgrade(host);
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(IDLE_PROBE_INTERVAL).await;
            let Some(host) = weak.upgrade() else { return };
            let tools = {
                let mut state = lock(&host.state);
                let busy = state
                    .agents
                    .values()
                    .any(|agent| agent.target_id == id && agent.in_flight());
                let Some(target) = state.targets.get_mut(&id) else {
                    return;
                };
                // Probing and starting a run reserve the same host slot. A ping
                // must never make a valid Agent tool fail with a busy socket.
                if busy || target.probing {
                    continue;
                }
                target.probing = true;
                target.tools.clone()
            };
            let probe = tools.probe().await;
            let mut state = lock(&host.state);
            if probe == ProbeState::Closed {
                state.retire_target(&id);
                return;
            }
            if let Some(target) = state.targets.get_mut(&id) {
                target.probing = false;
            }
        }
    });
}

#[tauri::command]
pub(crate) async fn release_unity_agent_target<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    session_id: String,
    queries: tauri::State<'_, UnityQueries>,
) -> Result<(), String> {
    let target = {
        let mut state = lock(&queries.agent.state);
        if !state
            .targets
            .get(&session_id)
            .is_some_and(|target| target.owner == window.label())
        {
            return Err("target_unavailable".into());
        }
        state
            .retire_target(&session_id)
            .ok_or("target_unavailable")?
    };
    // Keep closing if the caller stops waiting for the invoke response.
    tauri::async_runtime::spawn(async move { target.tools.close().await })
        .await
        .map_err(|_| "agent_worker_error".into())
}

#[tauri::command]
pub(crate) async fn create_unity_agent<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    session_id: String,
    endpoint: String,
    model: String,
    credential: Option<String>,
    allow_game_object_rename: Option<bool>,
) -> Result<CreatedAgent, String> {
    let source = AgentModelSource::Direct {
        endpoint,
        model,
        credential,
    };
    create_unity_with_model(
        window,
        session_id,
        source,
        allow_game_object_rename.unwrap_or(false),
    )
    .await
}

#[tauri::command]
pub(crate) async fn create_unity_agent_from_profile<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    session_id: String,
    profile_id: Option<String>,
    allow_game_object_rename: Option<bool>,
) -> Result<CreatedAgent, String> {
    let source = AgentModelSource::Saved { profile_id };
    create_unity_with_model(
        window,
        session_id,
        source,
        allow_game_object_rename.unwrap_or(false),
    )
    .await
}

async fn create_unity_with_model<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    session_id: String,
    source: AgentModelSource,
    allow_game_object_rename: bool,
) -> Result<CreatedAgent, String> {
    let host = Arc::clone(&window.state::<UnityQueries>().agent);
    let owner = window.label().to_owned();
    let tools = {
        let state = lock(&host.state);
        state
            .targets
            .get(&session_id)
            .filter(|target| target.owner == owner && !target.tools.is_retired())
            .ok_or("target_unavailable")?
            .tools
            .clone()
    };
    let agent_id = id()?;
    let AgentModel {
        provider,
        credential,
    } = source
        .resolve(window.app_handle().clone(), error_code)
        .await?;
    let mut registry = tools.tool_registry().map_err(error_code)?;
    let instructions = if allow_game_object_rename {
        registry = register_selected_name(
            registry,
            tools.clone(),
            Arc::clone(&host.name_writes),
            owner.clone(),
            agent_id.clone(),
        )
        .map_err(error_code)?;
        "Assist with the selected Unity editor. Use the rename tool to request approval for changing the single selected GameObject name. Report the outcome based on its receipt. The tool never saves the scene."
    } else {
        "You are a read-only assistant for the selected Unity editor. Use only the supplied read-only tools. Do not claim to change the project."
    };
    let session = AgentSession::new_with_system_prompt(
        provider,
        registry,
        tools.validator(),
        6,
        instructions,
    )
    .map_err(error_code)?;
    let mut state = lock(&host.state);
    if !state.targets.contains_key(&session_id) || tools.is_retired() {
        return Err("target_unavailable".into());
    }
    if state.agents.contains_key(&agent_id)
        || state
            .agents
            .values()
            .any(|agent| agent.target_id == session_id)
    {
        return Err("agent_already_exists".into());
    }
    state.agents.insert(
        agent_id.clone(),
        Agent {
            owner,
            target_id: session_id,
            session: Some(session),
            credential,
            run: None,
            retired: false,
            closed: false,
        },
    );
    Ok(CreatedAgent { agent_id })
}

#[tauri::command]
pub(crate) async fn start_unity_agent<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    agent_id: String,
    text: String,
    save_history: Option<bool>,
    queries: tauri::State<'_, UnityQueries>,
) -> Result<StartedRun, String> {
    if text.trim().is_empty() || text.len() > 64 * 1024 {
        return Err("invalid_message".into());
    }
    let host = Arc::clone(&queries.agent);
    let owner = window.label();
    let (target_id, tools) = {
        let mut state = lock(&host.state);
        let agent = state.live_agent(&agent_id, owner)?;
        if agent.in_flight() || agent.run.as_ref().is_some_and(|run| run.result.is_none()) {
            return Err("agent_busy".into());
        }
        let target_id = agent.target_id.clone();
        let target = state
            .targets
            .get_mut(&target_id)
            .ok_or("target_unavailable")?;
        if target.probing {
            return Err("agent_busy".into());
        }
        target.probing = true;
        (target_id, target.tools.clone())
    };
    // Dropping a cancelled invoke clears the host flag. An admitted ping still
    // keeps the underlying mutex, so another preflight will see it as Busy.
    let probe_gate = ProbeGate {
        host: &host,
        target_id: &target_id,
    };
    let probe = tools.probe().await;
    let (run, recording) = {
        let mut state = lock(&host.state);
        match probe {
            ProbeState::Busy => return Err("agent_busy".into()),
            ProbeState::Closed => {
                state.retire_target(&target_id);
                return Err("target_unavailable".into());
            }
            ProbeState::Ready => {}
        }
        let agent = state.live_agent(&agent_id, owner)?;
        let session = agent.session.as_ref().ok_or("agent_unavailable")?;
        let (run, recording) = start_recorded_run(
            window.app_handle(),
            session,
            &agent_id,
            uebox_chat_history::Engine::Unity,
            text,
            save_history.unwrap_or(false),
            error_code,
        )?;
        agent.run = Some(ActiveRun {
            id: run.id(),
            cancellation: run.cancellation(),
            result: None,
            fragments: Vec::new(),
            queued_bytes: 0,
            forced_error: None,
            cancelled: false,
        });
        (run, recording)
    };
    drop(probe_gate);
    let run_id = run.id();
    let worker_id = agent_id.clone();
    tauri::async_runtime::spawn(async move {
        let outcome = run
            .execute(|ModelEvent::TextDelta(text)| {
                if let Some(run) = lock(&host.state)
                    .agents
                    .get_mut(&worker_id)
                    .and_then(|agent| agent.run.as_mut())
                    .filter(|run| run.id == run_id)
                {
                    run.push(text);
                }
            })
            .await;
        let result = finish_recorded_run(recording, outcome, error_code).await;
        {
            let mut state = lock(&host.state);
            if tools.is_retired() {
                state.retire_target(&target_id);
            }
            if let Some(run) = state
                .agents
                .get_mut(&worker_id)
                .and_then(|agent| agent.run.as_mut())
                .filter(|run| run.id == run_id)
            {
                run.result = Some(result);
            }
        }
        loop {
            {
                let mut state = lock(&host.state);
                if !state.agents.get(&worker_id).is_some_and(|agent| {
                    agent.run.as_ref().is_some_and(|run| run.id == run_id) && agent.in_flight()
                }) {
                    if tools.is_retired() {
                        state.retire_target(&target_id);
                    }
                    state.agents.retain(|_, agent| agent.retain());
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    Ok(StartedRun { agent_id, run_id })
}

#[tauri::command]
pub(crate) fn poll_unity_agent<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    agent_id: String,
    run_id: u64,
    queries: tauri::State<'_, UnityQueries>,
) -> Result<RunUpdate, String> {
    let mut state = lock(&queries.agent.state);
    let agent = state
        .agents
        .get_mut(&agent_id)
        .filter(|agent| !agent.closed && agent.owner == window.label())
        .ok_or("agent_unavailable")?;
    let retired = agent.retired;
    let in_flight = agent.in_flight();
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
        None if !queries
            .agent
            .name_writes
            .approvals(window.label(), &agent_id)
            .is_empty() =>
        {
            "awaiting_approval"
        }
        None => "running",
    };
    let error = if retired {
        Some("target_unavailable")
    } else {
        run.forced_error.or_else(|| {
            if phase == "failed" {
                run.result.and_then(Result::err)
            } else {
                None
            }
        })
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
pub(crate) fn cancel_unity_agent<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    agent_id: String,
    run_id: u64,
    queries: tauri::State<'_, UnityQueries>,
) -> Result<(), String> {
    let mut state = lock(&queries.agent.state);
    let agent = state.live_agent(&agent_id, window.label())?;
    let in_flight = agent.in_flight();
    let run = agent
        .run
        .as_mut()
        .filter(|run| run.id == run_id)
        .ok_or("run_unavailable")?;
    if in_flight {
        run.cancel();
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn close_unity_agent<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    agent_id: String,
    queries: tauri::State<'_, UnityQueries>,
) -> Result<(), String> {
    let mut state = lock(&queries.agent.state);
    let agent = state
        .agents
        .get_mut(&agent_id)
        .filter(|agent| !agent.closed && agent.owner == window.label())
        .ok_or("agent_unavailable")?;
    agent.closed = true;
    agent.stop();
    if !agent.retain() {
        state.agents.remove(&agent_id);
    }
    Ok(())
}

#[derive(Serialize)]
pub(crate) struct UnityNameActivity {
    approvals: Vec<Proposal>,
    receipts: Vec<Receipt>,
}

#[tauri::command]
pub(crate) fn list_unity_name_activity<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    agent_id: String,
    queries: tauri::State<'_, UnityQueries>,
) -> Result<UnityNameActivity, &'static str> {
    let host = &queries.agent;
    Ok(UnityNameActivity {
        approvals: host.name_writes.approvals(window.label(), &agent_id),
        receipts: host.name_writes.receipts(window.label(), &agent_id),
    })
}

#[tauri::command]
pub(crate) fn answer_unity_name_approval<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    approval_id: String,
    approved: bool,
    queries: tauri::State<'_, UnityQueries>,
) -> Result<(), &'static str> {
    queries
        .agent
        .name_writes
        .answer(window.label(), &approval_id, approved)
}
