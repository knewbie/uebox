use std::sync::Arc;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};
use uebox_agent_core::{
    AgentSession, Engine, ReadOnlyTool, RunTarget, ToolDefinition, ToolError, ToolFuture,
    ToolRegistry,
};
use uebox_cli::{
    AskReply, Dispatch, Engine as ProjectEngine, HostError, HostSession, OnlineProject, ReadTool,
    MAX_ASK_QUESTION_BYTES, MAX_ASK_REPLY_BYTES,
};

use super::Console;

pub(super) async fn answer<R: Runtime>(
    console: &Console<R>,
    model_profile_id: &str,
    question: &str,
    project: &OnlineProject,
    allowed: &[ReadTool],
    dispatch: &Dispatch,
) -> Result<AskReply, HostError> {
    if !console.allow_ask {
        return Err(HostError::AskUnavailable);
    }
    let host = console.app.state::<super::NativeCli>();
    let _admission = host
        .ask_gate
        .try_lock()
        .map_err(|_| HostError::AskUnavailable)?;
    if question.trim().is_empty()
        || question.len() > MAX_ASK_QUESTION_BYTES
        || question.contains('\0')
    {
        return Err(HostError::PermissionDenied);
    }
    if !is_current(console, project).await? {
        return Err(HostError::ProjectNotConnected);
    }
    let available = console.tools().await?;
    let permitted: Vec<_> = ReadTool::allowlist_for(project.engine)
        .iter()
        .copied()
        .filter(|tool| available.contains(tool))
        .collect();
    if permitted.is_empty()
        || allowed.len() != permitted.len()
        || permitted.iter().any(|tool| !allowed.contains(tool))
    {
        return Err(HostError::ToolUnavailable);
    }

    let engine = match project.engine {
        ProjectEngine::Unreal => Engine::Unreal,
        ProjectEngine::Unity => Engine::Unity,
        ProjectEngine::Godot => Engine::Godot,
    };
    let target = RunTarget::new(engine, "selected project", "selected editor", 1)
        .map_err(|_| HostError::ProjectNotConnected)?;
    let mut tools = ToolRegistry::new(target.clone());
    for tool in permitted {
        let schema = json!({"type":"object","properties":{},"additionalProperties":false});
        tools
            .register(
                engine,
                ToolDefinition::new(tool.name(), "Read data from the selected editor.", schema),
                Arc::new(EditorRead {
                    app: console.app.clone(),
                    project: project.clone(),
                    tool,
                }),
            )
            .map_err(|_| HostError::ToolUnavailable)?;
    }
    let model =
        crate::settings::resolve_cli_model(console.app.clone(), model_profile_id.to_owned())
            .await
            .map_err(|_| HostError::ModelUnavailable)?;
    let session = AgentSession::new_with_system_prompt(
        model.provider,
        tools,
        Arc::new(move |current| current == &target),
        6,
        "Answer one question about the selected game editor. Use only the provided read-only tools. Do not claim to change the project.",
    ).map_err(|_| HostError::ModelUnavailable)?;
    let run = session
        .start(question.to_owned())
        .map_err(|_| HostError::ModelUnavailable)?;
    if !is_current(console, project).await? {
        return Err(HostError::ProjectNotConnected);
    }
    dispatch.mark_dispatched();
    let answer = run
        .execute(|_| {})
        .await
        .map_err(|_| HostError::ModelUnavailable)?;
    if !is_current(console, project).await? {
        return Err(HostError::ProjectNotConnected);
    }
    if answer.text.len() > MAX_ASK_REPLY_BYTES {
        return Err(HostError::ModelUnavailable);
    }
    Ok(AskReply { text: answer.text })
}

async fn is_current<R: Runtime>(
    console: &Console<R>,
    selected: &OnlineProject,
) -> Result<bool, HostError> {
    Ok(console.projects().await?.iter().any(|entry| {
        entry.engine == selected.engine
            && entry.connection_id == selected.connection_id
            && entry.path == selected.path
    }))
}

struct EditorRead<R: Runtime> {
    app: AppHandle<R>,
    project: OnlineProject,
    tool: ReadTool,
}

impl<R: Runtime> ReadOnlyTool for EditorRead<R> {
    fn execute(&self, _target: RunTarget, arguments: Value) -> ToolFuture {
        let console = Console {
            app: self.app.clone(),
            allow_ask: false,
        };
        let project = self.project.clone();
        let tool = self.tool;
        Box::pin(async move {
            if !arguments.as_object().is_some_and(|data| data.is_empty()) {
                return Err(ToolError);
            }
            console
                .call(tool, &project, &Dispatch::default())
                .await
                .map(|reply| reply.structured)
                .map_err(|_| ToolError)
        })
    }
}
