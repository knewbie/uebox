//! Fixed-target Unreal Agent sessions on the existing listener.
use crate::chat_history::{finish_recorded_run, start_recorded_run};
use crate::unreal::UnrealListenerState;
use crate::unreal_actor_tool::add_actor_tool;
use crate::unreal_approval::{ApprovalBook, Proposal, Receipt};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tauri::{Manager, Runtime, WebviewWindow};
use uebox_agent_core::{
    AgentSession, Cancellation, Error, ModelEvent, OpenAiProvider, ProviderConfig,
};
use uebox_agent_engine_tools::unreal::{UnrealCandidate, UnrealProjectInfoBinding};
use uebox_engine_core::{EngineKind, EngineRegistry, InteractionMode, ProjectIdentity};
use uebox_engine_unreal::{SocketKey, UnrealBridge};

const MAX_QUEUED_TEXT: usize = 1024 * 1024;
const QUERY_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(crate) struct UnrealAgentHost {
    registry: Arc<Mutex<EngineRegistry>>,
    agents: Mutex<HashMap<String, Agent>>,
    approvals: Arc<ApprovalBook>,
}

impl UnrealAgentHost {
    pub(crate) fn destroy_window(&self, owner: &str) {
        let mut agents = lock(&self.agents);
        agents.retain(|_, agent| {
            if agent.owner == owner {
                agent.closed = true;
                agent.retire();
                agent.retain()
            } else {
                true
            }
        });
        self.approvals.revoke_owner(owner);
    }
}

struct ModelAccess {
    provider: OpenAiProvider,
    credential: Arc<Mutex<Option<String>>>,
    allow_actor_label_changes: bool,
}

struct Agent {
    owner: String,
    socket: SocketKey,
    live: Option<LiveAgent>,
    run: Option<ActiveRun>,
    retired: bool,
    closed: bool,
}

struct LiveAgent {
    candidate: UnrealCandidate,
    binding: UnrealProjectInfoBinding,
    session: AgentSession,
    credential: Arc<Mutex<Option<String>>>,
    monitor: tauri::async_runtime::JoinHandle<()>,
}

