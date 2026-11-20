use serde::Serialize;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};
use tokio::sync::oneshot;

use crate::chat_history::RecordedTurn;

const RECEIPT_LIMIT: usize = 128;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Proposal {
    pub approval_id: String,
    pub agent_id: String,
    pub run_id: u64,
    pub actor_path: String,
    pub old_label: String,
    pub new_label: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Receipt {
    pub approval_id: String,
    pub agent_id: String,
    pub run_id: u64,
    pub status: &'static str,
    pub actual_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_error: Option<&'static str>,
}

struct Pending {
    owner: String,
    proposal: Proposal,
    deadline: Instant,
    answer: oneshot::Sender<bool>,
}

#[derive(Default)]
struct State {
    pending: HashMap<String, Pending>,
    receipts: VecDeque<(String, Receipt)>,
    writing: HashSet<PathBuf>,
    unresolved: HashSet<PathBuf>,
    recordings: HashMap<(String, u64), RecordedTurn>,
}

#[derive(Default)]
pub(crate) struct ApprovalBook(Mutex<State>);

impl ApprovalBook {
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) async fn ask(
        self: &Arc<Self>,
        owner: String,
        proposal: Proposal,
        deadline: Instant,
    ) -> bool {
        if Instant::now() >= deadline {
            return false;
        }
        let approval_id = proposal.approval_id.clone();
        let (answer, wait) = oneshot::channel();
        {
            let mut state = self.state();
            if state.pending.contains_key(&approval_id) {
                return false;
            }
            state.pending.insert(
                approval_id.clone(),
                Pending {
                    owner,
                    proposal,
                    deadline,
                    answer,
                },
            );
        }
        let _pending = PendingGuard {
            book: self,
            approval_id,
        };
        wait.await == Ok(true)
    }

    pub(crate) fn answer(&self, owner: &str, id: &str, allowed: bool) -> Result<(), &'static str> {
        let pending = {
            let mut state = self.state();
            let open = state
                .pending
                .get(id)
                .is_some_and(|pending| pending.owner == owner && pending.deadline > Instant::now());
            if !open {
                return Err("approval_unavailable");
            }
            state.pending.remove(id).ok_or("approval_unavailable")?
        };
        pending
            .answer
            .send(allowed)
            .map_err(|_| "approval_unavailable")
    }

    pub(crate) fn approvals(&self, owner: &str, agent_id: &str) -> Vec<Proposal> {
        let now = Instant::now();
        self.state()
            .pending
            .values()
            .filter(|pending| {
                pending.owner == owner
                    && pending.proposal.agent_id == agent_id
                    && pending.deadline > now
            })
            .map(|pending| pending.proposal.clone())
            .collect()
    }

    pub(crate) fn record(&self, owner: String, mut receipt: Receipt) {
        let recording = self
            .state()
            .recordings
            .get(&(receipt.agent_id.clone(), receipt.run_id))
            .cloned();
        if let Some(recording) = recording {
            receipt.history_error = recording
                .record_actor_label(
                    &receipt.approval_id,
                    receipt.status,
                    receipt.actual_label.as_deref(),
                )
                .err();
        }
        let mut state = self.state();
        if state.receipts.len() >= RECEIPT_LIMIT {
            state.receipts.pop_front();
        }
        state.receipts.push_back((owner, receipt));
    }

    pub(crate) fn record_run(
        self: &Arc<Self>,
        agent_id: String,
        run_id: u64,
        turn: RecordedTurn,
    ) -> RecordedRun {
        let key = (agent_id, run_id);
        self.state().recordings.insert(key.clone(), turn);
        RecordedRun {
            book: Arc::clone(self),
            key,
        }
    }

    pub(crate) fn receipts(&self, owner: &str) -> Vec<Receipt> {
        self.state()
            .receipts
            .iter()
            .rev()
            .filter(|(receipt_owner, _)| receipt_owner == owner)
            .map(|(_, receipt)| receipt.clone())
            .collect()
    }

    pub(crate) fn revoke_owner(&self, owner: &str) {
        let mut state = self.state();
        state.pending.retain(|_, pending| pending.owner != owner);
        state
            .receipts
            .retain(|(receipt_owner, _)| receipt_owner != owner);
    }

    pub(crate) fn try_begin_write(
        self: &Arc<Self>,
        project: &Path,
    ) -> Result<WritePermit, &'static str> {
        let mut state = self.state();
        if state.unresolved.contains(project) {
            return Err("previous_outcome_unknown");
        }
        if !state.writing.insert(project.to_path_buf()) {
            return Err("project_busy");
        }
        Ok(WritePermit {
            book: Arc::clone(self),
            project: project.to_path_buf(),
            uncertain: true,
        })
    }
}

