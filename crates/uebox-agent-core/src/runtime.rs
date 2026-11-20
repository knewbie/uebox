use crate::mutation::{MutationAttempt, MutationPolicy};
use crate::{
    ApprovalDecision, AssistantMessage, Cancellation, Error, Message, ModelEvent, MutationReceipt,
    MutationSettlementHook, MutationSkipReason, MutationStatus, MutationTool, OpenAiProvider,
    PreparedMutation, ToolApprovalRequest, ToolApprover, ToolCall,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};
use tokio::time::{self, Instant};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Engine {
    Unreal,
    Unity,
    Godot,
}

/// Immutable identity captured by the host for the lifetime of this session.
/// `instance` must be stable across generations of the same physical editor if
/// its tools must share the process-local gate across reconnects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunTarget {
    engine: Engine,
    project: String,
    instance: String,
    generation: u64,
}
impl RunTarget {
    pub fn new(
        engine: Engine,
        project: impl Into<String>,
        instance: impl Into<String>,
        generation: u64,
    ) -> Result<Self, Error> {
        let project = project.into();
        let instance = instance.into();
        if project.trim().is_empty() || instance.trim().is_empty() || generation == 0 {
            return Err(Error::InvalidConfig);
        }
        Ok(Self {
            engine,
            project,
            instance,
            generation,
        })
    }
    pub fn engine(&self) -> Engine {
        self.engine
    }
    pub fn project(&self) -> &str {
        &self.project
    }
    pub fn instance(&self) -> &str {
        &self.instance
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    fn as_value(&self) -> Value {
        json!({"engine":match self.engine { Engine::Unreal => "unreal", Engine::Unity => "unity", Engine::Godot => "godot" },
            "project":self.project, "instance":self.instance, "generation":self.generation})
    }
}

pub struct ToolDefinition {
    name: String,
    description: String,
    schema: Value,
}
impl ToolDefinition {
    pub fn new(name: impl Into<String>, description: impl Into<String>, schema: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema,
        }
    }
}

/// The host must inject only genuinely read-only implementations. `execute`
/// must return promptly and dispatch solely to the supplied target, rechecking
/// its connection generation inside the adapter at dispatch time.
#[derive(Debug)]
pub struct ToolError;
pub type ToolFuture = Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send>>;
pub trait ReadOnlyTool: Send + Sync {
    fn execute(&self, target: RunTarget, arguments: Value) -> ToolFuture;
}

struct DenyExternalSchemas;
impl jsonschema::Retrieve for DenyExternalSchemas {
    fn retrieve(
        &self,
        _: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err("external schema retrieval is disabled".into())
    }
}

struct RegisteredTool {
    definition: ToolDefinition,
    validator: jsonschema::Validator,
    executor: ToolExecutor,
}

enum ToolExecutor {
    ReadOnly(Arc<dyn ReadOnlyTool>),
    Mutation(Arc<dyn MutationTool>),
}
type InstanceKey = (Engine, String);
type InstanceGate = tokio::sync::Mutex<()>;
static INSTANCE_GATES: OnceLock<Mutex<HashMap<InstanceKey, Weak<InstanceGate>>>> = OnceLock::new();

fn instance_gate(target: &RunTarget) -> Arc<InstanceGate> {
    let mut gates = INSTANCE_GATES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    gates.retain(|_, gate| gate.strong_count() != 0);
    let key = (target.engine, target.instance.clone());
    if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(InstanceGate::new(()));
    gates.insert(key, Arc::downgrade(&gate));
    gate
}

pub struct ToolRegistry {
    target: RunTarget,
    tools: BTreeMap<String, RegisteredTool>,
    gate: Arc<InstanceGate>,
    mutation_policy: Option<MutationPolicy>,
}
impl ToolRegistry {
    pub fn new(target: RunTarget) -> Self {
        let gate = instance_gate(&target);
        Self {
            target,
            tools: BTreeMap::new(),
            gate,
            mutation_policy: None,
        }
    }
    pub fn register(
        &mut self,
        engine: Engine,
        definition: ToolDefinition,
        tool: Arc<dyn ReadOnlyTool>,
    ) -> Result<(), Error> {
        self.register_tool(engine, definition, ToolExecutor::ReadOnly(tool))
    }

