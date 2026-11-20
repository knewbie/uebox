use std::path::Path;

use serde_json::{json, Value};
use uebox_cli::{Dispatch, Engine, HostError, OnlineProject, ReadTool};

use super::{lock, UnityAgentHost, UnityTools};

fn summary(id: &str, tools: &UnityTools) -> OnlineProject {
    let path = tools.project_root();
    OnlineProject {
        engine: Engine::Unity,
        name: project_name(path),
        path: path.to_path_buf(),
        connection_id: id.to_owned(),
    }
}

fn project_name(path: &Path) -> String {
    path.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|value| value.len() <= 256 && !value.chars().any(char::is_control))
        .unwrap_or("Unity")
        .to_owned()
}

impl UnityAgentHost {
    pub(crate) fn cli_projects(&self) -> Vec<OnlineProject> {
        lock(&self.state)
            .targets
            .iter()
            .filter(|(_, target)| !target.tools.is_retired())
            .map(|(id, target)| summary(id, &target.tools))
            .collect()
    }

    pub(crate) async fn cli_query(
        &self,
        tool: ReadTool,
        project: &OnlineProject,
        dispatch: &Dispatch,
    ) -> Result<Value, HostError> {
        let tools = {
            let state = lock(&self.state);
            let target = state
                .targets
                .get(&project.connection_id)
                .ok_or(HostError::ProjectNotConnected)?;
            if target.tools.project_root().as_os_str() != project.path.as_os_str()
                || target.tools.is_retired()
            {
                return Err(HostError::ProjectNotConnected);
            }
            if target.probing
                || state
                    .agents
                    .values()
                    .any(|agent| agent.target_id == project.connection_id && agent.in_flight())
            {
                return Err(HostError::ToolUnavailable);
            }
            target.tools.clone()
        };
        let adapter = match tool {
            ReadTool::UnityProjectInfo => tools.project_info_tool(),
            ReadTool::UnityActiveScene => tools.active_scene_tool(),
            _ => return Err(HostError::ToolUnavailable),
        };
        dispatch.mark_dispatched();
        adapter
            .execute(tools.run_target(), json!({}))
            .await
            .map_err(|_| HostError::ToolFailed)
    }
}
