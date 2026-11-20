# Tauri migration preview / Tauri 迁移预览

This is a separate preview entry. It does not implement the Electron bridge. A limited
Unity project and active-scene query, an Unreal project query, and Godot editor
queries are available after explicit user actions. Each engine has a fixed-target
read-only assistant preview.
Use the Electron application for existing features.
Its identifier is separate so it does not use the Electron application's webview
preferences. Node is used only to build/serve the frontend during development;
the application backend and commands run in the Tauri Rust process.

这是独立的迁移预览入口，不实现 Electron 桥；只有用户主动操作才会执行有限的 Unity
项目和活动场景查询、Unreal 项目只读查询以及 Godot 编辑器查询。三种引擎各有绑定固定目标
的只读助手预览。现有功能请继续使用
Electron 版本。预览版使用独立标识，不读取 Electron 的 WebView 偏好设置。Node 只在
开发时构建或提供前端文件，应用后端和命令运行在 Tauri 的 Rust 进程内。

## Development / 开发

```sh
pnpm install
pnpm tauri:dev
```

`pnpm tauri:build` builds the native application. Rust and the platform prerequisites
for Tauri 2 must be installed. The normal Electron commands are unchanged.

`pnpm tauri:build` 构建原生应用；需先安装 Rust 和 Tauri 2 对应平台的系统依赖。
原有 Electron 命令不变。

### Engine features / 引擎编译选项

The default Cargo features include `unity`, `unreal`, and `godot`. A host can instead
be built with exactly one engine, or with no engines:

默认 Cargo features 同时启用 `unity`、`unreal` 和 `godot`。也可以只编译一种引擎，
或不编译任何引擎：

```sh
cargo build --manifest-path src-tauri/Cargo.toml --no-default-features --features unity
cargo build --manifest-path src-tauri/Cargo.toml --no-default-features --features unreal
cargo build --manifest-path src-tauri/Cargo.toml --no-default-features --features godot
cargo build --manifest-path src-tauri/Cargo.toml --no-default-features
```

To build a single-engine Tauri debug executable with the bundled frontend:

如需构建包含前端的单引擎 Tauri 调试程序：

```sh
pnpm tauri build --debug --no-bundle --features unity -- --no-default-features
pnpm tauri build --debug --no-bundle --features unreal -- --no-default-features
pnpm tauri build --debug --no-bundle --features godot -- --no-default-features
```

An excluded engine's commands and state are not registered, and its panel is
marked unavailable. The feature flag means the adapter is compiled in; it does
not indicate an editor is connected or that full engine support is complete.

未编译引擎的命令和状态不会注册，界面会标记该面板不可用。feature 只表示适配器已编译，
不表示编辑器已连接或完整引擎功能已完成。

## Interface / 接口

The host assembles `src/unity.rs`, `src/unreal.rs`, and `src/godot.rs` as separate command and
state modules. The preview keeps each engine's panel and command wrappers in
its own `ui/src/engines/` directory; `App.vue` composes them and passes the
host OS to the Unity panel for path validation. Shared panel styling and the
existing locale namespace remain engine-neutral.

宿主分别在 `src/unity.rs`、`src/unreal.rs` 和 `src/godot.rs` 中维护各引擎命令与状态。
预览界面在 `ui/src/engines/` 下分别维护各自的面板和命令封装；`App.vue`
负责组合，并向 Unity 面板传入宿主操作系统用于路径校验。面板基础样式和现有语言包
命名空间保持为中立的共享部分。

`host_status` returns `backend`, `processId`,
`os`, `architecture`, `version`, `migration`, `engineModules: { unity, unreal, godot }`
and a list of `{ id, available }`
capabilities. The values describing the process and platform come from Rust.
`projectRegistration` identifies the manual registry preview, independently of
the selected engine features. The `libraries` capability tracks migration of
the full libraries feature.

`host_status` 返回 `backend`、`processId`、`os`、`architecture`、
`version`、`migration`、`engineModules: { unity, unreal, godot }` 和 `{ id, available }`
能力列表。`engineModules` 表示编译进当前宿主的适配器。进程与平台字段由 Rust
读取。`projectRegistration` 表示手工登记预览，独立于引擎编译选项。`libraries`
表示完整项目库和资料库的迁移状态。

