//! Approval and mutation admission shared by engine-specific write tools.
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::oneshot;
use uebox_agent_core::{
    ApprovalDecision, Error, MutationSettlement, MutationStatus, ToolApprovalFuture,
    ToolApprovalRequest, ToolApprover, ToolRegistry,
};

use crate::chat_history::RecordedTurn;
use uebox_chat_history::ActorLabelStatus;

const RECEIPT_LIMIT: usize = 128;

struct Pending<P> {
    generation: Arc<()>,
    owner: String,
    agent_id: String,
    proposal: P,
    deadline: Instant,
    answer: oneshot::Sender<bool>,
}

pub(crate) struct ApprovalQueue<P>(Mutex<HashMap<String, Pending<P>>>);

impl<P> Default for ApprovalQueue<P> {
    fn default() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}

impl<P: Clone> ApprovalQueue<P> {
    pub(crate) async fn ask(
        &self,
        approval_id: String,
        owner: String,
        agent_id: String,
        proposal: P,
        deadline: Instant,
    ) -> bool {
        if Instant::now() >= deadline {
            return false;
        }
        let (answer, wait) = oneshot::channel();
        let generation = Arc::new(());
        {
            let Ok(mut pending) = self.0.lock() else {
                return false;
            };
            match pending.entry(approval_id.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => return false,
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Pending {
                        generation: Arc::clone(&generation),
                        owner,
                        agent_id,
                        proposal,
                        deadline,
                        answer,
                    });
                }
            }
        }
        let _drop_guard = PendingGuard {
            queue: self,
            approval_id,
            generation,
        };
        wait.await == Ok(true)
    }

    pub(crate) fn answer(&self, owner: &str, id: &str, allowed: bool) -> Result<(), &'static str> {
        const CLOSED: &str = "approval_unavailable";
        let mut pending = self.0.lock().map_err(|_| CLOSED)?;
        let open = pending
            .get(id)
            .is_some_and(|pending| pending.owner == owner && pending.deadline > Instant::now());
        if !open {
            return Err(CLOSED);
        }
        let selected = pending.remove(id).ok_or(CLOSED)?;
        drop(pending);
        selected.answer.send(allowed).map_err(|_| CLOSED)
    }

    pub(crate) fn approvals(&self, owner: &str, agent_id: &str) -> Vec<P> {
        let now = Instant::now();
        let Ok(pending) = self.0.lock() else {
            return Vec::new();
        };
        pending
            .values()
            .filter(|item| item.owner == owner && item.agent_id == agent_id && item.deadline > now)
            .map(|item| item.proposal.clone())
            .collect()
    }

    pub(crate) fn revoke_owner(&self, owner: &str) {
        if let Ok(mut pending) = self.0.lock() {
            pending.retain(|_, item| item.owner != owner);
        }
    }
}

struct PendingGuard<'a, P> {
    queue: &'a ApprovalQueue<P>,
    approval_id: String,
    generation: Arc<()>,
}

impl<P> Drop for PendingGuard<'_, P> {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.queue.0.lock() {
            let original = pending
                .get(&self.approval_id)
                .is_some_and(|proposal| Arc::ptr_eq(&self.generation, &proposal.generation));
            if original {
                pending.remove(&self.approval_id);
            }
        }
    }
}

#[derive(Default)]
struct Projects {
    writing: HashSet<PathBuf>,
    unresolved: HashSet<PathBuf>,
}

#[derive(Default)]
pub(crate) struct WriteGate(Mutex<Projects>);

impl WriteGate {
    pub(crate) fn acquire(self: &Arc<Self>, project: &Path) -> Result<WritePermit, &'static str> {
        let mut projects = self.0.lock().map_err(|_| "project_busy")?;
        if projects.unresolved.contains(project) {
            return Err("previous_outcome_unknown");
        }
        if !projects.writing.insert(project.to_path_buf()) {
            return Err("project_busy");
        }
        Ok(WritePermit {
            gate: Arc::clone(self),
            project: project.to_path_buf(),
            uncertain: true,
        })
    }
}

pub(crate) struct WritePermit {
    gate: Arc<WriteGate>,
    project: PathBuf,
    uncertain: bool,
}

impl WritePermit {
    pub(crate) fn settle(mut self, uncertain: bool) {
        self.uncertain = uncertain;
    }
}

