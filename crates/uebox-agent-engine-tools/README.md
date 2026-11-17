# uebox-agent-engine-tools

Engine-specific read-only agent tools. The `unreal` feature is enabled by
default; `--no-default-features` builds the engine-neutral crate without the
UE adapter. No tool is installed into the common agent runtime automatically.

## Binding an explicitly chosen UE connection

The host owns an `Arc<UnrealBridge>` and an `Arc<Mutex<EngineRegistry>>`. When
an identified UE candidate appears, it creates a canonical Unreal
`ProjectIdentity` from the project directory and captures a backend-owned
`UnrealCandidate` using the UE summary and the known `InteractionMode` (or
`Unknown` when it cannot be established). Capture creates and identifies an
engine-core connection with a new process-local public alias. The UE summary's
opaque selection credential stays private inside the candidate; it is never
used as `RunTarget.instance`, which the agent sends to the model.

Retain the candidate and its **original** `ConnectionToken` as the item shown to
the user. After the user explicitly selects that exact item:

```rust,ignore
let binding = UnrealProjectInfoBinding::bind_exact(&candidate, &displayed_token, deadline)?;
let session = AgentSession::new(
    provider, binding.tools()?, binding.target_validator(), max_model_steps,
)?;
```

Capture compares the current UE announcement's path text with the already
approved engine-core project, registers the alias and calls
`identify_with_mode`. `bind_exact` calls engine-core
`select_exact(token, project, false)`: explicit selection permits
non-interactive or unknown interaction modes. It obtains one `SelectedInstance`
using the retained UE credential and checks the announcement again before
returning. The binding retains that
`EditorSelection`, its immutable `RunTarget`, and that original UE handle.
At run time it revalidates the retained selection in engine-core and dispatches
only through the retained `SelectedInstance`; it never reselects or reconnects
using an ID. Changing the UI's current selection does not retarget the binding.

`tools()` constructs a registry containing exactly `ue_project_info`, with an
empty object schema and `additionalProperties: false`. Even direct calls to
`tool()` reject a different engine, project, instance, generation or nonempty
arguments. A successful `project.info` reply is returned unchanged only after
its `projectName` and `projectPath` match the captured UE identity. Announcement
and reply paths are compared as text and never opened, canonicalized, or sent
to a blocking filesystem worker. The host must construct `ProjectIdentity` from
a project the user has already authorized, never from a UE-provided path; that
trusted construction may itself perform filesystem I/O.

On Unix, the accepted peer spellings are the approved canonical path and that
path with one trailing `/`. On Windows, a drive path may use the approved
canonical `\\?\C:\...` spelling or its normal `C:\...` / `C:/...` form; the
drive letter alone is case-insensitive. An explicitly approved UNC project can
use its corresponding canonical `\\?\UNC\...` or normal UNC spelling. A single
trailing separator is accepted. Other path components must match exactly:
symlink, junction, short-name, mapped-drive, dot-segment and unverified case
aliases are rejected. Unrelated UNC shares, device namespaces and malformed
extended paths are rejected without opening them. This text comparison does not
detect an approved path being renamed or redirected after host approval; the
host remains responsible for the identity's lifecycle.

The host must retain one candidate per live UE connection and call
`candidate.retire()` when that connection disappears or changes. Tauri should
retain the bridge, registry, candidate, original token and binding in trusted
backend state; a frontend-supplied ID or `RunTarget` is not proof of selection.
UE's existing
`SelectedInstance` also rejects stale/disconnected handles when queried, even
if host reconciliation has not yet updated engine-core. In that lag window the
`target_validator` can still report the old registry selection as current;
the original UE handle, rather than a new connection lookup, rejects dispatch.

The agent core maps UE/identity errors to a generic `tool_failed` result. There
is no retry, fallback to another editor, arbitrary method, shell or write tool.
After cancellation of a dispatched read, the agent returns promptly but keeps
its session and connection gate draining until the UE query returns, disconnects
or reaches its deadline; late data is not sent to the model. The host must
wait for that drain or quarantine the old connection before allowing a new
candidate for the same physical editor to dispatch. Agent core's gate is keyed
by engine and instance alias, but each capture gets a new alias, so different
candidates for the same physical editor do not share an instance-wide lock.

The configured deadline applies to the UE request. After its reply, identity
validation does only bounded text comparison against the approved path; there
is no additional filesystem resolution to hang. This does not promise a hard
wall-clock deadline for every operation, including trusted registry lock waits.

## Limits

This is an adapter boundary, not Tauri wiring or real UE acceptance. The current
UE API has no synchronous `SelectedInstance` validity probe, per-connection
close method, or atomic engine-core-check-and-transport-dispatch operation.
The host must keep engine-core lifecycle state synchronized; there remains a
time-of-check/time-of-use window around a concurrently changing UE connection.
The original handle prevents intentional re-selection, UE invalidation fails
pending requests when its announcement is processed, and replies naming another
project are rejected, but these checks cannot prove that no frame was sent
during an unreported or simultaneous project change. Strict transport-level
guarantees require additional UE adapter/protocol support. The bridge's origin
filter is not authentication against other local processes.

Tests use temporary project directories, loopback WebSocket UE fixtures and a
loopback HTTP/SSE model fixture. They do not contact a real model or editor.