    pub fn register_mutation(
        &mut self,
        engine: Engine,
        definition: ToolDefinition,
        tool: Arc<dyn MutationTool>,
    ) -> Result<(), Error> {
        self.register_tool(engine, definition, ToolExecutor::Mutation(tool))
    }

    /// Configure approvals and a nonblocking host hook for final receipts.
    /// The timeout includes waiting to reacquire the instance gate.
    pub fn with_mutation_approval(
        mut self,
        approver: Arc<dyn ToolApprover>,
        timeout: Duration,
        on_settled: MutationSettlementHook,
    ) -> Result<Self, Error> {
        if timeout.is_zero() || std::time::Instant::now().checked_add(timeout).is_none() {
            return Err(Error::InvalidConfig);
        }
        self.mutation_policy = Some(MutationPolicy {
            approver,
            timeout,
            on_settled,
        });
        Ok(self)
    }

    fn register_tool(
        &mut self,
        engine: Engine,
        definition: ToolDefinition,
        executor: ToolExecutor,
    ) -> Result<(), Error> {
        if engine != self.target.engine {
            return Err(Error::WrongEngine);
        }
        let valid_name = !definition.name.is_empty()
            && definition.name.len() <= 64
            && definition
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !valid_name
            || !crate::schema::supported(&definition.schema)
            || self.tools.contains_key(&definition.name)
            || definition.schema.get("type") != Some(&json!("object"))
            || definition.schema.get("additionalProperties") != Some(&json!(false))
        {
            return Err(Error::InvalidTool);
        }
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft7)
            .with_retriever(DenyExternalSchemas)
            .should_validate_formats(true)
            .should_ignore_unknown_formats(false)
            .build(&definition.schema)
            .map_err(|_| Error::InvalidTool)?;
        self.tools.insert(
            definition.name.clone(),
            RegisteredTool {
                definition,
                validator,
                executor,
            },
        );
        Ok(())
    }
    fn declarations(&self) -> Vec<Value> {
        self.tools.values().map(|entry| json!({"type":"function","function":{"name":entry.definition.name,"description":entry.definition.description,"parameters":entry.definition.schema}})).collect()
    }
}

type TargetValidator = dyn Fn(&RunTarget) -> bool + Send + Sync;
struct SessionState {
    messages: Vec<Message>,
    active: Option<u64>,
    next_run: u64,
    phase: Option<RunPhase>,
    last_outcome: Option<RunOutcome>,
    partial_text: String,
    pending_tool: bool,
}
struct SessionInner {
    provider: OpenAiProvider,
    registry: ToolRegistry,
    validator: Arc<TargetValidator>,
    max_steps: usize,
    system_prompt: Option<String>,
    state: Mutex<SessionState>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPhase {
    Preparing,
    RunningModel,
    ValidatingTools,
    RunningTools,
    Draining,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Completed,
    Failed(Error),
    Abandoned,
}
#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub target: RunTarget,
    pub run_id: Option<u64>,
    pub phase: Option<RunPhase>,
    pub messages: Vec<Message>,
    pub last_outcome: Option<RunOutcome>,
    pub partial_text: String,
}
pub struct AgentSession {
    inner: Arc<SessionInner>,
}
impl AgentSession {
    pub fn new(
        provider: OpenAiProvider,
        registry: ToolRegistry,
        validator: Arc<TargetValidator>,
        max_steps: usize,
    ) -> Result<Self, Error> {
        if max_steps == 0
            || (registry.mutation_policy.is_none()
                && registry
                    .tools
                    .values()
                    .any(|tool| matches!(tool.executor, ToolExecutor::Mutation(_))))
        {
            return Err(Error::InvalidConfig);
        }
        Ok(Self {
            inner: Arc::new(SessionInner {
                provider,
                registry,
                validator,
                max_steps,
                system_prompt: None,
                state: Mutex::new(SessionState {
                    messages: Vec::new(),
                    active: None,
                    next_run: 0,
                    phase: None,
                    last_outcome: None,
                    partial_text: String::new(),
                    pending_tool: false,
                }),
            }),
        })
    }
    pub fn new_with_system_prompt(
        provider: OpenAiProvider,
        registry: ToolRegistry,
        validator: Arc<TargetValidator>,
        max_steps: usize,
        system_prompt: impl Into<String>,
    ) -> Result<Self, Error> {
        let mut session = Self::new(provider, registry, validator, max_steps)?;
        Arc::get_mut(&mut session.inner)
            .expect("new session is uniquely owned")
            .system_prompt = Some(system_prompt.into());
        Ok(session)
    }
    pub fn start(&self, text: impl Into<String>) -> Result<Run, Error> {
        let mut state = self.inner.state.lock().unwrap();
        if state.active.is_some() {
            return Err(Error::Busy);
        }
        state.next_run = state.next_run.checked_add(1).ok_or(Error::InvalidConfig)?;
        let id = state.next_run;
        state.active = Some(id);
        state.phase = Some(RunPhase::Preparing);
        state.last_outcome = None;
        state.partial_text.clear();
        state.messages.push(Message::user(text));
        Ok(Run {
            inner: self.inner.clone(),
            id,
            cancel: Cancellation::new(),
            finished: false,
        })
    }
    pub fn snapshot(&self) -> SessionSnapshot {
        let state = self.inner.state.lock().unwrap();
        SessionSnapshot {
            target: self.inner.registry.target.clone(),
            run_id: state.active,
            phase: state.phase,
            messages: state.messages.clone(),
            last_outcome: state.last_outcome.clone(),
            partial_text: state.partial_text.clone(),
        }
    }
}

