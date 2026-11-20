use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uebox_agent_core::{
    ApprovalDecision, Engine, Error, MutationPrepareFuture, MutationReceipt, MutationSettlement,
    MutationStatus, MutationTool, PreparedMutation, RunTarget, ToolApprovalFuture,
    ToolApprovalRequest, ToolApprover, ToolDefinition, ToolError, ToolRegistry,
};
use uebox_agent_unity_tools::UnityTools;
use uebox_engine_unity::GameObjectNameOutcome;

use crate::write_approval::{ApprovalQueue, WriteGate};

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(20);
const RECEIPT_LIMIT: usize = 128;

#[derive(Default)]
pub(crate) struct UnityNameWrites {
    pending: ApprovalQueue<Proposal>,
    writes: Arc<WriteGate>,
    receipts: Mutex<VecDeque<(String, Receipt)>>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Details {
    scene_handle: i32,
    game_object_instance_id: i32,
    old_name: String,
    new_name: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Proposal {
    approval_id: String,
    agent_id: String,
    run_id: u64,
    #[serde(flatten)]
    change: Details,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Receipt {
    approval_id: String,
    agent_id: String,
    run_id: u64,
    status: &'static str,
    actual_name: Option<String>,
}

impl UnityNameWrites {
    pub(crate) fn approvals(&self, owner: &str, agent_id: &str) -> Vec<Proposal> {
        self.pending.approvals(owner, agent_id)
    }

    pub(crate) fn answer(&self, owner: &str, id: &str, approved: bool) -> Result<(), &'static str> {
        self.pending.answer(owner, id, approved)
    }

    pub(crate) fn receipts(&self, owner: &str, agent_id: &str) -> Vec<Receipt> {
        self.receipts.lock().map_or_else(
            |_| Vec::new(),
            |receipts| {
                receipts
                    .iter()
                    .rev()
                    .filter(|(window, receipt)| window == owner && receipt.agent_id == agent_id)
                    .map(|(_, receipt)| receipt.clone())
                    .collect()
            },
        )
    }

    pub(crate) fn revoke_owner(&self, owner: &str) {
        self.pending.revoke_owner(owner);
        if let Ok(mut receipts) = self.receipts.lock() {
            receipts.retain(|(window, _)| window != owner);
        }
    }

    fn record(&self, owner: String, agent_id: String, settlement: MutationSettlement) {
        let receipt = Receipt {
            approval_id: settlement.invocation.approval_id.to_string(),
            agent_id,
            run_id: settlement.invocation.run_id,
            status: match settlement.receipt.status {
                MutationStatus::Applied => "applied",
                MutationStatus::Mismatch => "mismatch",
                MutationStatus::NotExecuted => "not_executed",
                MutationStatus::Unknown => "unknown",
            },
            actual_name: settlement.receipt.data["actualName"]
                .as_str()
                .map(str::to_owned),
        };
        if let Ok(mut receipts) = self.receipts.lock() {
            if receipts.len() >= RECEIPT_LIMIT {
                receipts.pop_front();
            }
            receipts.push_back((owner, receipt));
        }
    }
}

pub(crate) fn register_selected_name(
    registry: ToolRegistry,
    binding: UnityTools,
    state: Arc<UnityNameWrites>,
    owner: String,
    agent_id: String,
) -> Result<ToolRegistry, Error> {
    let approvals = Arc::new(NameApprover {
        state: Arc::clone(&state),
        owner: owner.clone(),
        agent_id: agent_id.clone(),
    });
    let writes = Arc::clone(&state.writes);
    let mut registry = registry.with_mutation_approval(
        approvals,
        APPROVAL_TIMEOUT,
        Arc::new(move |outcome| state.record(owner.clone(), agent_id.clone(), outcome)),
    )?;
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

struct NameApprover {
    state: Arc<UnityNameWrites>,
    owner: String,
    agent_id: String,
}

impl ToolApprover for NameApprover {
    fn approve(&self, request: ToolApprovalRequest) -> ToolApprovalFuture {
        let state = Arc::clone(&self.state);
        let owner = self.owner.clone();
        let agent_id = self.agent_id.clone();
        Box::pin(async move {
            let Ok(change) = serde_json::from_value(request.preview) else {
                return ApprovalDecision::Deny;
            };
            let proposal = Proposal {
                approval_id: request.invocation.approval_id.to_string(),
                agent_id: agent_id.clone(),
                run_id: request.invocation.run_id,
                change,
            };
            if state
                .pending
                .ask(
                    proposal.approval_id.clone(),
                    owner,
                    agent_id,
                    proposal,
                    request.deadline,
                )
                .await
            {
                ApprovalDecision::Approve
            } else {
                ApprovalDecision::Deny
            }
        })
    }
}
