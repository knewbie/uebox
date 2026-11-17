# Godot editor adapter

`uebox-engine-godot` is an in-process Rust library, paired with the opt-in editor
addon at `integrations/godot/addons/uebox_godot/`. It does not run a Node, Python,
or other backend service. The Rust caller connects directly to a Godot editor
process over TCP bound to `127.0.0.1`.

## What it can read

- `GodotClient::project_info()` returns the project name and Godot version read
  from that editor.
- `GodotClient::scene_tree()` reads Godot's
  [`EditorInterface.get_edited_scene_root()`](https://docs.godotengine.org/en/stable/classes/class_editorinterface.html#class-editorinterface-method-get-edited-scene-root)
  and returns the currently edited scene's `res://` path
  (or `None` for an unsaved or absent scene), plus nodes' relative paths, names,
  and Godot classes. An absent scene has an empty node list. Results are truncated
  at 2,048 queued nodes, a 512 KiB encoded node budget, an 8 KiB relative node
  path, or a 1,024-character node name or class. `truncated` tells the caller
  whether nodes were omitted; an omitted node's descendants are omitted too.
  A scene path over 4,096 characters is rejected as `response_too_large`.

These are the only supported commands. There is no generic command or code
execution API, and no write command. Queries neither save nor alter the scene.
Editor state is live; it need not match the last saved scene.

## Setup and targeting

The addon must be installed and enabled **in the intended project** by the user
or a separately authorized installer. Copy `addons/uebox_godot` from
`integrations/godot` into that project and enable it in Godot's Project Settings
→ Plugins. Enabling an addon modifies that project's configuration; this library
does not perform installation or alter a project on its own.

The editor must start with all three environment variables. Without any one of
them, with a malformed value, or if the port cannot be bound, the addon does not
listen:

- `UEBOX_GODOT_PORT`: explicit nonzero TCP port.
- `UEBOX_GODOT_INSTANCE_ID`: a fresh 32-character lowercase hex identifier.
- `UEBOX_GODOT_TOKEN`: a fresh 64-character lowercase hex secret.

Call `Credentials::generate()` once **per editor launch** and keep the token
private. Do not reuse the identifier or token between launches, even for the same
project. Pass the values only to the intended editor and retain them in the Rust
application's memory; do not commit them, put them in project settings, log them,
or expose them to a renderer or a model. Start the editor using the same
canonical project directory supplied to `Target::new` (including resolved
symlinks). The addon is activated at editor startup, so setting these variables
later in the calling process cannot configure an already-running editor.

On POSIX systems the client rejects a canonical project path containing a
literal backslash. It also scans entries of the target's ancestor directories
for backslash names that normalize to a target prefix, rejecting detected aliases,
unreadable entries and scans exceeding 16,384 entries. The addon checks the
editor's working-directory parent and `project.godot` against its reported
project before listening; launch with `--path` so Godot uses the project directory
as its working directory. These filesystem checks are not atomic and cannot prove
identity for every alias outside the inspected ancestors or a deliberately crafted
combination of links. They do not replace a trusted process-launch mechanism.

An integration can create a target once it has chosen the specific local port,
project directory and editor instance:

```rust
use std::{net::SocketAddr, time::Duration};
use uebox_engine_godot::{Credentials, GodotClient, Target};

fn example(project_dir: &std::path::Path, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let credentials = Credentials::generate()?;
    let project_dir = std::fs::canonicalize(project_dir)?;
    // Start the intended Godot editor with --editor --path <project_dir> and set
    // UEBOX_GODOT_PORT, UEBOX_GODOT_INSTANCE_ID, and UEBOX_GODOT_TOKEN from
    // `port` and `credentials`. Install/enable the addon only with user authorization.
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let client = GodotClient::new(Target::new(
        address,
        &project_dir,
        &credentials.instance_id,
        &credentials.token,
        Duration::from_secs(3),
    )?);
    // Once the editor has initialized:
    let project = client.project_info()?;
    let scene = client.scene_tree()?;
    let _ = (project, scene);
    Ok(())
}
```

The API is synchronous; GUI applications should call it on an appropriate
blocking worker. It has no discovery or automatic selection: the caller supplies
one endpoint, canonical project and instance ID. A successful response is
accepted only when its request ID, protocol version, project and instance match.
Each call opens a separate connection; its socket operations share a deadline.
Filesystem identity checks and local JSON parsing are outside that deadline. Error variants
distinguish invalid targets, timeouts, disconnections, malformed/mismatched
responses, editor rejections, entropy failure, and other I/O errors. Do not
silently retry against a different instance.

`GodotClient::contains_private_bridge_values` lets trusted Rust code check a
string for literal occurrences of this client's actual token or original
instance ID. It returns only a boolean; do not expose it as a model or UI
command. The low-level client does not itself filter returned data or remote
error text: its trusted caller must handle both before forwarding anything to a
UI or model. The check is not authentication or a general secret detector:
encoded or split values are not detected.

## Protocol and trust boundary

One newline-delimited UTF-8 JSON request and response are exchanged per connection.
Every request carries version, request ID, secret, exact project/instance target,
and a command from the two-command allowlist. The addon authenticates before
checking identity or executing a query. Unauthenticated and wrong-target errors
do not return project data or identity. Unknown commands are rejected. It limits
requests to 8 KiB, responses to 1 MiB, concurrent clients to 16, and each client
to three seconds; I/O is polled from the editor instead of blocking its UI thread.
An unauthenticated local process can occupy the 16 slots and deny service; the
slot and lifetime limits do not guarantee availability against local clients.

This is a local shared-secret boundary, not a sandbox against a hostile process
running as the same OS user. The token is sent over loopback as plaintext and may
be accessible to processes able to inspect the editor's environment or impersonate
the selected local port. Keep the per-launch secret private and do not use the
bridge as a remote service. Port selection, editor lifecycle, addon installation,
and authorization to query are the integrating application's responsibilities.

## Focused validation

```sh
cargo test --manifest-path crates/uebox-engine-godot/Cargo.toml --test protocol
GODOT_BIN=/path/to/Godot cargo test --manifest-path crates/uebox-engine-godot/Cargo.toml \
  --features real-editor --test real_editor
```

The first command uses local protocol peers; it does not claim a real engine.
The opt-in second command creates and deletes its own temporary projects, launches
real headless Godot editors, and fails if the specified editor is unavailable. It
expects Godot 4.7.2 and does not open an existing user project. Each test editor
gets private HOME, APPDATA, LOCALAPPDATA, XDG and temporary directory settings;
the test checks Godot's reported configuration, data and cache paths. The test
releases selected ports before editors bind them, so another process could claim
a port and impersonate the expected peer. Run only in a trusted local environment.
Validation here has been performed with Godot 4.7.2 on macOS; other OS and Godot
versions, including actual Windows process isolation and path handling, have not
been verified.
