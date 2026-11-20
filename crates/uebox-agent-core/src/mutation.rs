use crate::{Error, RunTarget, ToolError};
use serde_json::{json, Value};
use std::{
    future::Future,
    panic::{catch_unwind, AssertUnwindSafe},
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub type MutationPrepareFuture =
    Pin<Box<dyn Future<Output = Result<PreparedMutation, ToolError>> + Send>>;
pub type MutationExecuteFuture = Pin<Box<dyn Future<Output = MutationReceipt> + Send>>;
pub type ToolApprovalFuture = Pin<Box<dyn Future<Output = ApprovalDecision> + Send>>;
pub type MutationSettlementHook = Arc<dyn Fn(MutationSettlement) + Send + Sync>;

/// Preparation is read-only. The adapter must capture the exact operation and
/// target in its prepared value and revalidate its transport before writing.
pub trait MutationTool: Send + Sync {
    fn prepare(&self, target: RunTarget, arguments: Value) -> MutationPrepareFuture;
}

/// A preview and a single-use edit. The edit future must stay pending until any
/// underlying engine operation finishes and must report the observed result.
pub struct PreparedMutation {
    pub(crate) title: String,
    pub(crate) preview: Value,
    pub(crate) execute: Box<dyn FnOnce() -> MutationExecuteFuture + Send>,
}

impl PreparedMutation {
    pub fn new(
        title: impl Into<String>,
        preview: Value,
        execute: impl FnOnce() -> MutationExecuteFuture + Send + 'static,
    ) -> Self {
        Self {
            title: title.into(),
            preview,
            execute: Box::new(execute),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutationInvocation {
    pub approval_id: u64,
    pub run_id: u64,
    pub tool_call_id: String,
    pub tool_name: String,
    pub target: RunTarget,
}

pub struct ToolApprovalRequest {
    pub invocation: MutationInvocation,
    pub title: String,
    pub preview: Value,
    pub deadline: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approve,
    Deny,
}

/// Return promptly with a future bound to this request. Dropping the future
/// withdraws that request. Approval must describe this exact prepared operation.
pub trait ToolApprover: Send + Sync {
    fn approve(&self, request: ToolApprovalRequest) -> ToolApprovalFuture;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationStatus {
    Applied,
    Mismatch,
    NotExecuted,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutationReceipt {
    pub status: MutationStatus,
    /// Model-safe readback supplied by the adapter. Do not echo the preview.
    pub data: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationSkipReason {
    PrepareFailed,
    Denied,
    Expired,
    Cancelled,
    TargetChanged,
    Abandoned,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutationSettlement {
    pub invocation: MutationInvocation,
    pub receipt: MutationReceipt,
    pub reason: Option<MutationSkipReason>,
}

pub(crate) struct MutationPolicy {
    pub approver: Arc<dyn ToolApprover>,
    pub timeout: Duration,
    pub on_settled: MutationSettlementHook,
}

pub(crate) struct MutationAttempt {
    invocation: Option<MutationInvocation>,
    dispatched: bool,
    on_settled: MutationSettlementHook,
}

impl MutationAttempt {
    pub fn new(
        run_id: u64,
        tool_call_id: &str,
        tool_name: &str,
        target: RunTarget,
        on_settled: MutationSettlementHook,
    ) -> Result<Self, Error> {
        static LAST_APPROVAL_ID: Mutex<u64> = Mutex::new(0);
        let approval_id = {
            let mut id = LAST_APPROVAL_ID.lock().map_err(|_| Error::InvalidConfig)?;
            *id = id.checked_add(1).ok_or(Error::InvalidConfig)?;
            *id
        };
        Ok(Self {
            invocation: Some(MutationInvocation {
                approval_id,
                run_id,
                tool_call_id: tool_call_id.into(),
                tool_name: tool_name.into(),
                target,
            }),
            dispatched: false,
            on_settled,
        })
    }

    pub fn invocation(&self) -> &MutationInvocation {
        self.invocation.as_ref().expect("unsettled mutation")
    }

    pub fn mark_dispatched(&mut self) {
        self.dispatched = true;
    }

    pub fn skip(mut self, reason: MutationSkipReason) -> Result<(Value, bool), Error> {
        let receipt = MutationReceipt::without_data(MutationStatus::NotExecuted);
        self.publish(receipt.clone(), Some(reason));
        let code = match reason {
            MutationSkipReason::PrepareFailed => "mutation_prepare_failed",
            MutationSkipReason::Denied => "approval_denied",
            MutationSkipReason::Expired => "approval_expired",
            MutationSkipReason::Cancelled => return Err(Error::Cancelled),
            MutationSkipReason::TargetChanged => return Err(Error::TargetChanged),
            MutationSkipReason::Abandoned => "mutation_abandoned",
        };
        let (mut result, is_error) = receipt.wire();
        result["error"] = json!(code);
        Ok((result, is_error))
    }

    pub fn finish(mut self, receipt: MutationReceipt) -> (Value, bool) {
        self.publish(receipt.clone(), None);
        receipt.wire()
    }

    fn publish(&mut self, receipt: MutationReceipt, reason: Option<MutationSkipReason>) {
        if let Some(invocation) = self.invocation.take() {
            // A host callback that unwinds cannot trigger a second settlement or block cleanup.
            let _ = catch_unwind(AssertUnwindSafe(|| {
                (self.on_settled)(MutationSettlement {
                    invocation,
                    receipt,
                    reason,
                });
            }));
        }
    }
}

impl Drop for MutationAttempt {
    fn drop(&mut self) {
        if self.dispatched {
            self.publish(MutationReceipt::without_data(MutationStatus::Unknown), None);
        } else {
            self.publish(
                MutationReceipt::without_data(MutationStatus::NotExecuted),
                Some(MutationSkipReason::Abandoned),
            );
        }
    }
}

impl MutationReceipt {
    pub(crate) fn without_data(status: MutationStatus) -> Self {
        Self {
            status,
            data: Value::Null,
        }
    }

    pub(crate) fn wire(self) -> (Value, bool) {
        let status = match self.status {
            MutationStatus::Applied => "applied",
            MutationStatus::Mismatch => "mismatch",
            MutationStatus::NotExecuted => "not_executed",
            MutationStatus::Unknown => "unknown",
        };
        let applied = self.status == MutationStatus::Applied;
        (
            json!({"ok":applied,"status":status,"data":self.data}),
            !applied,
        )
    }
}
