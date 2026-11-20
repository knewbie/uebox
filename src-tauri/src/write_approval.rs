//! Approval and mutation admission shared by engine-specific write tools.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::oneshot;

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