### Unreal and Unity assistants / UE 与 Unity 助手

For Unreal, select an interactive listener connection and confirm the absolute
project directory. For Unity, explicitly approve takeover and choose the dedicated
assistant connection action. It holds one local socket and reserves the selected
port until the connection closes and admitted I/O drains. The existing one-shot
Unity query uses the same port reservation. Reconnecting requires fresh approval.

The model address, name, and optional credential are set for each assistant.
The host retires its target on disconnect or project change. Model-facing tool
results use opaque target identifiers and omit the absolute project directory.
The local bridge trust limits below also apply to the assistants.

UE 助手需要选择交互实例并确认工程目录绝对路径。Unity 助手需要明确同意接管，并选择
专用连接操作。它持有一个本机连接，直到连接关闭且已派发的读取完成才释放端口。原有
Unity 单次查询共用端口占用约束。重新连接需要重新授权。

每个助手单独配置模型地址、名称和可选凭据。断线或切换工程时，宿主退役该目标。
面向模型的工具结果使用不透明标识并省略工程目录绝对路径。下述本机协议的信任边界
同样适用于助手。

An opt-in Unity test reads an actual temporary editor project through Coplay
and passes the project and scene receipts into a loopback model fixture. Start
the disposable project's bridge on a dedicated port and open the
`Assets/UEBoxProbe.unity` scene first. The test refuses port 6400 and paths
outside the system temporary directory.

真实 Unity 测试通过 Coplay 读取临时编辑器工程，并将项目与场景回执送到环回模型夹具。
请先在独立端口启动临时工程的桥并打开 `Assets/UEBoxProbe.unity`。用例会拒绝 6400 端口
及系统临时目录以外的工程路径。

```sh
UEBOX_TEST_UNITY_PROJECT=/absolute/temporary/project UEBOX_TEST_UNITY_PORT=54830 \
  cargo test --locked --manifest-path src-tauri/Cargo.toml --no-default-features \
  --features unity --test unity_command unity_agent_queries_a_real_disposable_editor \
  -- --ignored --exact
```

### Unreal Actor label approval / UE Actor 标签审批

Before creating an Unreal assistant, enable Actor label changes for that
session. The assistant can then request a change to the single Actor selected
in the editor. The preview shows the Actor path and both labels. Approve or
decline each request in the window that started it. The editor rechecks the
connection, project, Actor, and original label, applies the change in an editor
transaction, and reads back the value. Save the project in Unreal when ready.
After an uncertain outcome, further label writes to the same project are
blocked for the life of the host. Inspect the result in the editor before
starting a new host session. Receipts are kept in memory. A real Unreal
build and editor validation are pending.

创建 UE 助手前，可为该会话开启 Actor 标签修改。助手可请求修改编辑器中唯一选中的
Actor。预览显示对象路径和新旧标签，在发起窗口逐次批准或拒绝。编辑器重新核验连接、
项目、对象和原标签，执行事务并读回标签。需要保存时请在 Unreal 中操作。结果未知时，
当前宿主会阻止该工程后续标签修改。请先在编辑器确认结果，再开启新的宿主会话。
回执保存在内存中。真实 UE 的插件构建与编辑器验证仍待完成。

### Chat history / 对话历史

Select **Save this chat to local history** before configuring an
assistant. The Rust host stores the submitted user message before contacting
the configured model. History records the assistant's final answer, the
terminal status, and settled UE Actor label receipts. Open **Conversation
history** to read the saved text. Open a new assistant session with a verified
editor connection to continue working. The local history has a separate SQLite
database and stores no provider credentials, connection tickets, or raw tools
messages. When a host restarts, any unfinished turn is shown as interrupted.

配置助手前选择「将本次对话保存到本机历史」。Rust 宿主先保存用户提问，再调用已配置的
模型。历史保留助手最终回复、轮次终态和已结算的 UE Actor 标签回执。打开「对话历史」即可
查看已保存的文本。继续操作需核验编辑器连接并建立新的助手会话。历史使用独立 SQLite 库，
不会保存模型凭据、连接票据或原始工具消息。应用重启时，未结束的轮次会标记为已中断。

