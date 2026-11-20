use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uebox_agent_core::{
    Engine, Error, MutationPrepareFuture, MutationReceipt, MutationStatus, MutationTool,
    PreparedMutation, RunTarget, ToolDefinition, ToolError, ToolRegistry,
};
use uebox_agent_godot_tools::GodotToolBinding;
use uebox_godot_session::NodeRenameOutcome;

use crate::write_approval::{NameWrites, Proposal as SharedProposal, WriteGate};

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(40);

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Details {
    node_path: String,
    scene_path: Option<String>,
    old_name: String,
    new_name: String,
}

pub(crate) type GodotNameWrites = NameWrites<Details>;
pub(crate) type Proposal = SharedProposal<Details>;

pub(crate) fn register_node_rename(
    registry: ToolRegistry,
    binding: Arc<GodotToolBinding>,
    state: Arc<GodotNameWrites>,
    owner: String,
    agent_id: String,
) -> Result<ToolRegistry, Error> {
    let writes = state.write_gate();
    let mut registry = state.install(registry, owner, agent_id, APPROVAL_TIMEOUT)?;
    registry.register_mutation(
        Engine::Godot,
        ToolDefinition::new(
            "godot_rename_scene_node",
            "Request an approved rename of an owned node in the currently edited Godot scene. Use the non-root path returned by godot_scene_tree. The tool never saves the scene.",
            json!({
                "type":"object","properties":{
                    "node_path":{"type":"string","minLength":1,"maxLength":4096},
                    "name":{"type":"string","minLength":1,"maxLength":128}
                },
                "required":["node_path","name"],"additionalProperties":false
            }),
        ),
        Arc::new(NodeRenameTool { binding, writes }),
    )?;
    Ok(registry)
}

struct NodeRenameTool {
    binding: Arc<GodotToolBinding>,
    writes: Arc<WriteGate>,
}

impl MutationTool for NodeRenameTool {
    fn prepare(&self, target: RunTarget, arguments: Value) -> MutationPrepareFuture {
        let binding = Arc::clone(&self.binding);
        let writes = Arc::clone(&self.writes);
        Box::pin(async move {
            let path = arguments["node_path"].as_str().ok_or(ToolError)?.to_owned();
            let name = arguments["name"].as_str().ok_or(ToolError)?.to_owned();
            let prepared = binding.prepare_node_rename(&target, path, name).await?;
            let details = json!({
                "nodePath": prepared.node_path(),
                "scenePath": prepared.scene_path(),
                "oldName": prepared.old_name(),
                "newName": prepared.new_name()
            });
            Ok(PreparedMutation::new(
                "Rename Godot node",
                details,
                move || {
                    Box::pin(async move {
                        let permit = match writes.acquire(binding.project_root()) {
                            Ok(permit) => permit,
                            Err(reason) => return not_executed(reason),
                        };
                        let outcome = binding.apply_node_rename(&target, prepared).await;
                        permit.settle(outcome == NodeRenameOutcome::Unknown);
                        match outcome {
                            NodeRenameOutcome::Applied { actual_name } => MutationReceipt {
                                status: MutationStatus::Applied,
                                data: json!({"actualName":actual_name,"saved":false}),
                            },
                            NodeRenameOutcome::Mismatch { actual_name } => MutationReceipt {
                                status: MutationStatus::Mismatch,
                                data: json!({"actualName":actual_name,"saved":false}),
                            },
                            NodeRenameOutcome::NotExecuted { reason } => not_executed(reason),
                            NodeRenameOutcome::Unknown => MutationReceipt {
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
