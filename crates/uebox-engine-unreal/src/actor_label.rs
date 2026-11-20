use super::{BridgeError, Connection, RpcMethod, SelectedInstance};
use serde_json::{json, Value};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

const PROTOCOL: &str = "actor_label_v1";
const MAX_LABEL_UNITS: usize = 128;

/// A single-use capability. Keep it in the trusted host until the user approves.
/// Only the display fields should leave the host.
#[must_use]
pub struct PreparedActorLabel {
    connection: Arc<Connection>,
    generation: u64,
    ticket: String,
    actor_path: String,
    old_label: String,
    new_label: String,
}

impl PreparedActorLabel {
    /// Unreal object path. This is not a filesystem path.
    pub fn actor_path(&self) -> &str {
        &self.actor_path
    }

    pub fn old_label(&self) -> &str {
        &self.old_label
    }

    pub fn new_label(&self) -> &str {
        &self.new_label
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorLabelRejection {
    WrongTarget,
    StaleSelection,
    BridgeUnavailable,
    InvalidRequest,
    ExpiredOrConsumed,
    ContextChanged,
    ActorChanged,
    LabelChanged,
    PlayInProgress,
    TransactionUnavailable,
}

#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub enum ActorLabelOutcome {
    Applied {
        actual_label: String,
    },
    ReadbackMismatch {
        actual_label: String,
    },
    NotExecuted(ActorLabelRejection),
    /// A frame may have reached UE. Do not automatically resend or re-prepare.
    OutcomeUnknown,
}

impl SelectedInstance {
    /// Snapshot the single selected Actor without editing it. An unsupported
    /// plugin fails here, before the host can obtain a write capability.
    pub async fn prepare_actor_label(
        &self,
        new_label: &str,
        deadline: Duration,
    ) -> Result<PreparedActorLabel, BridgeError> {
        if new_label.trim().is_empty()
            || new_label.encode_utf16().count() > MAX_LABEL_UNITS
            || new_label.chars().any(char::is_control)
        {
            return Err(BridgeError::InvalidActorLabel);
        }
        let receipt = self
            .request(
                RpcMethod::PrepareActorLabel,
                json!({"new_label": new_label}),
                deadline,
                false,
                Arc::default(),
            )
            .await?;
        if !self.is_current() || receipt["protocol"] != PROTOCOL || receipt["status"] != "prepared"
        {
            return Err(BridgeError::InvalidResponse);
        }
        let ticket = receipt["ticket"]
            .as_str()
            .filter(|value| value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or(BridgeError::InvalidResponse)?;
        Ok(PreparedActorLabel {
            connection: Arc::clone(&self.connection),
            generation: self.generation,
            ticket: ticket.to_owned(),
            actor_path: read_path(&receipt)?.to_owned(),
            old_label: read_label(&receipt, "old_label")?.to_owned(),
            new_label: new_label.to_owned(),
        })
    }

    /// Use the exact prepared request once. The caller should keep this future
    /// alive through settlement even after the UI stops waiting for it.
    pub async fn commit_actor_label(
        &self,
        prepared: PreparedActorLabel,
        deadline: Duration,
    ) -> ActorLabelOutcome {
        if !Arc::ptr_eq(&self.connection, &prepared.connection)
            || self.generation != prepared.generation
        {
            return ActorLabelOutcome::NotExecuted(ActorLabelRejection::WrongTarget);
        }

        let dispatched = Arc::default();
        let reply = self
            .request(
                RpcMethod::CommitActorLabel,
                // The legacy setter requires targets and properties. Never
                // include them here, so an old plugin cannot apply this frame.
                json!({"guarded_actor_label":{"ticket":prepared.ticket}}),
                deadline,
                true,
                Arc::clone(&dispatched),
            )
            .await;
        match reply {
            Ok(receipt) => decode_commit(&receipt, &prepared),
            Err(error) if !dispatched.load(Ordering::Acquire) => {
                let reason = match error {
                    BridgeError::StaleSelection => ActorLabelRejection::StaleSelection,
                    _ => ActorLabelRejection::BridgeUnavailable,
                };
                ActorLabelOutcome::NotExecuted(reason)
            }
            Err(_) => ActorLabelOutcome::OutcomeUnknown,
        }
    }
}

fn read_path(receipt: &Value) -> Result<&str, BridgeError> {
    receipt["actor_path"]
        .as_str()
        .filter(|path| !path.is_empty() && path.len() <= 4096 && !path.contains('\0'))
        .ok_or(BridgeError::InvalidResponse)
}

fn read_label<'a>(receipt: &'a Value, key: &str) -> Result<&'a str, BridgeError> {
    receipt[key]
        .as_str()
        .filter(|label| label.len() <= 4096 && !label.contains('\0'))
        .ok_or(BridgeError::InvalidResponse)
}

fn decode_commit(receipt: &Value, prepared: &PreparedActorLabel) -> ActorLabelOutcome {
    if receipt["protocol"] != PROTOCOL || receipt["ticket"] != prepared.ticket {
        return ActorLabelOutcome::OutcomeUnknown;
    }
    if receipt["status"] == "not_executed" {
        return match receipt["reason"].as_str() {
            Some("invalid_request") => {
                ActorLabelOutcome::NotExecuted(ActorLabelRejection::InvalidRequest)
            }
            Some("expired_or_consumed") => {
                ActorLabelOutcome::NotExecuted(ActorLabelRejection::ExpiredOrConsumed)
            }
            Some("context_changed") => {
                ActorLabelOutcome::NotExecuted(ActorLabelRejection::ContextChanged)
            }
            Some("actor_changed") => {
                ActorLabelOutcome::NotExecuted(ActorLabelRejection::ActorChanged)
            }
            Some("label_changed") => {
                ActorLabelOutcome::NotExecuted(ActorLabelRejection::LabelChanged)
            }
            Some("play_in_progress") => {
                ActorLabelOutcome::NotExecuted(ActorLabelRejection::PlayInProgress)
            }
            Some("transaction_unavailable") => {
                ActorLabelOutcome::NotExecuted(ActorLabelRejection::TransactionUnavailable)
            }
            _ => ActorLabelOutcome::OutcomeUnknown,
        };
    }
    if read_path(receipt).ok() != Some(prepared.actor_path.as_str()) {
        return ActorLabelOutcome::OutcomeUnknown;
    }
    let Ok(actual_label) = read_label(receipt, "actual_label") else {
        return ActorLabelOutcome::OutcomeUnknown;
    };
    match receipt["status"].as_str() {
        Some("applied") if actual_label == prepared.new_label => ActorLabelOutcome::Applied {
            actual_label: actual_label.to_owned(),
        },
        Some("readback_mismatch") if actual_label != prepared.new_label => {
            ActorLabelOutcome::ReadbackMismatch {
                actual_label: actual_label.to_owned(),
            }
        }
        _ => ActorLabelOutcome::OutcomeUnknown,
    }
}
