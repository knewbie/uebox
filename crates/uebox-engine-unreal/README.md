# uebox-engine-unreal

An in-process Rust adapter for the existing UnrealAgentLink WebSocket protocol.
The plugin connects to the listener; no external runtime or sidecar is started.

## API

Run the adapter inside a Tokio runtime owned by the Tauri host:

```rust,no_run
use std::time::Duration;
use uebox_engine_unreal::{BridgeError, UnrealBridge};

async fn query_chosen(
    bridge: &UnrealBridge,
    chosen_connection_id: &str,
) -> Result<serde_json::Value, BridgeError> {
    // The host obtains this ID from an explicit user choice in bridge.instances().
    let selected = bridge.select_instance(chosen_connection_id)?;
    selected.get_project_info(Duration::from_secs(10)).await
}
```

In a real host, do not select the first entry automatically: retain the user's
chosen connection ID or the returned `SelectedInstance` in application state.
Both `InstanceSummary` and `BridgeError` implement `serde::Serialize` for
Tauri commands. `start` returns a bind error if the port is already occupied;
`bind(0)` is available for isolated local tests. Dropping the bridge initiates
shutdown; `shutdown().await` waits for listeners and requests to be cleaned up.

Each list item's `connection_id` is an opaque selection credential for that
particular connection and project identity. It rotates on a project identity
change or an invalidating announcement, even when the WebSocket stays open. A
stale list item therefore cannot select the next project; the host must present
a fresh list and obtain a new user choice. Previously returned
`SelectedInstance` handles also become stale. Reconnection, project closure,
and shutdown likewise require a new choice. IDs are ephemeral, unique within
one host process, and must not be persisted across application restarts.
Non-interactive instances remain visible with `interactive: false`; older
plugin announcements that omit the field are treated as interactive.

The read-only project query uses `project.info`. The adapter
returns the engine's result object without synthesizing project data, reports
protocol errors, and never retries a timed-out or disconnected request. The
caller supplies the deadline. A timeout does not prove that the remote side
didn't receive the request.

### Guarded Actor labels

On an explicitly chosen interactive selection, `prepare_actor_label(label,
deadline)` sends `actor.prepare_label` with `new_label`. A supported plugin
returns an `actor_label_v1` preview for the single selected Actor in the current
editor level. Keep `PreparedActorLabel` in trusted host state and show only
its actor object path, old label, and requested label. The plugin expires the
ticket after 120 seconds. Drop the preview on denial or cancellation before
dispatch.

After approval, call `commit_actor_label(prepared, deadline)` on the original
selection. It consumes the preview and sends one `actor.set_property` frame
containing only `guarded_actor_label.ticket`. The legacy setter needs fields
absent from this frame, and older plugins cannot issue a supported preview.
The plugin consumes the ticket before editing and rechecks socket, project,
editor world, level, object identity, selection, and the original label. It
rejects play/simulate and reports an actual post-transaction label. The command
never saves the level.

`ActorLabelOutcome` distinguishes an applied value, observed mismatch,
confirmed non-execution, and an unknown outcome after possible dispatch.
The host should retain and await the commit through settlement even if the UI
or model run stops waiting. Never retry an unknown write, including by issuing
a fresh preview implicitly. Approval ownership and handling of unknown results
across reconnections belong to the host.

By default the listener sends WebSocket pings every 15 seconds and disconnects
a socket after 45 seconds without inbound WebSocket activity. Plugin
`system.heartbeat` events and pong frames count as activity; one project-query
timeout does not close the socket. A connection also has 45 seconds to announce
a valid project, even if it answers pings. Losing a previously valid project
announcement starts another registration deadline. Pending requests fail when
the connection is actually closed. `bind_with_liveness(port, LivenessConfig)`
allows shorter test durations or explicit host settings; all durations must be
positive, and the ping interval must be shorter than the inactivity timeout.

The listener permits only a missing Origin or exactly
`http://127.0.0.1`, matching the current plugin and Electron server. Origin
filtering is not local-process authentication: another local process can
connect or imitate an Origin. Incoming messages and frames are limited to
32 MiB.

## Verification

```sh
cargo test --manifest-path crates/uebox-engine-unreal/Cargo.toml --test protocol
```

These tests use real local TCP/WebSocket connections on temporary ports with a
Rust protocol fixture. They do not start Unreal Engine or establish a real UE
plugin connection.