pub struct Run {
    inner: Arc<SessionInner>,
    id: u64,
    cancel: Cancellation,
    finished: bool,
}
impl Run {
    pub fn cancellation(&self) -> Cancellation {
        self.cancel.clone()
    }
    pub fn id(&self) -> u64 {
        self.id
    }
    pub async fn execute<F>(mut self, mut events: F) -> Result<AssistantMessage, Error>
    where
        F: FnMut(ModelEvent),
    {
        let result = self.cancel.settle(self.execute_inner(&mut events).await);
        {
            let mut state = self.inner.state.lock().unwrap();
            if let Ok(assistant) = &result {
                state.messages.push(Message::Assistant(assistant.clone()));
            }
            state.last_outcome = Some(match &result {
                Ok(_) => RunOutcome::Completed,
                Err(error) => RunOutcome::Failed(error.clone()),
            });
            if result.is_ok() {
                state.partial_text.clear();
            }
        }
        self.finished = true;
        result
    }
    fn set_phase(&self, phase: RunPhase) {
        self.inner.state.lock().unwrap().phase = Some(phase);
    }
    async fn execute_inner<F>(&self, events: &mut F) -> Result<AssistantMessage, Error>
    where
        F: FnMut(ModelEvent),
    {
        let mut mutation_calls = HashSet::new();
        for step in 0..self.inner.max_steps {
            if self.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if !(self.inner.validator)(&self.inner.registry.target) {
                return Err(Error::TargetChanged);
            }
            let mut messages = self.inner.state.lock().unwrap().messages.clone();
            if let Some(content) = &self.inner.system_prompt {
                messages.insert(
                    0,
                    Message::System {
                        content: content.clone(),
                    },
                );
            }
            self.inner.state.lock().unwrap().partial_text.clear();
            self.set_phase(RunPhase::RunningModel);
            let assistant = self
                .inner
                .provider
                .complete(
                    &messages,
                    &self.inner.registry.declarations(),
                    &self.cancel,
                    |event| {
                        let ModelEvent::TextDelta(ref part) = event;
                        self.inner.state.lock().unwrap().partial_text.push_str(part);
                        events(event);
                    },
                )
                .await?;
            if self.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if !(self.inner.validator)(&self.inner.registry.target) {
                return Err(Error::TargetChanged);
            }
            if self.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if assistant.tool_calls.is_empty() {
                return Ok(assistant);
            }
            if step + 1 == self.inner.max_steps {
                return Err(Error::StepLimit);
            }
            let mut results = Vec::new();
            self.set_phase(RunPhase::ValidatingTools);
            for call in &assistant.tool_calls {
                let (mut content, is_error) = self.dispatch_tool(call, &mut mutation_calls).await?;
                content["target"] = self.inner.registry.target.as_value();
                results.push(Message::Tool {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    content,
                    is_error,
                });
            }
            if self.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if !(self.inner.validator)(&self.inner.registry.target) {
                return Err(Error::TargetChanged);
            }
            if self.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            self.cancel.admit()?.deliver(|| {
                let mut state = self.inner.state.lock().unwrap();
                state.messages.push(Message::Assistant(assistant));
                state.messages.extend(results);
            });
        }
        Err(Error::StepLimit)
    }

