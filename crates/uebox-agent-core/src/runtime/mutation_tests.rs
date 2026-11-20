use super::*;
use crate::{MutationPrepareFuture, MutationSettlement, ProviderConfig, ToolApprovalFuture};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::{mpsc, oneshot, Notify};

struct Broker(mpsc::UnboundedSender<oneshot::Sender<ApprovalDecision>>);

impl ToolApprover for Broker {
    fn approve(&self, _: ToolApprovalRequest) -> ToolApprovalFuture {
        let (reply, answer) = oneshot::channel();
        assert!(self.0.send(reply).is_ok());
        Box::pin(async move { answer.await.unwrap_or(ApprovalDecision::Deny) })
    }
}

struct Edit {
    count: Arc<AtomicUsize>,
    started: Arc<Notify>,
    completion: Mutex<Option<oneshot::Receiver<MutationReceipt>>>,
}

impl MutationTool for Edit {
    fn prepare(&self, _: RunTarget, _: Value) -> MutationPrepareFuture {
        let count = self.count.clone();
        let started = self.started.clone();
        let completion = self.completion.lock().unwrap().take();
        Box::pin(async move {
            Ok(PreparedMutation::new(
                "Set label",
                json!({"from":"a","to":"b"}),
                move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    started.notify_one();
                    Box::pin(async move {
                        match completion {
                            Some(completion) => completion.await.unwrap(),
                            None => MutationReceipt::without_data(MutationStatus::Applied),
                        }
                    })
                },
            ))
        })
    }
}

struct Fixture {
    agent: AgentSession,
    approvals: mpsc::UnboundedReceiver<oneshot::Sender<ApprovalDecision>>,
    settlements: mpsc::UnboundedReceiver<MutationSettlement>,
    current: Arc<AtomicBool>,
    edit: Arc<Edit>,
}

impl Fixture {
    fn new(
        instance: &str,
        timeout: Duration,
        edit: Option<oneshot::Receiver<MutationReceipt>>,
    ) -> Self {
        let (settlement_send, settlements) = mpsc::unbounded_channel();
        let (approval_send, approvals) = mpsc::unbounded_channel();
        let target = RunTarget::new(Engine::Unreal, "project", instance, 5).unwrap();
        let mut registry = ToolRegistry::new(target)
            .with_mutation_approval(
                Arc::new(Broker(approval_send)),
                timeout,
                Arc::new(move |event| settlement_send.send(event).unwrap()),
            )
            .unwrap();
        let edit = Arc::new(Edit {
            count: Arc::new(AtomicUsize::new(0)),
            started: Arc::new(Notify::new()),
            completion: Mutex::new(edit),
        });
        registry
            .register_mutation(
                Engine::Unreal,
                ToolDefinition::new(
                    "set_label",
                    "Set label",
                    json!({"type":"object","properties":{},"additionalProperties":false}),
                ),
                edit.clone(),
            )
            .unwrap();
        let current = Arc::new(AtomicBool::new(true));
        let validator = current.clone();
        // Exercise the dispatch state machine without issuing a provider request.
        let provider =
            OpenAiProvider::new(ProviderConfig::new("http://127.0.0.1:9/fixture", "fixture"))
                .unwrap();
        let agent = AgentSession::new(
            provider,
            registry,
            Arc::new(move |_| validator.load(Ordering::SeqCst)),
            2,
        )
        .unwrap();
        Self {
            agent,
            approvals,
            settlements,
            current,
            edit,
        }
    }

    fn start(&self) -> (Cancellation, DispatchTask) {
        let run = self.agent.start("Change").unwrap();
        let cancel = run.cancellation();
        let task = tokio::spawn(async move {
            run.dispatch_tool(&mock_model_call(), &mut HashSet::new())
                .await
        });
        (cancel, task)
    }
}

type DispatchTask = tokio::task::JoinHandle<Result<(Value, bool), Error>>;

fn mock_model_call() -> ToolCall {
    ToolCall {
        id: "tool-1".into(),
        name: "set_label".into(),
        arguments: "{}".into(),
    }
}

#[tokio::test]
async fn approved_model_call_dispatches_once_and_a_denied_call_never_dispatches() {
    for (decision, expected, count) in [
        (ApprovalDecision::Approve, "applied", 1),
        (ApprovalDecision::Deny, "not_executed", 0),
    ] {
        let mut fixture = Fixture::new("model-call-once", Duration::from_secs(2), None);
        let run = fixture.agent.start("Edit").unwrap();
        let task = tokio::spawn(async move {
            let mut seen = HashSet::new();
            let call = mock_model_call();
            let first = run.dispatch_tool(&call, &mut seen).await.unwrap();
            let duplicate = run.dispatch_tool(&call, &mut seen).await.unwrap();
            (first, duplicate)
        });
        fixture
            .approvals
            .recv()
            .await
            .unwrap()
            .send(decision)
            .unwrap();
        let ((first, _), (duplicate, _)) = task.await.unwrap();
        assert_eq!(first["status"], expected);
        assert_eq!(duplicate["error"], "duplicate_tool_call");
        assert_eq!(fixture.edit.count.load(Ordering::SeqCst), count);
        assert_eq!(
            fixture
                .settlements
                .recv()
                .await
                .unwrap()
                .invocation
                .tool_call_id,
            "tool-1"
        );
        assert!(fixture.settlements.try_recv().is_err());
        assert!(fixture.approvals.try_recv().is_err());
    }
}