impl Drop for WritePermit {
    fn drop(&mut self) {
        if let Ok(mut projects) = self.gate.0.lock() {
            projects.writing.remove(&self.project);
            if self.uncertain {
                projects.unresolved.insert(self.project.clone());
            }
        }
    }
}

pub(crate) struct NameWrites<T> {
    pending: ApprovalQueue<Proposal<T>>,
    writes: Arc<WriteGate>,
    receipts: Mutex<VecDeque<(String, Receipt)>>,
    recordings: Recordings,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Proposal<T> {
    approval_id: String,
    agent_id: String,
    run_id: u64,
    #[serde(flatten)]
    change: T,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Receipt {
    approval_id: String,
    agent_id: String,
    run_id: u64,
    status: &'static str,
    actual_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    history_error: Option<&'static str>,
}

impl<T> Default for NameWrites<T> {
    fn default() -> Self {
        Self {
            pending: ApprovalQueue::default(),
            writes: Default::default(),
            receipts: Default::default(),
            recordings: Default::default(),
        }
    }
}

impl<T> NameWrites<T>
where
    T: Clone + DeserializeOwned + Send + Sync + 'static,
{
    pub(crate) fn install(
        self: &Arc<Self>,
        registry: ToolRegistry,
        owner: String,
        agent_id: String,
        timeout: Duration,
    ) -> Result<ToolRegistry, Error> {
        let state = Arc::clone(self);
        let approver = Arc::new(NameApprover {
            state: Arc::clone(self),
            owner: owner.clone(),
            agent_id: agent_id.clone(),
        });
        registry.with_mutation_approval(
            approver,
            timeout,
            Arc::new(move |outcome| state.record(owner.clone(), agent_id.clone(), outcome)),
        )
    }

    pub(crate) fn write_gate(&self) -> Arc<WriteGate> {
        Arc::clone(&self.writes)
    }

    pub(crate) fn approvals(&self, owner: &str, agent_id: &str) -> Vec<Proposal<T>> {
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

    pub(crate) fn record_run(
        &self,
        agent_id: String,
        run_id: u64,
        turn: RecordedTurn,
    ) -> NameRecording {
        let key = (agent_id, run_id);
        self.recordings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(key.clone(), turn);
        NameRecording {
            recordings: Arc::clone(&self.recordings),
            key,
        }
    }

    fn record(&self, owner: String, agent_id: String, settlement: MutationSettlement) {
        let run_id = settlement.invocation.run_id;
        let recording = self
            .recordings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&(agent_id.clone(), run_id))
            .cloned();
        let status = match settlement.receipt.status {
            MutationStatus::Applied => ActorLabelStatus::Applied,
            MutationStatus::Mismatch => ActorLabelStatus::Mismatch,
            MutationStatus::NotExecuted => ActorLabelStatus::NotExecuted,
            MutationStatus::Unknown => ActorLabelStatus::Unknown,
        };
        let mut receipt = Receipt {
            approval_id: settlement.invocation.approval_id.to_string(),
            agent_id,
            run_id,
            status: status.as_str(),
            actual_name: settlement.receipt.data["actualName"]
                .as_str()
                .map(str::to_owned),
            history_error: None,
        };
        if let Some(recording) = recording {
            receipt.history_error = recording
                .record_node_name(&receipt.approval_id, status, receipt.actual_name.as_deref())
                .err();
        }
        if let Ok(mut receipts) = self.receipts.lock() {
            if receipts.len() >= RECEIPT_LIMIT {
                receipts.pop_front();
            }
            receipts.push_back((owner, receipt));
        }
    }
}

type RunKey = (String, u64);
type Recordings = Arc<Mutex<HashMap<RunKey, RecordedTurn>>>;

pub(crate) struct NameRecording {
    recordings: Recordings,
    key: RunKey,
}

impl Drop for NameRecording {
    fn drop(&mut self) {
        self.recordings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.key);
    }
}

struct NameApprover<T> {
    state: Arc<NameWrites<T>>,
    owner: String,
    agent_id: String,
}

impl<T> ToolApprover for NameApprover<T>
where
    T: Clone + DeserializeOwned + Send + Sync + 'static,
{
    fn approve(&self, request: ToolApprovalRequest) -> ToolApprovalFuture {
        let state = Arc::clone(&self.state);
        let owner = self.owner.clone();
        let agent_id = self.agent_id.clone();
        Box::pin(async move {
            let Ok(change) = serde_json::from_value::<T>(request.preview) else {
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
