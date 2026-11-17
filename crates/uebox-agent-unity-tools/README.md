# Unity read-only Agent tools

This crate binds one already connected `UnityClient` to an `EditorSelection`
and its original `ConnectionToken`. It never discovers, opens, takes over,
reconnects, or reselects an editor. The host must have explicitly obtained
the user's takeover approval and connected the client before calling it.

The host must have associated that exact client with the supplied token when
registering the connection with `EngineRegistry`. The Unity protocol and the
existing client API cannot independently prove process identity or verify the
host's client/token association. A token cannot be bound to two clients at the
same time in this crate. New generations and connections must be created and
authorized explicitly by the host.

```rust,no_run
use std::sync::{Arc, Mutex};
use uebox_agent_unity_tools::{BindError, UnityTools};
use uebox_engine_core::{ConnectionToken, EditorSelection, EngineRegistry};
use uebox_engine_unity::UnityClient;

async fn bind_existing(
    client: UnityClient,
    registry: Arc<Mutex<EngineRegistry>>,
    selection: EditorSelection,
    token: ConnectionToken,
) -> Result<UnityTools, BindError> {
    UnityTools::new(client, registry, selection, token).await
}
```

The constructor consumes the client even on error and resolves only its
host-selected project path, on a blocking worker. It checks that path, the
original project-checked reply, and the core selection identify the selected
canonical directory. The original reply is historical evidence used for this
binding check, **not** an online indication. A subsequent live read must still
succeed. A noncanonical project path reported by the peer is conservatively
rejected at binding, even if a filesystem lookup could resolve it to the same
directory. On Windows, normal and verbatim forms are equated only for the safe
ASCII path subset described by `uebox-engine-unity`; other mixed-form cases,
including non-ASCII components, are rejected. No untrusted peer path is resolved.
These checks do not hold a directory handle, prove that paths cannot change, or
authenticate the peer.
If binding fails, the host must retire the token it registered for the consumed
client. The crate cannot safely retire an arbitrary supplied token on a binding
error, because it may belong to a different client or project.

Use `run_target()` for the pinned Agent identity, `tool_registry()` for the
two tool declarations, and `validator()` for `AgentSession::new`. The declared
functions are `unity_project_info` and `unity_active_scene`; both accept only
an empty JSON object. Agent core adds engine, canonical project, instance and
generation to each model-facing receipt. Only known read-only project or scene
fields are returned; raw replies, credentials and raw error text are not.
The host must not put a credential in an instance ID or project name. Read-only
project and scene fields are sent to the configured model service and may
themselves contain user-authored content.

Both tools use the same owned client. Each dispatch checks all four target
fields, revalidates the exact selection and token before and after I/O, and
runs the blocking bridge call on a worker. An invalidation during a read causes
its result to be discarded. A scene read first validates a live project reply,
then `UnityClient::active_scene()` performs its own project check and scene
request; the resulting project snapshot is checked again before returning data.
These checks cannot atomically lock the editor or the registry across the TCP
operation. A change is discarded only if a project or selection check actually
detects it; the protocol cannot atomically prove that the peer did not switch
projects between its project and scene replies. Even a valid relative scene
path cannot prove the scene belongs to the pinned project.

Scene paths are checked as strings without filesystem access. An empty path
(an unsaved scene), `Assets/<asset>` and `Packages/<package>/<asset>` paths are
accepted. Absolute paths are rejected even if they name the selected project;
backslashes, colons, control characters, empty components, `.` and `..`
components are also rejected. Package paths are Unity virtual asset paths and
do not assert that a package's underlying files are physically inside the
project. This is a conservative syntax check, not filesystem identity proof.

A busy client returns a tool failure rather than queueing a new call. Once a
call starts, the blocking worker keeps the connection exclusively until it
finishes, including when its future is dropped. If the owning Tokio runtime
continues running, Agent core keeps its session in `Draining` while a cancelled
call finishes. An already admitted worker may still send subsequent read frames
on that connection after cancellation. Bridge socket timeouts do not bound a
worker waiting for the registry lock, and cancellation does not forcibly
interrupt a blocked worker.

**Runtime shutdown changes that guarantee:** shutting down the owning Tokio
runtime can drop the asynchronous task, clear core's `Draining` phase and
release its gate while blocking I/O is still running. The host must not reuse
the same port or physical editor instance for a replacement connection until
the old blocking I/O has actually finished. This crate does not currently
provide a completion signal independent of the runtime; if completion cannot
be established after shutdown, keep that port and instance quarantined. It
cannot prevent other code or processes from opening connections.

A bridge error closes the owned client and retires its exact token; the caller
receives a generic failure. Core can return `TargetChanged` before sending a
tool error to the model once the connection is retired. The validator is
nonblocking and fails closed if the registry is locked or poisoned. If it can
confirm that a selection is stale, it also schedules cleanup after any active
worker finishes. Dropping
the final tool/session reference also closes the connection and retires the
token; if the registry is currently locked, cleanup is deferred to a thread and
the token's binding claim stays reserved until cleanup completes.

Run this crate's isolated tests with:

```text
cargo test --offline --locked --manifest-path crates/uebox-agent-unity-tools/Cargo.toml
```

The TCP editor and HTTP/SSE model peers are temporary loopback test fixtures;
they do not contact a Unity editor or a real model provider. The tests explicitly
reject port 6400 and use synthetic credentials only.
