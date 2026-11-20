use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uebox_agent_core::{
    Engine, Error, MutationPrepareFuture, MutationReceipt, MutationStatus, MutationTool,
    PreparedMutation, RunTarget, ToolDefinition, ToolError, ToolRegistry,
};
use uebox_agent_unity_tools::UnityTools;
use uebox_engine_unity::GameObjectNameOutcome;

use crate::write_approval::{NameWrites, Proposal as SharedProposal, WriteGate};

pub(crate) type UnityNameWrites = NameWrites<Details>;
pub(crate) type Proposal = SharedProposal<Details>;

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Details {
    scene_handle: i32,
    game_object_instance_id: i32,
    old_name: String,
    new_name: String,
}

pub(crate) fn register_selected_name(
    registry: ToolRegistry,
    binding: UnityTools,
    state: Arc<UnityNameWrites>,
    owner: String,
    agent_id: String,
) -> Result<ToolRegistry, Error> {
    let writes = state.write_gate();
    let mut registry = state.install(registry, owner, agent_id, APPROVAL_TIMEOUT)?;
    registry.register_mutation(
        Engine::Unity,
        ToolDefinition::new(
            "unity_rename_selected_game_object",
            "Request an approved rename of the single selected GameObject in the active Unity scene. This does not save the scene.",
            json!({
                "type":"object","properties":{"name":{"type":"string","minLength":1,"maxLength":256}},
                "required":["name"],"additionalProperties":false
            }),
        ),
        Arc::new(NameTool { binding, writes }),
    )?;
    Ok(registry)
}

struct NameTool {
    binding: UnityTools,
    writes: Arc<WriteGate>,
}

impl MutationTool for NameTool {
    fn prepare(&self, target: RunTarget, arguments: Value) -> MutationPrepareFuture {
        let binding = self.binding.clone();
        let writes = Arc::clone(&self.writes);
        Box::pin(async move {
            let name = arguments["name"].as_str().ok_or(ToolError)?.to_owned();
            let prepared = binding.prepare_game_object_name(&target, name).await?;
            let details = json!({
                "sceneHandle": prepared.scene_handle(),
                "gameObjectInstanceId": prepared.game_object_instance_id(),
                "oldName": prepared.old_name(),
                "newName": prepared.proposed_name()
            });
            Ok(PreparedMutation::new(
                "Rename selected GameObject",
                details,
                move || {
                    Box::pin(async move {
                        let permit = match writes.acquire(binding.project_root()) {
                            Ok(permit) => permit,
                            Err(reason) => return not_executed(reason),
                        };
                        let outcome = binding.apply_game_object_name(&target, prepared).await;
                        permit.settle(outcome == GameObjectNameOutcome::Unknown);
                        match outcome {
                            GameObjectNameOutcome::Applied { actual_name } => MutationReceipt {
                                status: MutationStatus::Applied,
                                data: json!({"actualName":actual_name,"saved":false}),
                            },
                            GameObjectNameOutcome::Mismatch { actual_name } => MutationReceipt {
                                status: MutationStatus::Mismatch,
                                data: json!({"actualName":actual_name,"saved":false}),
                            },
                            GameObjectNameOutcome::NotExecuted { reason } => not_executed(reason),
                            GameObjectNameOutcome::Unknown => MutationReceipt {
                                status: MutationStatus::Unknown,
                                data: json!({"reason":"do_not_retry"}),
                            },
                        }
                    })
                },
            ))
        })
    }
}

fn not_executed(reason: &'static str) -> MutationReceipt {
    MutationReceipt {
        status: MutationStatus::NotExecuted,
        data: json!({"reason":reason}),
    }
}
