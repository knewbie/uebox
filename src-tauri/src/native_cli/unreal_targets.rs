use std::{path::PathBuf, time::Duration};

use serde_json::Value;
use uebox_agent_engine_tools::unreal::public_project_info;
use uebox_cli::{Dispatch, Engine, HostError, OnlineProject};
use uebox_engine_unreal::InstanceSummary;

use crate::unreal::UnrealListenerState;

fn summary(instance: InstanceSummary) -> Option<OnlineProject> {
    if instance.project_path.len() > 8192
        || instance.project_path.chars().any(char::is_control)
        || instance.project_name.len() > 256
        || instance.project_name.chars().any(char::is_control)
    {
        return None;
    }
    let path = PathBuf::from(instance.project_path);
    if !path.is_absolute() {
        return None;
    }
    Some(OnlineProject {
        engine: Engine::Unreal,
        name: instance.project_name,
        path,
        connection_id: instance.connection_id,
    })
}

impl UnrealListenerState {
    pub(crate) async fn cli_projects(&self) -> Vec<OnlineProject> {
        let bridge = self.bridge.lock().await;
        bridge
            .as_ref()
            .map(|bridge| bridge.instances().into_iter().filter_map(summary).collect())
            .unwrap_or_default()
    }

    pub(crate) async fn cli_query(
        &self,
        project: &OnlineProject,
        dispatch: &Dispatch,
    ) -> Result<Value, HostError> {
        let instance = {
            let bridge = self.bridge.lock().await;
            let bridge = bridge.as_ref().ok_or(HostError::ProjectNotConnected)?;
            if !bridge
                .instances()
                .into_iter()
                .filter_map(summary)
                .any(|entry| {
                    entry.connection_id == project.connection_id
                        && entry.path.as_os_str() == project.path.as_os_str()
                        && entry.engine == project.engine
                })
            {
                return Err(HostError::ProjectNotConnected);
            }
            bridge
                .select_instance(&project.connection_id)
                .map_err(|_| HostError::ProjectNotConnected)?
        };
        dispatch.mark_dispatched();
        let result = instance
            .get_project_info(Duration::from_secs(10))
            .await
            .map_err(|_| HostError::ToolFailed)?;
        let target = project
            .path
            .to_str()
            .ok_or(HostError::ProjectNotConnected)?;
        let reported = result["projectPath"]
            .as_str()
            .ok_or(HostError::ToolFailed)?;
        if target.trim_end_matches('/') != reported.trim_end_matches('/') {
            return Err(HostError::ProjectNotConnected);
        }
        Ok(public_project_info(&result))
    }
}
