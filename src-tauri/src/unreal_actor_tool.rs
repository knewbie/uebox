use crate::unreal_approval::{ApprovalBook, Proposal, Receipt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc, time::Duration};
use uebox_agent_core::{
    ApprovalDecision, Engine, Error, MutationPrepareFuture, MutationReceipt, MutationSettlement,
    MutationStatus, MutationTool, PreparedMutation, RunTarget, ToolApprovalFuture,
    ToolApprovalRequest, ToolApprover, ToolDefinition, ToolError, ToolRegistry,
};
use uebox_agent_engine_tools::unreal::UnrealProjectInfoBinding;
use uebox_engine_unreal::{ActorLabelOutcome, ActorLabelRejection};

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(45);

pub(crate) fn add_actor_tool(
    tools: ToolRegistry,
    binding: UnrealProjectInfoBinding,
    book: Arc<ApprovalBook>,
    project: PathBuf,
    owner: String,
    agent_id: String,
) -> Result<ToolRegistry, Error> {
    let approver = Arc::new(ActorApprover {
        book: book.clone(),
        owner: owner.clone(),
        agent_id: agent_id.clone(),
    });
    let receipts = Arc::clone(&book);
    let mut tools = tools.with_mutation_approval(
        approver,
        APPROVAL_TIMEOUT,
        Arc::new(move |settlement| record(&receipts, &owner, &agent_id, settlement)),
    )?;
    tools.register_mutation(
        Engine::Unreal,
        ToolDefinition::new(
            "ue_change_selected_actor_label",
            "Request an approved label change for the single selected Unreal Actor. This does not save the level.",
            json!({"type":"object","properties":{"label":{"type":"string","minLength":1,"maxLength":128}},"required":["label"],"additionalProperties":false}),
        ),
        Arc::new(ActorLabelTool { binding, project, book }),
    )?;
    Ok(tools)
}

struct ActorLabelTool {
    binding: UnrealProjectInfoBinding,
    project: PathBuf,
    book: Arc<ApprovalBook>,
}

impl MutationTool for ActorLabelTool {
    fn prepare(&self, target: RunTarget, arguments: Value) -> MutationPrepareFuture {
        let binding = self.binding.clone();
        let project = self.project.clone();
        let book = self.book.clone();
        Box::pin(async move {
            if &target != binding.target() {
                return Err(ToolError);
            }
            let label = arguments["label"].as_str().ok_or(ToolError)?;
            let prepared = binding
                .prepare_actor_label(label)
                .await
                .map_err(|_| ToolError)?;
            let preview = json!({
                "actorPath": prepared.actor_path(),
                "oldLabel": prepared.old_label(),
                "newLabel": prepared.new_label()
            });
            Ok(PreparedMutation::new(
                "Change the selected Actor label",
                preview,
                move || {
                    Box::pin(async move {
                        let permit = match book.try_begin_write(&project) {
                            Ok(permit) => permit,
                            Err(reason) => return not_executed(reason),
                        };
                        let reply = binding.commit_actor_label(prepared).await;
                        permit.settle(matches!(reply, ActorLabelOutcome::OutcomeUnknown));
                        match reply {
                            ActorLabelOutcome::Applied { actual_label } => MutationReceipt {
                                status: MutationStatus::Applied,
                                data: json!({"actualLabel":actual_label,"saved":false}),
                            },
                            ActorLabelOutcome::ReadbackMismatch { actual_label } => {
                                MutationReceipt {
                                    status: MutationStatus::Mismatch,
                                    data: json!({"actualLabel":actual_label,"saved":false}),
                                }
                            }
                            ActorLabelOutcome::NotExecuted(rejection) => {
                                not_executed(rejection_code(rejection))
                            }
                            ActorLabelOutcome::OutcomeUnknown => MutationReceipt {
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

fn rejection_code(rejection: ActorLabelRejection) -> &'static str {
    match rejection {
        ActorLabelRejection::WrongTarget | ActorLabelRejection::StaleSelection => "target_changed",
        ActorLabelRejection::BridgeUnavailable => "bridge_unavailable",
        ActorLabelRejection::InvalidRequest => "invalid_request",
        ActorLabelRejection::ExpiredOrConsumed => "expired_or_consumed",
        ActorLabelRejection::ContextChanged => "context_changed",
        ActorLabelRejection::ActorChanged => "actor_changed",
        ActorLabelRejection::LabelChanged => "label_changed",
        ActorLabelRejection::PlayInProgress => "play_in_progress",
        ActorLabelRejection::TransactionUnavailable => "transaction_unavailable",
    }
}

struct ActorApprover {
    book: Arc<ApprovalBook>,
    owner: String,
    agent_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Preview {
    actor_path: String,
    old_label: String,
    new_label: String,
}

impl ToolApprover for ActorApprover {
    fn approve(&self, request: ToolApprovalRequest) -> ToolApprovalFuture {
        let owner = self.owner.clone();
        let agent_id = self.agent_id.clone();
        let book = self.book.clone();
        Box::pin(async move {
            let Ok(preview) = serde_json::from_value::<Preview>(request.preview) else {
                return ApprovalDecision::Deny;
            };
            let proposal = Proposal {
                approval_id: request.invocation.approval_id.to_string(),
                agent_id,
                run_id: request.invocation.run_id,
                actor_path: preview.actor_path,
                old_label: preview.old_label,
                new_label: preview.new_label,
            };
            if book.ask(owner, proposal, request.deadline).await {
                ApprovalDecision::Approve
            } else {
                ApprovalDecision::Deny
            }
        })
    }
}

fn record(book: &ApprovalBook, owner: &str, agent_id: &str, settlement: MutationSettlement) {
    let status = match settlement.receipt.status {
        MutationStatus::Applied => "applied",
        MutationStatus::Mismatch => "mismatch",
        MutationStatus::NotExecuted => "not_executed",
        MutationStatus::Unknown => "unknown",
    };
    book.record(
        owner.to_owned(),
        Receipt {
            approval_id: settlement.invocation.approval_id.to_string(),
            agent_id: agent_id.to_owned(),
            run_id: settlement.invocation.run_id,
            status,
            actual_label: settlement.receipt.data["actualLabel"]
                .as_str()
                .map(str::to_owned),
            history_error: None,
        },
    );
}
