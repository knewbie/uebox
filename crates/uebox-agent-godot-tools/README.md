# Godot read-only Agent tools

This Rust crate binds an already launched and connected `GodotSession` to one
`uebox-engine-core::EditorSelection`. It does not discover, launch, reconnect,
install, or modify an editor. It registers only `godot_project_info` and
`godot_scene_tree`, both with empty, closed argument schemas. Project and scene
values come from the editor response; `scene_tree` always includes the returned
`scene_path`, nodes and original `truncated` flag. It does not depend on UE or
Unity adapter crates.

The trusted host must create and connect the Godot session, identify it in its
shared `EngineRegistry`, and retain the exact `EditorSelection` and connection
token. Put the existing session in `GodotSessionHandle`, an alias for
`Arc<Mutex<Option<GodotSession>>>`, and call:

```rust
use std::sync::{Arc, Mutex};
use uebox_agent_godot_tools::{GodotSessionHandle, GodotToolBinding};
use uebox_engine_core::{EditorSelection, EngineRegistry};

fn bind(
    registry: Arc<Mutex<EngineRegistry>>,
    selection: EditorSelection,
    session: GodotSessionHandle,
) -> Result<Arc<GodotToolBinding>, uebox_agent_godot_tools::BindError> {
    GodotToolBinding::new(registry, selection, session)
}
```

`binding.tool_registry()` returns a registry containing the two tools and a
fixed `RunTarget`; pass it and `binding.validator()` to `AgentSession::new`.
`binding.target()` exposes the same target to trusted host code. Its project
and instance strings are process-local aliases derived from the globally unique
core connection generation; they are stable for the same selection and distinct
across generations, but are not secrets or authorization tokens. Agent core
includes this target in model-facing receipts. The selected canonical project
path and original bridge instance ID remain internal to the binding and are
compared against the actual session on every dispatch.

The binding checks the core selection before dispatch and after the read, and
holds the same session mutex through the real blocking socket operation. It
never looks up a replacement session by name. Blocking work runs with
`tokio::task::spawn_blocking`; an async permit and the host session lock stay in
that worker until it finishes, even if the tool future is cancelled or dropped.
A queued request does not start another read on the same binding. Locks or
registry validation that cannot be acquired fail closed. The Agent validator is
nonblocking: when a session lock is busy it defers the decisive check to the
worker. Hosts should not hold the engine registry lock across editor I/O.

When the host releases a session, locking the shared handle and calling `take()`
waits for any in-flight read to finish. The old tools then fail without opening
another connection. Taking the session does **not** retire `EngineRegistry`'s
connection token: the host must separately close the exact token on release or
editor exit, and should route future work through a fresh selection. The
existing v1 addon may continue listening after session release; this crate cannot
revoke it or kill the editor.

Tool failures become `ToolError`; the current Agent core emits a generic
`tool_failed` receipt instead of returning raw socket errors, environment data,
credentials or untrusted diagnostic text. The session rejects project or scene
results containing literal occurrences of its actual token or original instance
ID before returning them to a host. The binding also checks every string in its
output, using a trusted-Rust-only session/client interface that returns a boolean,
not the secret. Unrelated 64-character hashes are accepted. This is not a general
secret detector: a malicious editor or project can encode or split secrets among
fields, or expose unrelated credentials. The host must trust the editor and its
project data; this crate cannot protect secrets against hostile same-user
processes.

The v1 Godot bridge sends its token over loopback without first authenticating
the server and has no authenticated revoke/lease. Port takeover or same-user
processes/project scripts can obtain or forge data. These tools inherit that
boundary and are not a security approval for public Tauri or model integration.
The host remains responsible for user authorization, provider selection, session
lifecycle and the existing cross-generation quarantine requirements.

## Tests

```sh
CARGO_HOME=/tmp/uebox-agent-godot-cargo CARGO_TARGET_DIR=/tmp/uebox-agent-godot-target \
  cargo test --manifest-path crates/uebox-agent-godot-tools/Cargo.toml --features test-fixtures --offline
GODOT_BIN=/Applications/Godot.app/Contents/MacOS/Godot \
  CARGO_HOME=/tmp/uebox-agent-godot-cargo CARGO_TARGET_DIR=/tmp/uebox-agent-godot-target \
  cargo test --manifest-path crates/uebox-agent-godot-tools/Cargo.toml --features real-editor --test real_editor --offline
```

The first suite uses a test-only fake editor over real loopback TCP and a local
simulated SSE provider, with generated test-only credentials. The optional real
suite creates its own temporary project and private config directories, copies
the existing addon into that temporary project, and launches only the selected
real Godot executable. It has been exercised with macOS Godot 4.7.2 headless;
Windows, Linux, visible editor operation and Tauri command integration are not
established by these tests.