### Legacy chat archive / 旧版对话归档

In Electron, open the actions menu beside the Chats heading and select
**Export chat history**. In Tauri's **Legacy chat archive**, choose the exported
JSON and confirm the import. Imports preserve visible chat text and status.
Supported inline images are validated and stored locally for on-demand viewing.
The archive marks other attachments as unavailable when the original bytes
were never persisted in the visible history. Re-importing the same saved
snapshot leaves one archived copy. Updated snapshots are retained separately.

在 Electron 的「对话」标题操作菜单中选择「导出对话历史」。在 Tauri 的「旧版对话归档」
选择导出的 JSON 并确认导入。归档保留可见文本与消息状态。支持的内嵌图片经校验后保存在本机，
可按需查看。原始内容未保存到可见历史的附件会标记为不可用。同一快照重复导入只保留一份，
历史内容更新后的快照单独归档。

### Saved model settings / 模型设置

In **Model settings**, add a Chat Completions endpoint and model ID. Choose
whether the model needs a system credential or an environment variable. The
Rust host associates credentials with the saved endpoint. In any of the three
assistant panels, choose **Use a saved model** to select a profile or the Agent
default. Editing or removing a profile revokes its credential for subsequent
requests from an assistant configured with that profile.

Electron's **Export model profiles** action writes supported model metadata
and default bindings to a JSON file. Import that file in Tauri's **Model
settings**. Profiles that used credentials, OAuth, or custom headers require
authorization before use. Credential values stay in the original app's store
and the new system store is configured separately.

在「模型设置」添加 Chat Completions 地址和模型 ID，并选择系统凭据或环境变量等授权方式。
Rust 将保存的凭据绑定到配置地址。三种引擎的助手均可选择「使用已保存的模型」及助手默认模型。
修改或移除模型配置会撤销已经使用该凭据的助手后续请求。

Electron 的「导出模型配置」仅保存受支持的模型元数据和默认用途绑定。在 Tauri 的「模型设置」
中选中并导入文件。原来使用凭据、OAuth 或自定义请求头的配置需重新授权。原系统存储继续保留，
新系统凭据单独配置。

### Project collections / 项目合集

The project library can group registered projects, including offline entries,
into named or unnamed collections. A project can belong to several collections.
Changing collection order or dissolving a collection preserves the project
registrations and files.

In Electron's project toolbar, export registrations and project collections.
Import the registrations first in Tauri, then choose and confirm the collection
file. Projects match by engine and their originally registered absolute path.
An import with a missing registration or conflicting collection leaves the
current collection records intact.

项目库支持将已登记的工程加入具名或未命名合集，离线工程也可使用。一个工程可以同时属于
多个合集。调整顺序或解散合集会保留工程登记与文件。

在 Electron 项目工具栏分别导出项目登记与项目合集。在 Tauri 先导入项目登记，再选择并
确认合集文件。成员按引擎与最初登记的绝对路径匹配。若有登记缺失或合集冲突，现有合集记录
会保留。

### Native CLI / 原生 CLI

On macOS and Linux builds, use **Native CLI → Start CLI endpoint** to start
the in-process read-only server. Build the console binary and use the directory
displayed in the app. The server authenticates the local system user, accepts
the five compiled editor read operations, and only queries connected targets.
The console can show help and its version while the app is closed.

在 macOS 与 Linux 构建中，从「原生 CLI」启动进程内只读入口，使用界面显示的目录。
服务器校验本机用户身份，按编译选项公开五种固定读取操作，并只查询已经连接的编辑器。
应用未启动时，控制台仍可显示帮助与版本。

```sh
cargo build --locked --manifest-path crates/uebox-cli/Cargo.toml --release
crates/uebox-cli/target/release/uebox --endpoint "/directory/shown/in/UEBox" doctor --json
crates/uebox-cli/target/release/uebox --endpoint "/directory/shown/in/UEBox" projects list --json
```

### Project registrations / 项目登记