#[tokio::test]
async fn approval_does_not_hold_the_instance_gate_and_expires_before_an_admitted_dispatch() {
    let mut fixture = Fixture::new("expiry-before-dispatch", Duration::from_millis(150), None);
    let (_cancel, task) = fixture.start();
    let approve = fixture.approvals.recv().await.unwrap();
    let permit = fixture
        .agent
        .inner
        .registry
        .gate
        .try_lock()
        .expect("approval must not hold the instance gate");
    approve.send(ApprovalDecision::Approve).unwrap();
    let (result, is_error) = task.await.unwrap().unwrap();
    assert!(is_error);
    assert_eq!(result["error"], "approval_expired");
    let settlement = fixture.settlements.recv().await.unwrap();
    assert_eq!(settlement.reason, Some(MutationSkipReason::Expired));
    assert_eq!(fixture.edit.count.load(Ordering::SeqCst), 0);
    drop(permit);
}

#[tokio::test]
async fn cancellation_or_target_change_while_an_approved_mutation_queues_never_dispatches() {
    for (instance, expected, reason) in [
        (
            "approved-then-cancelled",
            Error::Cancelled,
            MutationSkipReason::Cancelled,
        ),
        (
            "approved-then-stale",
            Error::TargetChanged,
            MutationSkipReason::TargetChanged,
        ),
    ] {
        let mut fixture = Fixture::new(instance, Duration::from_secs(2), None);
        let (cancel, task) = fixture.start();
        let approve = fixture.approvals.recv().await.unwrap();
        let permit = fixture.agent.inner.registry.gate.try_lock().unwrap();
        approve.send(ApprovalDecision::Approve).unwrap();
        time::timeout(Duration::from_secs(1), async {
            while fixture.agent.snapshot().phase != Some(RunPhase::RunningTools) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if expected == Error::Cancelled {
            cancel.cancel();
        } else {
            fixture.current.store(false, Ordering::SeqCst);
        }
        assert_eq!(task.await.unwrap().unwrap_err(), expected);
        let settlement = fixture.settlements.recv().await.unwrap();
        assert_eq!(settlement.receipt.status, MutationStatus::NotExecuted);
        assert_eq!(settlement.reason, Some(reason));
        assert_eq!(fixture.edit.count.load(Ordering::SeqCst), 0);
        drop(permit);
    }
}

#[tokio::test]
async fn dropping_the_run_does_not_drop_the_dispatched_mutation_or_its_receipt() {
    let (finish, edit) = oneshot::channel();
    let mut fixture = Fixture::new("drop-after-dispatch", Duration::from_secs(2), Some(edit));
    let (_cancel, task) = fixture.start();
    fixture
        .approvals
        .recv()
        .await
        .unwrap()
        .send(ApprovalDecision::Approve)
        .unwrap();
    fixture.edit.started.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(fixture.agent.snapshot().phase, Some(RunPhase::Draining));
    assert!(fixture.agent.inner.registry.gate.try_lock().is_err());
    let receipt = MutationReceipt {
        status: MutationStatus::Applied,
        data: json!({"observed":"b"}),
    };
    finish.send(receipt.clone()).unwrap();
    assert_eq!(fixture.settlements.recv().await.unwrap().receipt, receipt);
    let _permit = fixture.agent.inner.registry.gate.lock().await;
    assert!(fixture.agent.snapshot().run_id.is_none());
    assert_eq!(fixture.edit.count.load(Ordering::SeqCst), 1);
    assert!(fixture.settlements.try_recv().is_err());
}

#[tokio::test]
async fn a_panicking_dispatched_edit_settles_unknown_once() {
    let (finish, edit) = oneshot::channel();
    let mut fixture = Fixture::new("panic-after-dispatch", Duration::from_secs(2), Some(edit));
    let (_cancel, task) = fixture.start();
    fixture
        .approvals
        .recv()
        .await
        .unwrap()
        .send(ApprovalDecision::Approve)
        .unwrap();
    fixture.edit.started.notified().await;
    drop(finish); // The fixture adapter unwinds after dispatch.
    let (result, is_error) = task.await.unwrap().unwrap();
    assert!(is_error);
    assert_eq!(result["status"], "unknown");
    assert_eq!(
        fixture.settlements.recv().await.unwrap().receipt.status,
        MutationStatus::Unknown
    );
    assert!(fixture.settlements.try_recv().is_err());
}
