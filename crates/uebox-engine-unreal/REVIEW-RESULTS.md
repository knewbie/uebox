# UE crate 审查问题修复记录

本记录只覆盖本 crate；审查依据为 workspace 中的 `review-ue.md`。没有连接真实 UE。

## 项目快照选择竞态：红灯

先新增 `stale_list_snapshot_cannot_select_a_changed_project_on_the_same_connection`，保存 First 的列表项，再让同一 WebSocket 公布 Second，并用旧项选择。

执行：
`CARGO_TARGET_DIR="$PWD/crates/uebox-engine-unreal/target" cargo test --manifest-path crates/uebox-engine-unreal/Cargo.toml --test protocol stale_list_snapshot_cannot_select_a_changed_project_on_the_same_connection -- --exact`

结果：退出码 101；旧列表项仍被 `select_instance` 接受，断言失败，复现审查结论。

修复后同一测试通过：公开选择 ID 在项目身份变化或原身份失效时轮换，旧列表项及保留的旧凭据不能选中新身份；原 `SelectedInstance` 仍按代次失效。

## 静默与未登记连接：红灯

先编写短期限的本机 WebSocket 测试，并仅提供暂时忽略期限配置的 API 骨架以运行测试：

- `cargo test --manifest-path crates/uebox-engine-unreal/Cargo.toml --test protocol silent_registered_connection_expires_and_fails_pending_without_a_close_frame -- --exact`：退出码 101；静默连接 2 秒内未被清理，等待请求未得到断线错误。
- `cargo test --manifest-path crates/uebox-engine-unreal/Cargo.toml --test protocol unregistered_connection_has_a_deadline_even_if_it_answers_pings -- --exact`：退出码 101；未登记连接 2 秒内未关闭。

之后才实现 ping、有效活动期限和独立登记期限。

## 修复与接口

- `InstanceSummary.connection_id` 仍是 `select_instance(&str)` 的输入，但现在是与当前项目身份绑定的不透明选择凭据。同连接重报不同项目、无效项目信息导致原身份失效时轮换；即使旧项目名和路径后来重现，旧凭据也不能重新授权。调用方必须根据新列表明确重新选择。
- `UnrealBridge::bind_with_liveness(port, LivenessConfig)` 提供可测试的期限配置；`bind` 和 `start` 默认每 15 秒发送 WebSocket ping，45 秒无入站活动则清理连接，未登记连接另有从握手完成开始的 45 秒登记期限。失去已有登记后会开始新的登记期限。
- 接收的 WebSocket 消息和控制帧（包括插件 `system.heartbeat`、pong）按连接分别计作活动。未登记连接即使持续发送心跳或回答 ping，也不能无限期占用连接。超时清理沿用连接 guard，清除实例、关闭 socket 并令等待中的请求返回 `Disconnected`。
- 项目查询的单次 `Timeout` 不会触发连接失效；仍只允许显式选择后的只读 `project.info` 查询，不自动重试。监听仍只绑定 IPv4 环回地址。

## 实际验证

在当前 macOS 环境中，仅运行本 crate 的针对性验证：

- `CARGO_TARGET_DIR="$PWD/crates/uebox-engine-unreal/target" cargo test --manifest-path crates/uebox-engine-unreal/Cargo.toml --test protocol`：24 项通过，0 项失败。包括旧列表项先取后选、身份回切、已保存选择、静默断线和等待请求、心跳与 pong、连接之间的存活隔离、未登记期限及持续活动、重新登记、单次查询超时、配置校验；关键竞态与存活测试使用多线程 Tokio runtime。
- `CARGO_TARGET_DIR="$PWD/crates/uebox-engine-unreal/target" cargo test --manifest-path crates/uebox-engine-unreal/Cargo.toml --lib`：1 项通过，检查请求超时和取消时等待项清理。
- `cargo fmt --manifest-path crates/uebox-engine-unreal/Cargo.toml -- --check`：通过。
- `CARGO_TARGET_DIR="$PWD/crates/uebox-engine-unreal/target" cargo clippy --manifest-path crates/uebox-engine-unreal/Cargo.toml --all-targets -- -D warnings`：通过。

协议测试使用真实本机临时端口上的 Rust WebSocket 夹具；没有连接真实 UE，也没有运行 Tauri 宿主或 Windows 构建。没有运行全仓 `pnpm verify`，没有修改其他 crate、插件、用户工程或本 crate 之外的报告文件。