pub(crate) struct RecordedRun {
    book: Arc<ApprovalBook>,
    key: (String, u64),
}

impl Drop for RecordedRun {
    fn drop(&mut self) {
        self.book.state().recordings.remove(&self.key);
    }
}

struct PendingGuard<'a> {
    book: &'a ApprovalBook,
    approval_id: String,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.book.state().pending.remove(&self.approval_id);
    }
}

pub(crate) struct WritePermit {
    book: Arc<ApprovalBook>,
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
        let mut state = self.book.state();
        state.writing.remove(&self.project);
        if self.uncertain {
            state.unresolved.insert(self.project.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn proposal(id: &str) -> Proposal {
        Proposal {
            approval_id: id.into(),
            agent_id: "agent".into(),
            run_id: 1,
            actor_path: "/Game/Test.Actor".into(),
            old_label: "Old".into(),
            new_label: "New".into(),
        }
    }

    #[tokio::test]
    async fn approval_is_single_use_and_requires_the_original_window() {
        let book = Arc::new(ApprovalBook::default());
        let ask = book.ask(
            "main".into(),
            proposal("request"),
            Instant::now() + Duration::from_secs(1),
        );
        tokio::pin!(ask);
        assert!(futures_util::poll!(&mut ask).is_pending());
        assert!(book.approvals("other", "agent").is_empty());
        assert_eq!(book.approvals("main", "agent").len(), 1);
        assert!(book.answer("other", "request", true).is_err());
        assert!(book.answer("main", "request", true).is_ok());
        assert!(book.answer("main", "request", true).is_err());
        assert!(ask.await);
    }

    #[tokio::test]
    async fn dropping_or_expiring_the_wait_removes_the_proposal() {
        let book = Arc::new(ApprovalBook::default());
        let mut ask = Box::pin(book.ask(
            "main".into(),
            proposal("cancelled"),
            Instant::now() + Duration::from_secs(1),
        ));
        assert!(futures_util::poll!(&mut ask).is_pending());
        drop(ask);
        assert!(book.answer("main", "cancelled", true).is_err());

        assert!(
            !book
                .ask("main".into(), proposal("expired"), Instant::now())
                .await
        );
        assert!(book.approvals("main", "agent").is_empty());
    }

    #[tokio::test]
    async fn destroying_a_window_revokes_its_pending_approval() {
        let book = Arc::new(ApprovalBook::default());
        let ask = book.ask(
            "main".into(),
            proposal("previous-window"),
            Instant::now() + Duration::from_secs(1),
        );
        tokio::pin!(ask);
        assert!(futures_util::poll!(&mut ask).is_pending());
        book.revoke_owner("main");
        assert!(book.approvals("main", "agent").is_empty());
        assert!(book.answer("main", "previous-window", true).is_err());
        assert!(!ask.await);
    }

    #[test]
    fn project_write_permit_excludes_concurrent_writes_and_remembers_uncertainty() {
        let book = Arc::new(ApprovalBook::default());
        let project = Path::new("project-a");
        let permit = book.try_begin_write(project).unwrap();
        assert!(matches!(book.try_begin_write(project), Err("project_busy")));
        book.try_begin_write(Path::new("project-b"))
            .unwrap()
            .settle(false);
        permit.settle(false);
        book.try_begin_write(project).unwrap().settle(true);
        assert!(matches!(
            book.try_begin_write(project),
            Err("previous_outcome_unknown")
        ));
    }
}
