# Unity editor bridge

This library talks directly to the TCP bridge in Coplay's Unity MCP editor
plugin (the examined version is 9.5.3). It does not start an MCP server or any
sidecar. The caller must select both an explicit loopback socket address and the
expected absolute Unity project root; the library does not scan for editors.

```rust,no_run
use std::net::SocketAddr;
use uebox_engine_unity::{BridgeError, BridgeOptions, Target, UnityClient};

fn inspect(address: SocketAddr, project_root: &str, user_approved_takeover: bool) -> Result<(), BridgeError> {
    let target = Target::new(address, project_root)?;
    if !user_approved_takeover {
        return Err(BridgeError::AuthorizationRequired);
    }
    // Only call this after the user approves replacing the client on this target.
    let approved = target.authorize_takeover();
    let mut editor = UnityClient::connect_authorized(approved, BridgeOptions::default())?;
    let project = editor.project_info()?;
    let scene = editor.active_scene()?;
    println!("{}: {}", project.project_name, scene.name);
    Ok(())
}
```

The old `connect(Target, ...)` always returns `AuthorizationRequired` without
opening a socket. After the user explicitly authorizes taking over a specific
target, `Target::authorize_takeover()` creates a non-cloneable, one-use value
for `connect_authorized`. The host must obtain that user approval; constructing
the value is an assertion by the caller, not a consent prompt.

The authorized connection checks the editor's framed-protocol welcome and
queries `get_project_info`; it refuses a different project root. Before opening
any socket, the selected project root must resolve to an accessible directory.
The library retains that selected path and its canonical result and compares
the editor's report with those two representations without resolving the
untrusted report. On macOS and Linux, a selected symlink therefore matches the
canonical path; another alias is rejected even if it reaches the same directory.
These are path comparisons against the pre-connection result, not directory
handles: they do not detect later filesystem changes or authenticate the peer.

On Windows, selected paths, their canonical results, and editor reports must be
drive-rooted; UNC and device paths are rejected. A selected drive that resolves
to a UNC path is also rejected before connecting. Canonicalizing the selected
path can itself access storage, including a mapped network drive, and is not
bounded by the TCP timeout. Ordinary and verbatim drive paths are compared
without ignoring case; equivalence across those two forms is limited to safe
ASCII path components (letters, digits, spaces, dots, `_` and `-`, excluding
reserved device names and trailing spaces or dots). Other ambiguous forms and
aliases not equal to the selected or retained canonical path are rejected.
`last_observed_project_info()` returns a
historical snapshot only and **must not be used as an online indicator**.
`project_info()` performs a live query; `active_scene()` checks the project
again on the same connection before issuing `manage_scene/get_active`.
`ping()` explicitly sends Coplay's framed `ping` on that connection and checks
for `pong`; it does not start a background task, reconnect, or verify process
identity. Coplay may close an idle connection after approximately 30 seconds,
so a host may schedule its own serialized pings if it wants to keep it alive.
Requests are sequential; a failed request closes the connection. A later
connection requires a new target selection and explicit authorization. Server
errors, malformed replies, invalid frame lengths, timeouts and disconnects are
returned as `BridgeError` values.

The default request/handshake deadline is 5 seconds and the default frame cap
is 1 MiB. Options permit at most 30 seconds and 64 MiB, respectively. These
are blocking operations: a Tauri host should run them on a worker rather than
its UI thread. This crate is not yet wired into Tauri.

The editor plugin uses a single-client socket: **any new connection can evict
the current client**, even before identity is checked. Do not probe ports or
create duplicate clients for one endpoint. The bridge has no peer authentication.
A matching project path limits accidental routing but does not establish trust
against another local process impersonating the editor. The protocol cannot
prove that the same project and port after a restart identify the same editor
instance. Treat the established connection as the only session and require a
new selection and authorization when it closes.

The exposed commands read project and scene data; this does not imply the
third-party plugin has no editor-wide side effects. Coplay may initialize
user-level EditorPrefs when discovering tools. Disabled resources or tools can
make the queries unavailable, in which case the editor's error is returned.

`cargo test --manifest-path crates/uebox-engine-unity/Cargo.toml` runs the
crate's real loopback TCP protocol tests. `examples/inspect.rs` requires the
`--allow-disconnect-existing-client` flag, a socket address and project root;
use it only with a disposable or explicitly authorized editor instance.
