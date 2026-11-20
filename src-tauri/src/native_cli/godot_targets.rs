use serde_json::{json, Value};
use uebox_cli::{Dispatch, Engine, HostError, OnlineProject, ReadTool};

use super::{AgentHost, GodotToolBinding, Target};

fn summary(id: &str, target: &Target) -> OnlineProject {
    let path = target.selection.project().root();
    OnlineProject {
        engine: Engine::Godot,
        name: path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .filter(|value| value.len() <= 256 && !value.chars().any(char::is_control))
            .unwrap_or("Godot")
            .to_owned(),
        path: path.to_owned(),
        connection_id: id.to_owned(),
    }
}

impl AgentHost {
    pub(crate) fn cli_projects(&self) -> Vec<OnlineProject> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .targets
            .iter()
            .map(|(id, target)| summary(id, target))
            .collect()
    }

    pub(crate) async fn cli_query(
        &self,
        tool: ReadTool,
        project: &OnlineProject,
        dispatch: &Dispatch,
    ) -> Result<Value, HostError> {
        let target = {
            let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            let target = state
                .targets
                .get(&project.connection_id)
                .ok_or(HostError::ProjectNotConnected)?;
            if target.selection.project().root().as_os_str() != project.path.as_os_str() {
                return Err(HostError::ProjectNotConnected);
            }
            if state
                .agents
                .values()
                .any(|agent| agent.target_id == project.connection_id && agent.run_is_active())
            {
                return Err(HostError::ToolUnavailable);
            }
            target.clone()
        };
        let binding = tauri::async_runtime::spawn_blocking({
            let registry = self.registry.clone();
            move || GodotToolBinding::new(registry, target.selection, target.handle)
        })
        .await
        .map_err(|_| HostError::ToolFailed)?
        .map_err(|_| HostError::ProjectNotConnected)?;
        let adapter = match tool {
            ReadTool::GodotProjectInfo => binding.project_info_tool(),
            ReadTool::GodotSceneTree => binding.scene_tree_tool(),
            _ => return Err(HostError::ToolUnavailable),
        };
        dispatch.mark_dispatched();
        adapter
            .execute(binding.target(), json!({}))
            .await
            .map_err(|_| HostError::ToolFailed)
    }
}