Choose an engine and enter a name and absolute project directory in the preview.
`list_registered_projects`, `register_project`, and `remove_registered_project`
manage the same registry across restarts. Registration requires an online directory.
Removing a record preserves the project files, and offline records can be removed.
The decimal string `id` identifies a record without losing SQLite integer precision.

The first registration creates `project-library/library.sqlite` within the Tauri
preview's application-local data directory. The Rust host holds a process-shared
file lock while operating on the database. Unix checks the storage mode bits;
Windows relies on the application's LocalAppData permissions and rejects reparse
points. These checks assume a trusted local user profile. Incompatible databases
and rollback journals are preserved and reported. Crash recovery remains out of
the preview, and the Electron database is separate.

在预览中选择引擎，并输入名称与项目目录的绝对路径。`list_registered_projects`、
`register_project` 和 `remove_registered_project` 操作同一个可跨重启持久化的登记库。
登记时目录必须在线。可以移除离线记录，项目文件会保留。记录 ID 用十进制字符串传递，
以保留 SQLite 整数精度。

首次登记在 Tauri 预览应用的本地数据目录创建 `project-library/library.sqlite`。
Rust 宿主在每次操作期间持有跨进程文件锁。Unix 检查目录和文件的权限位，
Windows 依赖应用的 LocalAppData 权限并拒绝重解析点。这些检查以可信的本机用户配置
目录为前提。不兼容的数据库和回滚日志会保留，并在界面提示。崩溃恢复尚待接入，
Electron 数据库独立存放。

In the Electron project's toolbar, **Export project registrations for Tauri**
saves project names and directories to a versioned JSON file. In the Tauri
project registry, select that file and click **Import registrations**. Import
accepts at most 1 MiB and 4,096 entries and supports offline projects. It keeps
the existing name and ID for an exact engine/path match. Different spellings
of the same path can remain as separate records.

在 Electron 项目工具栏点击 **导出项目登记到 Tauri**，将项目名称与目录保存为带版本号的
JSON 文件。在 Tauri 项目登记中选择该文件，然后点击 **导入项目登记**。文件上限为 1 MiB
和 4,096 条登记，支持离线工程。引擎和路径完全相同的记录保留现有名称和 ID，同一路径的
不同写法可能作为不同记录保留。

`inspect_unity({ projectRoot, port, allowClientTakeover })` requires an existing absolute project directory,
an explicit nonzero TCP port, and `allowClientTakeover: true`. Omitted, false, or
legacy generic confirmation is denied before a socket is opened. The address is fixed to
`127.0.0.1`; no ports are probed automatically. It connects on a blocking worker,
checks the reported project path, reads the active scene and returns `{ project, scene }`,
then closes the connection. Same-port requests in flight are rejected. Each new
connection can disconnect an existing client, including before project verification;
the UI requires a new acknowledgement for each attempt. This bridge does not
authenticate the peer, so a matching path is not proof of editor identity. The same
project path and port after a restart can refer to a different editor instance.
The third-party Unity MCP plugin may initialize user-level EditorPrefs during
tool discovery even though these commands only query project and scene data.

`inspect_unity({ projectRoot, port, allowClientTakeover })` 要求真实存在的绝对项目根目录、明确指定的非零
TCP 端口及 `allowClientTakeover: true`。缺省、false 或旧的普通确认参数均在连接前拒绝。
地址固定为 `127.0.0.1`，不会自动探测端口。命令在阻塞
工作线程连接、核对对端报告的项目路径、读取活动场景并返回 `{ project, scene }`，随后关闭连接；
同端口并发请求会被拒绝。每次新连接都可能挤掉现有客户端，甚至在校验项目之前就发生；
界面要求每次尝试重新确认。桥接协议不验证对端身份，项目路径匹配不等于编辑器身份可信；
重启后相同项目路径与端口也可能指向另一个编辑器实例。
虽然命令只查询项目和场景，第三方 Unity MCP 插件发现工具时仍可能初始化用户级 EditorPrefs。