    async fn dispatch_tool(
        &self,
        call: &ToolCall,
        mutation_calls: &mut HashSet<String>,
    ) -> Result<(Value, bool), Error> {
        self.check_target()?;
        let Some(tool) = self.inner.registry.tools.get(&call.name) else {
            return Ok((json!({"ok":false,"error":"tool_not_allowed"}), true));
        };
        let arguments = match serde_json::from_str::<Value>(&call.arguments) {
            Ok(arguments) if arguments.is_object() && tool.validator.is_valid(&arguments) => {
                arguments
            }
            _ => return Ok((json!({"ok":false,"error":"invalid_arguments"}), true)),
        };
        match &tool.executor {
            ToolExecutor::ReadOnly(executor) => {
                let executor = executor.clone();
                let result = self
                    .run_read_work(move |target| executor.execute(target, arguments))
                    .await?;
                Ok(match result {
                    Ok(value) => (json!({"ok":true,"data":value}), false),
                    Err(_) => (json!({"ok":false,"error":"tool_failed"}), true),
                })
            }
            ToolExecutor::Mutation(executor) if mutation_calls.insert(call.id.clone()) => {
                self.execute_mutation(executor.clone(), call, arguments)
                    .await
            }
            ToolExecutor::Mutation(_) => Ok((
                json!({"ok":false,"status":"not_executed","error":"duplicate_tool_call"}),
                true,
            )),
        }
    }

    fn pending(&self, permit: tokio::sync::OwnedMutexGuard<()>) -> PendingTool {
        self.inner.state.lock().unwrap().pending_tool = true;
        PendingTool {
            inner: self.inner.clone(),
            id: self.id,
            _permit: permit,
        }
    }

    fn check_target(&self) -> Result<(), Error> {
        check_current(
            &self.cancel,
            &*self.inner.validator,
            &self.inner.registry.target,
        )
    }

    // Read-only tools and mutation preparation may both perform blocking reads.
    // A detached task owns the gate until that work is really done.
    async fn run_read_work<T, F>(
        &self,
        work: impl FnOnce(RunTarget) -> F + Send + 'static,
    ) -> Result<Result<T, ToolError>, Error>
    where
        T: Send + 'static,
        F: Future<Output = Result<T, ToolError>> + Send + 'static,
    {
        self.set_phase(RunPhase::RunningTools);
        let permit = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => return Err(Error::Cancelled),
            permit = self.inner.registry.gate.clone().lock_owned() => permit,
        };
        self.check_target()?;
        let pending = self.pending(permit);
        let target = self.inner.registry.target.clone();
        let validator = self.inner.validator.clone();
        let cancel = self.cancel.clone();
        let mut task = tokio::spawn(async move {
            let _pending = pending;
            check_current(&cancel, &*validator, &target)?;
            let admitted = cancel.admit()?;
            Ok(admitted.deliver(|| work(target)).await)
        });
        let result = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => return Err(Error::Cancelled),
            result = &mut task => match result {
                Ok(result) => result?,
                Err(_) => Err(ToolError),
            },
        };
        self.check_target()?;
        self.set_phase(RunPhase::ValidatingTools);
        Ok(result)
    }

    async fn await_approval<T>(
        &self,
        deadline: Instant,
        work: impl Future<Output = T>,
    ) -> Result<T, MutationSkipReason> {
        tokio::pin!(work);
        let mut checks = time::interval(Duration::from_millis(50));
        checks.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Err(MutationSkipReason::Cancelled),
                _ = time::sleep_until(deadline) => return Err(MutationSkipReason::Expired),
                _ = checks.tick() => self.check_target().map_err(skip_reason)?,
                value = &mut work => {
                    self.check_target().map_err(skip_reason)?;
                    return Ok(value);
                }
            }
        }
    }

    async fn execute_mutation(
        &self,
        executor: Arc<dyn MutationTool>,
        call: &ToolCall,
        arguments: Value,
    ) -> Result<(Value, bool), Error> {
        let policy = self
            .inner
            .registry
            .mutation_policy
            .as_ref()
            .ok_or(Error::InvalidConfig)?;
        let target = self.inner.registry.target.clone();
        let mut attempt = MutationAttempt::new(
            self.id,
            &call.id,
            &call.name,
            target.clone(),
            policy.on_settled.clone(),
        )?;
        let PreparedMutation {
            title,
            preview,
            execute,
        } = match self
            .run_read_work(move |target| executor.prepare(target, arguments))
            .await
        {
            Ok(Ok(prepared)) if !prepared.title.trim().is_empty() => prepared,
            Ok(_) => return attempt.skip(MutationSkipReason::PrepareFailed),
            Err(error) => return attempt.skip(skip_reason(error)),
        };
        let deadline = Instant::now()
            .checked_add(policy.timeout)
            .ok_or(Error::InvalidConfig)?;
        let request = ToolApprovalRequest {
            invocation: attempt.invocation().clone(),
            title,
            preview,
            deadline: deadline.into_std(),
        };
        match self
            .await_approval(deadline, async { policy.approver.approve(request).await })
            .await
        {
            Ok(ApprovalDecision::Approve) => {}
            Ok(ApprovalDecision::Deny) => return attempt.skip(MutationSkipReason::Denied),
            Err(reason) => return attempt.skip(reason),
        }
        self.set_phase(RunPhase::RunningTools);
        let permit = match self
            .await_approval(deadline, self.inner.registry.gate.clone().lock_owned())
            .await
        {
            Ok(permit) => permit,
            Err(reason) => {
                self.set_phase(RunPhase::ValidatingTools);
                return attempt.skip(reason);
            }
        };
        let pending = self.pending(permit);
        let validator = self.inner.validator.clone();
        let cancel = self.cancel.clone();
        let mut task = tokio::spawn(async move {
            let _pending = pending;
            if let Err(error) = check_current(&cancel, &*validator, &target) {
                return attempt.skip(skip_reason(error));
            }
            if Instant::now() >= deadline {
                return attempt.skip(MutationSkipReason::Expired);
            }
            let admitted = match cancel.admit() {
                Ok(admitted) => admitted,
                Err(error) => return attempt.skip(skip_reason(error)),
            };
            attempt.mark_dispatched();
            let receipt = admitted.deliver(execute).await;
            Ok(attempt.finish(receipt))
        });
        let result = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => return Err(Error::Cancelled),
            result = &mut task => match result {
                Ok(result) => result?,
                Err(_) => MutationReceipt::without_data(MutationStatus::Unknown).wire(),
            },
        };
        self.check_target()?;
        self.set_phase(RunPhase::ValidatingTools);
        Ok(result)
    }
}

