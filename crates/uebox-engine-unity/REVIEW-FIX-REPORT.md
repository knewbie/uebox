# Unity 独立审查修复报告

## Tauri / Unity 审查 U2、U3 后续修复（2026-11-17）

- 未改变公开 API；`Target` 仍保存调用方选择的路径。连接在接触 TCP 前解析并保存该路径的规范结果；之后项目校验只比较选定路径和该规范结果，不对编辑器回包中的路径进行文件系统解析。`target()` 仍返回原始选择。
- Windows 只接受盘符根路径；对端的普通/扩展 UNC、设备命名空间和不受支持的表示均拒绝，选择的路径及其规范结果也不能是 UNC。普通与扩展盘符形式仅对保守的安全 ASCII 路径分量互认，且不忽略大小写。不在选定值或保存的规范值中的别名会保守拒绝。所选路径本身在建连前仍会调用 `canonicalize`，它可能访问映射网络驱动器，且不受 TCP 超时约束。
- macOS/Linux 可将用户选择的符号链接与编辑器报告的规范路径匹配；同目录的其他未知别名仍会拒绝，不解析对端路径。这是对建连前路径字符串及规范结果的比较，不持有目录句柄，不证明后续文件系统没有变化，也不认证对端。协议仍不能证明跨重启实例身份；仅使用已建立的连接，不自动重连或暗中重选。
- 测试先行：修复前，UNC 路径用例中的注入解析器被调用两次；不受信任的本地路径也被传给解析器；选中临时项目的符号链接时，规范路径回包错误返回 `ProjectMismatch`。其他新增的 UNC 目标、规范结果为 UNC 和有歧义的 Win32/扩展路径断言也先观察到失败。设备路径包含在 UNC 测试中，但修复前该测试在首个 UNC 断言失败后即停止。
- macOS 验证：完整 crate 测试执行时 **8 项单元测试、22 项环回 TCP 协议测试、3 项文档测试通过**。随后补充真实目录匹配断言，将单元测试改为 **9 项**，单独重跑该组全部通过；协议测试与文档测试未再改动。`cargo fmt --manifest-path crates/uebox-engine-unity/Cargo.toml -- --check` 和 `cargo clippy --offline --locked --manifest-path crates/uebox-engine-unity/Cargo.toml --all-targets -- -D warnings` 通过。未运行全仓 gate。
- 注入测试在 macOS 上执行 Windows 路径匹配逻辑，并断言 UNC/设备路径使解析器调用次数为零；本机没有 Windows Rust target 或 Windows 编辑器，不能宣称已在 Windows 编译或实测。TCP 测试只使用自建随机环回监听器并排除 6400，符号链接位于临时目录；未连接用户 Unity 或修改用户工程。

## 协调者：公开 API 变更

本次修复只修改 `crates/uebox-engine-unity/`。以下接口约定在实现前已写入本报告并在 pane 中通知，供 Tauri 集成同步：

- 旧的 `UnityClient::connect(Target, BridgeOptions)` 返回 `BridgeError::AuthorizationRequired`，不会创建 TCP 连接。它保留为显式失败的兼容入口，不再提供无授权连接。
- 调用方必须先针对具体 `Target` 获得用户的明确接管授权，之后调用 `Target::authorize_takeover()` 生成一次性授权目标，再交给 `UnityClient::connect_authorized(...)`。授权目标绑定地址和项目，不能用作其他目标或隐式重连；授权创建本身不是替代用户确认的 UI。
- 即使已有授权，连接前也会要求所选项目根路径能解析为实际目录；路径不存在或是文件时返回 `InvalidTarget`，不接触 TCP 端口。
- `identity()` 已改名为 `last_observed_project_info()`；它只表示先前读到的项目数据，不证明当前连接在线。需要最新项目状态时使用会进行网络请求的 `project_info()`。
- 新增同步 `ping()`：调用方可主动在现有连接上检查 Coplay 的 `ping`/`pong`，也可自行安排保活；不会启动后台任务、创建新连接或验证跨重启实例身份。
- 首次远端 EOF、连接重置、断开或写入断链返回 `BridgeError::Disconnected`；不可把上次快照作为在线结果。
- Coplay 协议不能证明重启后同项目、同端口是否仍为原编辑器实例。此库只对当前建立的连接提供只读查询，不自动重连、不重新选择实例；重新连接仍需要新的明确授权和目标选择。

## 验证记录

- 测试先行：修复前分别观察到无授权连接实际触达监听器、首次 EOF 返回普通 I/O 错误、旧 `identity()` 编译成功；Windows 比较的大小写、扩展路径及未解析路径三个断言也按预期失败。后续针对真实目录解析、显式接管、`ping()` 以及连接前拒绝不存在目录和文件的失败测试同样先运行，再完成实现。
- `cargo test --offline --manifest-path crates/uebox-engine-unity/Cargo.toml -- --test-threads=1`：**4 项单元测试、21 项环回 TCP 协议测试、3 项编译失败文档测试通过，0 失败、0 忽略**。测试覆盖未授权时不建立第二条连接且旧客户端仍可通信；显式授权后模拟 Coplay 挤掉旧客户端；连接前拒绝不存在的目录和文件；首次 EOF；坏 pong 后断开；原端口被替换时不自动重连；Windows 大小写、扩展路径和解析失败的比较逻辑。
- `cargo fmt --manifest-path crates/uebox-engine-unity/Cargo.toml -- --check` 通过；`cargo clippy --offline --manifest-path crates/uebox-engine-unity/Cargo.toml --all-targets -- -D warnings` 通过。Cargo 使用本任务此前的隔离缓存；没有运行全仓 gate。
- 测试监听器由操作系统分配环回端口并明确排除 6400；没有连接用户 Unity 会话或修改用户工程。没有重启真实 Unity：独立审查指出即使临时项目也可能写入用户级 Unity EditorPrefs，本次没有隔离 OS 用户环境。

## 实现边界和待集成事项

- 此前 Windows 两边分别 `canonicalize` 的实现及其验证记录已被上方 U2 修复取代。当前仍没有 Windows 编译或实机验证；应在隔离环境验收大小写敏感 NTFS、常规路径和扩展路径的行为。
- Coplay 的握手和项目路径不认证对端，也没有可核验的跨重启进程身份；`ping()` 只检查当前连接。授权令牌是调用方对用户许可的显式声明，Tauri 必须先完成用户确认，不能仅凭程序自动构造令牌冒充用户授权。
- 文档明确区分只读项目/场景查询与零编辑器设置副作用：Coplay 首次工具发现可能初始化用户级 EditorPrefs。仅修改客户端无法消除该插件行为。