The Unreal preview starts no listener on boot. The user chooses a nonzero port and
explicitly starts `start_unreal_listener({ port })`, which binds only `127.0.0.1`.
`list_unreal_instances()` returns registered project announcements, and only an
explicitly chosen `connection_id` can be passed to
`query_unreal_project({ connectionId })`. The latter sends only the read-only
`project.info` request with a 10-second deadline and returns its actual result or
error. The bridge rotates selection IDs on project changes or invalidation; a
disconnected or stale selection fails instead of choosing another instance. No
request is automatically retried. `stop_unreal_listener()` closes the listener,
connections and pending requests before returning. The local protocol does not
authenticate connecting processes; an announced project is not proof of identity.

Unreal 预览启动时不监听。用户选择非零端口并明确启动
`start_unreal_listener({ port })` 后才绑定 `127.0.0.1`。
`list_unreal_instances()` 返回已登记的项目，用户明确选择的 `connection_id`
可传给 `query_unreal_project({ connectionId })`。该命令仅发送只读的
`project.info` 请求，期限为 10 秒，返回实际结果或错误。项目变化或登记失效时，桥接层
会轮换选择 ID；断线或失效的选择会失败，不会自动改选，也不会自动重试。
`stop_unreal_listener()` 会等待监听器、连接和待处理请求清理后返回。本地协议不验证连接
进程的身份，项目登记信息不能证明身份。

`launch_godot({ executable, project })` requires an explicitly selected absolute
native executable and an absolute project directory. The user's launch action
authorizes starting that editor; the host acknowledges the crate's v1 protocol
boundary internally. It starts the selected interactive editor with the
session crate's fixed arguments on a blocking worker. The project must already
have the addon enabled; no plugin is installed or project copied. Launch returns
only a host-generated opaque `sessionId` and `starting` state. No token or port is
returned to the WebView. `connect_godot`, `query_godot_project`,
`query_godot_scene`, `status_godot_session`, and `release_godot_session` require
that exact ID. They do not discover, attach, poll, or reconnect automatically.
Initial connection attempts are explicit; an established disconnected session is terminal.

Godot's v1 protocol does not authenticate the server, sends its token in plaintext
over loopback, and passes it in the editor's environment. Local processes or
project code can read it or forge responses. Launching a project can execute its
scripts and plugins and write project or editor files. Releasing management neither
kills the editor nor revokes the addon; its listener and token may persist until
the editor closes. The preview is not a secure public production integration.

`launch_godot({ executable, project })` 要求明确选择的本机可执行文件绝对路径和项目
目录绝对路径。用户点击启动即授权启动该编辑器；宿主在内部确认 crate 的 v1 协议边界。
命令在阻塞工作线程中使用
session crate 的固定参数启动所选交互式编辑器。项目必须预先启用插件；不会安装插件或
复制项目。启动仅返回宿主生成的不透明 `sessionId` 与 `starting` 状态，不向 WebView
返回令牌或端口。`connect_godot`、`query_godot_project`、`query_godot_scene`、
`status_godot_session` 和 `release_godot_session` 均要求该 ID；不自动发现、附着、
轮询或重连。初始连接由用户主动触发，已建立后断开的会话不可自动恢复。

Godot v1 协议不验证服务器身份，令牌在本机回环上明文传输，并进入编辑器环境。本机
进程或项目代码可能读取令牌或伪造响应。启动项目可能运行脚本和插件并写入项目或编辑器
文件。释放管理不会结束编辑器或撤销插件；监听和令牌可能持续到编辑器关闭。此预览不是
安全的公开生产集成。

The loopback restriction applies to the bridge; Godot's LSP/DAP and project-defined
services may open other network ports. Native executable checks do not authenticate
the binary, and the host does not verify that the addon is enabled. Port allocation
also has a release-before-bind race. A host crash may bypass cleanup, and OS logout
or shutdown can end the editor.

回环限制只适用于桥接；Godot 的 LSP/DAP 和项目定义的服务可能打开其他网络端口。本机
可执行文件检查不能认证程序身份，宿主也不会验证插件是否已启用。端口分配存在释放后
再绑定的竞态。宿主崩溃可能绕过清理，系统注销或关闭也可能结束编辑器。

`uebox-tauri --host-status-json` prints the same status without opening a window;
its `processId` belongs to that diagnostic process. The window's status reports
the process running the window instead.

