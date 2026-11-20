# Unity editor integration

UEBox connects directly to the Coplay Unity MCP editor bridge from Rust. For the
optional GameObject rename tool, install the Coplay Unity MCP package and copy
`Editor/UeBoxGameObjectName.cs` into the project's `Assets/Editor` directory.
The companion was verified with Coplay 9.5.3 and Unity 2022.3.62f3.

In Unity, start the bridge and select a single GameObject in the ordinary
active scene. In UEBox, select that project and bridge port, then enable
GameObject rename requests before connecting the assistant. When the assistant
proposes a change, approve or decline it in the window that owns the run.

The editor holds a short-lived, single-use ticket for the proposed name, verifies
the selected object, active scene and original name again before applying, and
returns the live result. Rename can be undone in Unity. Save the scene from
Unity when ready.

For an isolated run, prepare a disposable project under the system temporary
directory and save a scene at `Assets/UEBoxRenameProbe.unity` containing a selected
GameObject named `Original Fixture Target`. Start only its Coplay bridge on a
dedicated port. The integration test uses a synthetic model and checks approval
ownership, one-use replies, editor read-back and unchanged scene bytes.

```sh
UEBOX_TEST_UNITY_PROJECT=/absolute/path/to/disposable/project \
UEBOX_TEST_UNITY_PORT=54876 \
cargo test --locked --manifest-path src-tauri/Cargo.toml --no-default-features \
  --features unity --test unity_command unity_agent_approves_rename_in_a_real_disposable_editor \
  -- --ignored
```
