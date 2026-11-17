# uebox-agent-core

An in-process Rust library for a first, text-only OpenAI Chat Completions
agent loop. It does not start a sidecar, discover an editor, or register any
tools by itself. The host supplies the endpoint, model, target, target
validator and trusted read-only tool implementations.

## Host integration

1. Construct `ProviderConfig` with the **full** completions URL and model ID.
   Optionally set a bearer token or a nonblocking `with_token_provider`
   callback, custom headers, `max_tokens`, and an encoded request byte limit.
   The callback is consulted for each request; the crate does not read the
   environment or any credential store. Redirects and implicit proxies are
   disabled. Header values are marked sensitive and provider errors do not
   include upstream bodies, URLs or credentials.
2. Construct a `RunTarget` with engine, host-normalized project identity,
   instance ID and connection generation. These fields are fixed for the
   session. Build a `ToolRegistry` for that target and explicitly register only
   same-engine `ReadOnlyTool` implementations. Each tool returns a `ToolFuture`
   and must dispatch only to the supplied target, checking the adapter's actual
   connection generation at dispatch. `execute` and the target validator must
   return promptly; the returned future must remain pending until the underlying
   editor operation has actually finished.
3. Create `AgentSession::new` or `new_with_system_prompt`, supplying a
   nonblocking validator that checks whether the captured connection is still
   live. The validator should check the captured identity, not the current UI
   selection. Call `start` to reserve a run atomically, obtain its cancellation
   handle, and await `Run::execute` for the final result. Text deltas arrive via
   its callback. `snapshot` returns the fixed target, active run and phase,
   committed messages, current partial text and last outcome.

Tool calls are held until the full stream has a valid finish reason and `[DONE]`.
Only a registered name with complete, schema-valid JSON is dispatched. Tools
run sequentially; unknown names, invalid arguments and tool failures become error
tool results. Result payloads include the fixed target identity, so the project
identity and tool result will be sent to the explicitly configured model service.
Call IDs are treated as stable metadata: repeated identical IDs are accepted,
but different IDs for one index (including ID fragments) are rejected.
Chat Completions `delta.refusal` fragments are emitted and stored as ordinary
text, so their wording survives in the answer and subsequent history; the
original refusal field is not retained as separate metadata. A response that
mixes a nonempty refusal with tool calls is rejected as a protocol error.
An empty refusal fragment is allowed as a prefix; if any non-null refusal was
present and the completed response contains neither text nor tool calls, the
result is a protocol error. `refusal: null` is treated as absent.
The loop stores completed user/assistant/tool messages in memory across runs;
the system instruction is projected once per request. It never silently truncates
history. The optional byte limit is a bound on the encoded request body, **not**
a model-token estimate.

Registries for the same `(engine, stable instance)` share a process-local tool
gate across sessions and connection generations. Different engines have
separate gates; the project and generation are still checked for each run but
do not change the gate key. If a dispatched tool is cancelled or its run future
is dropped, the caller returns
without waiting, while the session remains `Draining` and the gate is retained
until the tool future completes. A queued tool is not dispatched when
cancellation wins before its dispatch admission. This gate cannot serialize
editor operations initiated outside this crate or in other processes; adapters
must also honor their own transport's concurrency rules. The host must assign
the **same stable instance ID** to all
generations of one physical editor and distinct IDs to different editors. If an
adapter uses a new alias after reconnect, the gates do not match: the host must
wait for the earlier operation to drain or quarantine the old connection. The
tool future must not finish before the underlying editor operation ends.
For example, the current UE tool bridge creates a new public alias for each
captured connection, so its aliases are not a stable cross-reconnect identity;
the host must provide quarantine or a separately managed stable identity.

## Cancellation ordering

Cancellation, event delivery, tool dispatch, transcript batch commit, and
terminal completion use an atomic admission point. If cancellation is ordered
first, later admissions fail with `Cancelled`. If an event, tool, commit, or
terminal result was admitted first, its callback or result can be observed
after `cancel()` returns; an admitted tool may still run and must drain. A
provider completion and its enclosing run completion have separate admission
points, so cancellation can win between them. This contract does not promise
that wall-clock callback start or future observation stops exactly at the return
from `cancel()`. Consumers that require stricter presentation should also filter
events by run identity and their own cancelled state.

Callbacks execute without holding a cancellation lock and may call `cancel()`
themselves. After such a callback returns, no further event from that request is
admitted and the request returns `Cancelled`, unless its terminal result was
already admitted before that cancellation. Each run gets its own cancellation
token; avoid sharing one token between unrelated concurrent requests if a single
ordered event stream is required.

## Schema and trust boundary

Tool parameter schemas must be self-contained Draft 7 object schemas with
`additionalProperties: false` at the root. Unsupported keywords, unknown
formats, `$ref`, `$id`, content encodings and external schema retrieval are
rejected rather than ignored. Validation does not transform arguments or apply
defaults. The host is responsible for choosing an appropriately strict schema
and for ensuring injected implementations are genuinely read-only. A Rust trait
cannot prove that trusted host code is read-only, nor prevent an implementation
from ignoring its target; the crate contains no shell, code execution, write tool
or generic engine-method dispatcher.

This slice does not provide Tauri bindings, real UE/Unity/Godot tools,
persistent history, session restoration, steering, approvals, write locks,
multimodal input, token accounting, context compaction, automatic retries,
provider protocols other than Chat Completions, or complete pi compatibility.
The host must reject unsupported structured capabilities rather than discard
them. Only text deltas are emitted incrementally; tool call drafts are held
internally until complete. In-flight reads that never return keep their gate
and session in `Draining`; the adapter must define how such connections are
quarantined or recovered.

## Verification

`cargo test --offline --manifest-path crates/uebox-agent-core/Cargo.toml` runs
the crate's tests. They use loopback-only TCP HTTP/SSE fixtures and synthetic
credentials, never a real provider or existing user credentials. A sandbox may
need permission to bind a loopback port.
