# @dsh-tui-nano/bridge

薄桥接：在官方 dsh profile 进程内装载，独占 `protocol/` 的 `nano.*` 契约、会话挂载账本和关停顺序。

- `runtime/`：Cordis 插件入口、帧解析、fd 3/4 传输、账本、订阅、关停。
- `profile/install.mjs`：初始化 `$DSH_HOME/profiles/dsh-tui-nano` 并安装本包（`dsh plugin --profile ... install`）。
- `cordis.patch.yml`：bundle 的固定行集（bridge、`tool-ask-user`、关闭 HMR）。

运行前提：由 Rust `nano-runtime` 通过 `dsh --profile dsh-tui-nano --patch <runtime-patch>` 启动；`--patch` 提供每会话 storage 根和 `nano-bridge` 配置。手动直接运行该 profile 会因缺配置报 `nano.startupError: startup-failed`，这是预期行为。

诊断只写 stderr；协议帧只在 fd 3/4（或带 nonce 的 stdin/stdout 回退）上。
