//! In-process, explicitly configured agent and provider primitives.

mod mutation;
mod provider;
mod runtime;
mod schema;
pub use mutation::{
    ApprovalDecision, MutationExecuteFuture, MutationInvocation, MutationPrepareFuture,
    MutationReceipt, MutationSettlement, MutationSettlementHook, MutationSkipReason,
    MutationStatus, MutationTool, PreparedMutation, ToolApprovalFuture, ToolApprovalRequest,
    ToolApprover,
};
pub use provider::{
    AssistantMessage, Cancellation, Error, Message, ModelEvent, OpenAiProvider, ProviderConfig,
    ToolCall,
};
pub use runtime::{
    AgentSession, Engine, ReadOnlyTool, Run, RunOutcome, RunPhase, RunTarget, SessionSnapshot,
    ToolDefinition, ToolError, ToolFuture, ToolRegistry,
};