impl Agent {
    fn in_flight(&self) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.session.snapshot().run_id.is_some())
    }

    fn retire(&mut self) {
        if self.retired {
            return;
        }
        self.retired = true;
        if let Some(live) = &self.live {
            live.candidate.retire();
            lock(&live.credential).take();
            live.monitor.abort();
        }
        if let Some(run) = &mut self.run {
            run.cancel();
        }
    }

    fn retain(&mut self) -> bool {
        if !self.retired || self.in_flight() {
            return true;
        }
        self.live = None;
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

impl ActiveRun {
    fn cancel(&mut self) {
        self.cancelled = true;
        self.fragments.clear();
        self.queued_bytes = 0;
        self.cancellation.cancel();
    }

    fn queue_text(&mut self, text: String) {
        if self.cancelled {
            return;
        }
        if self.queued_bytes.saturating_add(text.len()) > MAX_QUEUED_TEXT {
            self.forced_error = Some("agent_output_buffer_limit");
            self.cancel();
        } else {
            self.queued_bytes += text.len();
            self.fragments.push(text);
        }
    }
}

impl UnrealAgentHost {
    pub(crate) fn retire_all(&self) {
        lock(&self.agents).retain(|_, agent| {
            agent.retire();
            agent.retain()
        });
    }

    fn retire(&self, id: &str) {
        let mut agents = lock(&self.agents);
        if let Some(agent) = agents.get_mut(id) {
            agent.retire();
            if !agent.retain() {
                agents.remove(id);
            }
        }
    }

    fn create(
        self: &Arc<Self>,
        owner: String,
        bridge: Arc<UnrealBridge>,
        connection_id: &str,
        project: ProjectIdentity,
        access: ModelAccess,
    ) -> Result<CreatedAgent, String> {
        let selected = bridge
            .select_instance(connection_id)
            .map_err(|_| "target_unavailable")?;
        let socket = selected.socket_key();
        let mut agents = lock(&self.agents);
        agents.retain(|_, agent| {
            if agent
                .live
                .as_ref()
                .is_some_and(|live| !live.binding.is_current())
            {
                agent.retire();
            }
            agent.retain()
        });
        if agents
            .values()
            .any(|agent| agent.live.is_some() && agent.socket == socket)
        {
            return Err("agent_already_exists".into());
        }
        // A new socket cannot establish that it is a different physical editor
        // from one whose retired run is still draining.
        if agents
            .values()
            .any(|agent| agent.retired && agent.live.is_some())
        {
            return Err("agent_busy".into());
        }
        let summary = bridge
            .instances()
            .into_iter()
            .find(|summary| summary.connection_id == connection_id)
            .ok_or("target_unavailable")?;
        let mode = if summary.interactive {
            InteractionMode::Interactive
        } else {
            InteractionMode::NonInteractive
        };
        if access.allow_actor_label_changes && !summary.interactive {
            return Err("target_unavailable".into());
        }
        let project_root = project.root().to_owned();
        let candidate =
            UnrealCandidate::capture(bridge, Arc::clone(&self.registry), project, &summary, mode)
                .map_err(|_| "target_unavailable")?;

        let initialized = (|| {
            let binding =
                UnrealProjectInfoBinding::bind_exact(&candidate, candidate.token(), QUERY_DEADLINE)
                    .map_err(|_| "target_unavailable")?;
            let mut agent_id = id()?;
            while agents.contains_key(&agent_id) {
                agent_id = id()?;
            }
            let mut tools = binding.tools().map_err(error_code)?;
            let prompt = if access.allow_actor_label_changes {
                tools = add_actor_tool(
                    tools,
                    binding.clone(),
                    self.approvals.clone(),
                    project_root,
                    owner.clone(),
                    agent_id.clone(),
                )
                .map_err(error_code)?;
                "You may request approval to change the selected Actor label. Treat denied, expired, unknown, or mismatched receipts accurately. Never claim a level was saved. Do not retry an unknown write."
            } else {
                "You are a read-only assistant for the selected Unreal editor. Use only the supplied read-only tool. Do not claim to change the project."
            };
            let session = AgentSession::new_with_system_prompt(
                access.provider,
                tools,
                binding.target_validator(),
                6,
                prompt,
            )
            .map_err(error_code)?;
            Ok::<_, &'static str>((binding, session, agent_id))
        })();
        let (binding, session, agent_id) = match initialized {
            Ok(initialized) => initialized,
            Err(code) => {
                candidate.retire();
                return Err(code.into());
            }
        };
        let monitor = {
            let binding = binding.clone();
            let host = Arc::downgrade(self);
            let id = agent_id.clone();
            tauri::async_runtime::spawn(async move {
                binding.wait_until_stale().await;
                if let Some(host) = host.upgrade() {
                    host.retire(&id);
                }
            })
        };
        agents.insert(
            agent_id.clone(),
            Agent {
                owner,
                socket,
                live: Some(LiveAgent {
                    candidate,
                    binding,
                    session,
                    credential: access.credential,
                    monitor,
                }),
                run: None,
                retired: false,
                closed: false,
            },
        );
        Ok(CreatedAgent { agent_id })
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn id() -> Result<String, &'static str> {
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

fn owned_agent<'a>(
    agents: &'a mut HashMap<String, Agent>,
    agent_id: &str,
    owner: &str,
) -> Result<&'a mut Agent, &'static str> {
    agents
        .get_mut(agent_id)
        .filter(|agent| agent.owner == owner && !agent.closed)
        .ok_or("agent_unavailable")
}

#[tauri::command]
pub(crate) async fn create_unreal_agent<R: Runtime>(
    window: WebviewWindow<R>,
    connection_id: String,
    project_root: String,
    endpoint: String,
    model: String,
    credential: Option<String>,
    allow_actor_label_changes: Option<bool>,
) -> Result<CreatedAgent, String> {
    let state = window.state::<UnrealListenerState>();
    let root = PathBuf::from(project_root);
    if !root.is_absolute() {
        return Err("target_unavailable".into());
    }
    {
        let listener = state.bridge.lock().await;
        listener
            .as_ref()
            .ok_or("target_unavailable")?
            .select_instance(&connection_id)
            .map_err(|_| "target_unavailable")?;
    }
    let project = tauri::async_runtime::spawn_blocking(move || {
        ProjectIdentity::new(EngineKind::Unreal, root).map_err(|_| "target_unavailable")
    })
    .await
    .map_err(|_| "agent_worker_error")??;
    let credential = Arc::new(Mutex::new(credential.filter(|value| !value.is_empty())));
    let required = lock(&credential).is_some();
    let token_slot = Arc::clone(&credential);
    let config = ProviderConfig::new(endpoint, model)
        .with_max_output_tokens(1024)
        .with_max_request_bytes(1024 * 1024)
        .with_token_provider(move || {
            let token = token_slot
                .lock()
                .map_err(|_| Error::CredentialUnavailable)?
                .clone();
            if required && token.is_none() {
                Err(Error::CredentialUnavailable)
            } else {
                Ok(token)
            }
        });
    let provider = OpenAiProvider::new(config).map_err(error_code)?;
    let listener = state.bridge.lock().await;
    state.agents.create(
        window.label().to_owned(),
        Arc::clone(listener.as_ref().ok_or("target_unavailable")?),
        &connection_id,
        project,
        ModelAccess {
            provider,
            credential,
            allow_actor_label_changes: allow_actor_label_changes == Some(true),
        },
    )
}

#[tauri::command]
pub(crate) async fn start_unreal_agent<R: Runtime>(
    window: WebviewWindow<R>,
    agent_id: String,
    text: String,
    save_history: Option<bool>,
    state: tauri::State<'_, UnrealListenerState>,
) -> Result<StartedRun, String> {
    let host = Arc::clone(&state.agents);
    let (run, recording) = {
        let mut agents = lock(&host.agents);
        let agent = owned_agent(&mut agents, &agent_id, window.label())?;
        let live = agent
            .live
            .as_ref()
            .filter(|_| !agent.retired)
            .ok_or("target_unavailable")?;
        if !live.binding.is_current() {
            agent.retire();
            if !agent.retain() {
                agents.remove(&agent_id);
            }
            return Err("target_unavailable".into());
        }
        if agent.run.as_ref().is_some_and(|run| run.result.is_none()) {
            return Err("agent_busy".into());
        }
        let (run, recording) = start_recorded_run(
            window.app_handle(),
            &live.session,
            &agent_id,
            uebox_chat_history::Engine::Unreal,
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
        (run, recording)
    };
    let run_id = run.id();
    let task_id = agent_id.clone();
    let receipt_log = recording.as_ref().map(|turn| {
        host.approvals
            .record_run(agent_id.clone(), run_id, turn.clone())
    });
    tauri::async_runtime::spawn(async move {
        let _receipt_log = receipt_log;
        let outcome = run
            .execute(|ModelEvent::TextDelta(text)| {
                if let Some(run) = lock(&host.agents)
                    .get_mut(&task_id)
                    .and_then(|agent| agent.run.as_mut())
                    .filter(|run| run.id == run_id)
                {
                    run.queue_text(text);
                }
            })
            .await;
        let result = finish_recorded_run(recording, outcome, error_code).await;
        if let Some(run) = lock(&host.agents)
            .get_mut(&task_id)
            .and_then(|agent| agent.run.as_mut())
            .filter(|run| run.id == run_id)
        {
            run.result = Some(result);
        }
        // A cancelled run can return before the core releases its tool permit.
        loop {
            let draining = {
                let mut agents = lock(&host.agents);
                let Some(agent) = agents.get_mut(&task_id) else {
                    break;
                };
                if !agent.run.as_ref().is_some_and(|run| run.id == run_id) {
                    break;
                }
                let in_flight = agent.in_flight();
                if !agent.retain() {
                    agents.remove(&task_id);
                }
                in_flight
            };
            if !draining {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    Ok(StartedRun { agent_id, run_id })
}

#[tauri::command]
pub(crate) fn poll_unreal_agent<R: Runtime>(
    window: WebviewWindow<R>,
    agent_id: String,
    run_id: u64,
    state: tauri::State<'_, UnrealListenerState>,
) -> Result<RunUpdate, String> {
    let mut agents = lock(&state.agents.agents);
    let agent = owned_agent(&mut agents, &agent_id, window.label())?;
    if agent
        .live
        .as_ref()
        .is_some_and(|live| !live.binding.is_current())
    {
        agent.retire();
    }
    let in_flight = agent.in_flight();
    let run = agent
        .run
        .as_mut()
        .filter(|run| run.id == run_id)
        .ok_or("run_unavailable")?;
    let phase = match &run.result {
        Some(_) if in_flight => "draining",
        Some(_) if agent.retired || run.forced_error.is_some() => "failed",
        Some(_) if run.cancelled => "cancelled",
        Some(Ok(())) => "completed",
        Some(Err(_)) => "failed",
        None if run.cancelled => "cancelling",
        None if state
            .agents
            .approvals
            .approvals(window.label(), &agent_id)
            .iter()
            .any(|proposal| proposal.run_id == run_id) =>
        {
            "awaiting_approval"
        }
        None => "running",
    };
    let error = if agent.retired {
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
    if agent.retired && !in_flight && run.result.is_some() {
        agents.remove(&agent_id);
    }
    Ok(RunUpdate {
        agent_id,
        run_id,
        phase,
        fragments,
        error,
    })
}

#[derive(Serialize)]
pub(crate) struct ActorApprovalSnapshot {
    approvals: Vec<Proposal>,
    receipts: Vec<Receipt>,
}

#[tauri::command]
pub(crate) fn list_unreal_actor_approvals<R: Runtime>(
    window: WebviewWindow<R>,
    agent_id: Option<String>,
    state: tauri::State<'_, UnrealListenerState>,
) -> ActorApprovalSnapshot {
    let book = &state.agents.approvals;
    ActorApprovalSnapshot {
        approvals: agent_id.map_or_else(Vec::new, |id| book.approvals(window.label(), &id)),
        receipts: book.receipts(window.label()),
    }
}

#[tauri::command]
pub(crate) fn answer_unreal_actor_approval<R: Runtime>(
    window: WebviewWindow<R>,
    approval_id: String,
    allowed: bool,
    state: tauri::State<'_, UnrealListenerState>,
) -> Result<(), &'static str> {
    state
        .agents
        .approvals
        .answer(window.label(), &approval_id, allowed)
}

#[tauri::command]
pub(crate) fn cancel_unreal_agent<R: Runtime>(
    window: WebviewWindow<R>,
    agent_id: String,
    run_id: u64,
    state: tauri::State<'_, UnrealListenerState>,
) -> Result<(), String> {
    let mut agents = lock(&state.agents.agents);
    let agent = owned_agent(&mut agents, &agent_id, window.label())?;
    if agent.retired {
        return Err("agent_unavailable".into());
    }
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
pub(crate) fn close_unreal_agent<R: Runtime>(
    window: WebviewWindow<R>,
    agent_id: String,
    state: tauri::State<'_, UnrealListenerState>,
) -> Result<(), String> {
    let mut agents = lock(&state.agents.agents);
    let agent = owned_agent(&mut agents, &agent_id, window.label())?;
    agent.closed = true;
    agent.retire();
    if !agent.retain() {
        agents.remove(&agent_id);
    }
    Ok(())
}
