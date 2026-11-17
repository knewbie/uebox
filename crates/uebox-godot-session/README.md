# Godot session foundation

`uebox-godot-session` is a synchronous Rust library for an explicitly selected
Godot executable and project. The editor is a user-chosen
application process; there is no Node/Python/backend sidecar. This crate does not
add Tauri commands or a UI and is **not ready for a public production launch flow**.
The existing addon implements protocol v1 and cannot authenticate the server,
revoke a live bridge, or keep its token out of the editor's inherited environment.

The host must obtain explicit user authorization before calling `launch`: it
selects the actual native executable file and the trusted project, which must
already contain and enable the existing Godot addon for queries to work. The
selected project can be the user's actual project. A
`.app` directory is not accepted; on macOS select the executable inside it. The
library neither creates a copy nor installs, updates, enables or configures a
plugin. It does not scan for editors or ports, attach to an existing editor,
start a process implicitly, or reconnect automatically. The underlying Godot
client does inspect ancestor directory entries for ambiguous POSIX path aliases;
this is path validation, not editor discovery.

## API and lifecycle

The earlier Task 10 report describes the superseded source-and-copy API; the
single-project interface documented here is the current one.

```rust
use std::{path::Path, time::Duration};
use uebox_godot_session::{EditorMode, GodotSession, LaunchOptions, SessionError};

fn launch_authorized_project(
    executable: &Path, project: &Path,
) -> Result<GodotSession, SessionError> {
    // Only an authorized, trusted native host should opt in to the v1 risks.
    let options = LaunchOptions::new(executable, project)
        .acknowledge_legacy_protocol_risks()
        .editor_mode(EditorMode::Interactive)
        .query_timeout(Duration::from_secs(3));
    GodotSession::launch(options)
}
```

`launch` spawns one process with fixed `--editor --path <project>` arguments
(and a fixed `--headless` option for the explicitly selected headless mode).
LSP and DAP get separate locally allocated ports; the bridge uses a separate
`127.0.0.1` port. The process uses the canonical selected project directory as cwd,
null standard streams, a platform environment allowlist, and fresh per-launch
credentials held in private Rust state and the child environment. No token,
environment map, port input, generic command, shell, or automatic retry is exposed to
the caller. The path and instance ID are available for an application's own
trusted native routing; do not forward internals to a WebView or model.

`launch` returns in `Starting`, without opening a bridge connection.
`connect_once()` performs exactly one initial `project.info` request. A caller
can explicitly try again while starting if the plugin is not listening yet;
there is no background polling. After success, `project_info()` and
`scene_tree()` query only that session. An established session becomes terminal
`Disconnected` on a timeout, broken connection, protocol or target failure;
subsequent queries return `SessionClosed` and do not reconnect. Plugin-reported
size failures remain errors rather than an empty scene. Remote messages and
unrecognized error codes are not echoed to callers.

Trusted Rust code can call `contains_private_bridge_values` to check a string
for literal occurrences of this session's actual token or original instance ID
without retrieving either value. It returns `true` when the client is unavailable
so callers can fail closed. Do not expose this check as a model or UI command;
it is not authentication or a general secret detector and cannot identify
encoded or split values.

Before returning initial project info, project info or a scene tree, the session
checks every returned string field and returns `PrivateValueInResult` without the
field contents when an exact match occurs. A rejected initial result leaves the
session `Starting`; a rejected later query leaves it `Ready`. The host may retry
only through an explicit call. Unrelated 64-character hashes are accepted.

`status()` polls only the saved `Child` and reports `Starting`, `Ready`,
`Disconnected`, `StartupExpired`, or `Exited` with success/code and, on Unix,
a terminating signal when available. A nonzero exit code is not by itself proof
of a crash. There is no background status event; the host must poll on a blocking
worker. The startup window is 60 seconds by default and at most 300 seconds;
query socket operations use a positive timeout of at most 30 seconds. Expiry is
observed on the next operation, and an in-flight request can finish after the
startup window. The existing client's local path inspections and JSON processing
are outside the socket deadline. Never block the Tauri async/UI thread with these
synchronous methods.

Dropping the session does not kill the editor; while the Rust host stays alive, a
background waiter attempts to reap it on exit. `into_editor_process()` releases
management and transfers the original `Child` to a trusted Rust caller, who then
owns waiting on it. Neither action revokes the v1 addon: its listener and token
may remain active until the addon exits or the editor closes. A host crash bypasses
Rust cleanup, and OS logout or shutdown may end the editor; survival is not
guaranteed. Do not expose the `Child` to a renderer or silently kill it.

## Limits and trust boundary

- The library requires an absolute project directory containing `project.godot`;
  it canonicalizes the directory and does not require a second project or a copy.
  The library itself does not copy the project, install the addon or rewrite
  project files. Godot may generate its normal `.godot` import and editor caches,
  and project plugins or scripts may write files with the user's permissions.
  Starting the editor is not a guarantee that the project remains unchanged, nor
  an OS sandbox. Path and executable checks are not atomic with spawn.
- Native executable magic and permissions reject ordinary shell wrappers; they do
  not authenticate Godot or the selected binary. Project code and other addons
  execute in the editor and may access inherited credentials. By default platform config/cache paths are
  inherited; `isolate_editor_paths()` can instead use an existing selected home
  with `appdata`, `localappdata`, `config`, `data`, `cache` and `temp` directories.
  The library does not create them. The allowlist and isolation behavior need
  acceptance on each supported GUI/platform combination.
- Protocol v1 transmits the token in plaintext on each loopback connection, and the
  server gives no proof of possession before receiving it. A process that wins the
  released-port race can obtain the token and forge responses, even when the
  spawned child is alive. The same OS user, other project plugins and descendants
  may also read the editor environment or memory. `acknowledge_legacy_protocol_risks`
  only makes the risk explicit; it does not fix it. Do not present this as secure
  public Tauri integration. A future authenticated protocol, credential consumption,
  lease/revoke, authorized native launch flow and platform validation remain needed.
- The bridge's loopback restriction does not prove that Godot's independent
  LSP/DAP or project-defined network services are disabled or protected. All three
  test/launch ports have a release-before-bind race. Project and scene data are
  live responses from the selected editor and may contain unsaved changes.
- The library does not parse `project.godot` to prove that the addon is enabled.
  A missing/disabled addon, bind conflict, slow editor or other failure may all
  appear as an unavailable connection; no speculative root cause is asserted.
  GUI operation and Windows/Linux real Godot behavior have not been verified here.

## Focused validation

```sh
CARGO_HOME=/tmp/uebox-godot-cargo CARGO_TARGET_DIR=/tmp/uebox-godot-session-target \
  cargo test --manifest-path crates/uebox-godot-session/Cargo.toml --features test-fixtures --offline
GODOT_BIN=/Applications/Godot.app/Contents/MacOS/Godot \
  CARGO_HOME=/tmp/uebox-godot-cargo CARGO_TARGET_DIR=/tmp/uebox-godot-session-target \
  cargo test --manifest-path crates/uebox-godot-session/Cargo.toml --features real-editor --test real_editor --offline
```

The first command uses a test-only fake process and local fixture peer, not a
Godot editor. The second creates its own temporary project, addon and private
configuration directories, and launches the specified real headless Godot. It
checks Godot 4.7.2 on macOS; it does not open an existing project. The test-only
probe records a temporary port and directories but never reads or records the
secret. Both test processes clean up only editors they launched themselves.