`uebox-tauri --host-status-json` 无需打开窗口即可输出同一状态；其中 `processId`
属于诊断进程，窗口展示的则是运行窗口的进程。

For local diagnosis, setting `UEBOX_TAURI_TRACE=1` logs the process ID to stderr
when the status is read. It does not enable a network service.

本地排查时，可设置 `UEBOX_TAURI_TRACE=1`，在读取状态时将进程 ID 写到 stderr；
这不会启用网络服务。

## Focused checks / 针对性检查

```sh
cargo test --manifest-path src-tauri/Cargo.toml --test host_binary
cargo test --manifest-path src-tauri/Cargo.toml --test unity_command
cargo test --manifest-path src-tauri/Cargo.toml --test unreal_command
cargo test --manifest-path src-tauri/Cargo.toml --test engine_features
cargo test --manifest-path src-tauri/Cargo.toml --test godot_command
pnpm tauri:ui:test
pnpm exec vue-tsc --noEmit -p src-tauri/ui/tsconfig.json
pnpm tauri:ui:build
```

For the Godot assistant preview, run the scoped Rust checks, build the two
fixtures, then run the Godot UI E2E. It mounts Vue and calls the real Tauri
commands through the mock runtime. The editor and model fixtures use loopback and
temporary projects. No native WebView, real model, or user project is needed.

Godot 助手预览可独立运行以下 Rust 检查、构建两个夹具，再运行 Godot UI E2E。
测试挂载 Vue，通过 Tauri 模拟运行时调用真实命令。编辑器和模型夹具使用环回地址和
临时工程，无需原生 WebView、真实模型或用户工程。

```sh
cargo test --locked --manifest-path src-tauri/Cargo.toml --no-default-features \
  --features godot --lib --test godot_command --test godot_agent_command \
  --test engine_features --test host_binary
cargo build --locked --manifest-path src-tauri/Cargo.toml --no-default-features \
  --features godot --example godot_agent_ui_harness --example godot_editor_fixture
pnpm exec vitest run --config src-tauri/ui/vitest.e2e.config.ts src-tauri/ui/e2e/godot-agent.e2e.test.ts
```

To opt in to the real Godot command test, set `UEBOX_TEST_GODOT_BIN` to a trusted
native executable. This Unix-only test uses temporary projects, a private editor
home, two headless test-owned editors and a loopback model fixture. It verifies
that the Agent reads only the selected editor and that releasing management
leaves the editors running. The test then closes its own editors.

真实 Godot 命令测试需将 `UEBOX_TEST_GODOT_BIN` 指向可信的本机可执行文件。测试使用临时
项目、隔离的编辑器配置目录、两个由测试启动的 headless 编辑器和环回模型夹具。
测试验证助手仅读取所选编辑器、释放管理后编辑器仍在运行，随后关闭自己的编辑器。
该测试仅适用于 Unix。

```sh
UEBOX_TEST_GODOT_BIN=/path/to/Godot cargo test --manifest-path src-tauri/Cargo.toml \
  --no-default-features --features godot-real-editor-test --lib \
  godot::real_editor_test::real_godot_commands_query_the_selected_temporary_editor_and_release_without_killing_it \
  -- --exact
```

The Unreal UI integration test uses a test-only transport into the real Tauri
commands and a Rust WebSocket fixture on an isolated local port. It does not test a
native WebView or a real Unreal Editor. Both UI E2E suites locate fixtures under
`CARGO_TARGET_DIR` when set, or `src-tauri/target` otherwise; use the same value
for the Cargo build and Vitest run.

Unreal 界面集成测试使用仅用于测试的传输调用真实 Tauri 命令，并在隔离的本地端口运行
Rust WebSocket 夹具；它不验证原生 WebView 或真实 Unreal 编辑器。两套 UI E2E 均从
`CARGO_TARGET_DIR` 读取夹具，未设置时使用 `src-tauri/target`。Cargo 构建和 Vitest
运行时需使用相同的值。

```sh
cargo build --locked --manifest-path src-tauri/Cargo.toml --example unreal_ui_harness
pnpm exec vitest run --config src-tauri/ui/vitest.e2e.config.ts src-tauri/ui/e2e/unreal.e2e.test.ts
```