fn check_current(
    cancel: &Cancellation,
    validator: &TargetValidator,
    target: &RunTarget,
) -> Result<(), Error> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    if !validator(target) {
        return Err(Error::TargetChanged);
    }
    Ok(())
}

fn skip_reason(error: Error) -> MutationSkipReason {
    match error {
        Error::Cancelled => MutationSkipReason::Cancelled,
        Error::TargetChanged => MutationSkipReason::TargetChanged,
        _ => MutationSkipReason::Abandoned,
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if !self.finished {
            self.cancel.cancel();
        }
        let mut state = self.inner.state.lock().unwrap();
        if state.active == Some(self.id) {
            if !self.finished {
                state.last_outcome = Some(RunOutcome::Abandoned);
            }
            if state.pending_tool {
                state.phase = Some(RunPhase::Draining);
            } else {
                state.active = None;
                state.phase = None;
            }
        }
    }
}

struct PendingTool {
    inner: Arc<SessionInner>,
    id: u64,
    _permit: tokio::sync::OwnedMutexGuard<()>,
}
impl Drop for PendingTool {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap();
        if state.active == Some(self.id) {
            state.pending_tool = false;
            if state.phase == Some(RunPhase::Draining) {
                state.active = None;
                state.phase = None;
            }
        }
    }
}

#[cfg(test)]
mod mutation_tests;

#[cfg(test)]
mod tests {
    use super::{Engine, RunTarget, ToolRegistry};

    #[tokio::test]
    async fn same_stable_instance_shares_gate_across_generations() {
        let old = ToolRegistry::new(
            RunTarget::new(Engine::Unity, "project", "stable-editor", 1).unwrap(),
        );
        let permit = old.gate.clone().lock_owned().await;
        drop(old);
        let replacement = ToolRegistry::new(
            RunTarget::new(Engine::Unity, "project", "stable-editor", 2).unwrap(),
        );
        assert!(replacement.gate.try_lock().is_err());
        drop(permit);
        assert!(replacement.gate.try_lock().is_ok());
    }
}
