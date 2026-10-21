# @dsh-tui-nano/bridge

薄桥接：在官方 dsh profile 进程内装载，独占 `protocol/` 的 `nano.*` 契约、会话挂载账本和关停顺序。

- `runtime/`：Cordis 插件入口、帧解析、fd 3/4 传输、账本、订阅、关停。
- `controls/`：控制宿主。命令目录来自官方 `commands` 服务；模型目录来自官方 LLM 注册表（或已组合的 `sessionController`），切换写官方 `model/selection` 并安装与 `installModelSelection` 相同的路由钩子；权限读取来自官方投影/预设目录，切换固定走官方 `/permission <preset>` 命令（不直接 `permissionPresets.set()`）。实时事件为 `controls.model` 与 `controls.permission`。
- `conversation/`：对话宿主。`submit` 按官方 `user-rpc` 源写入 `requestId`，`editQueued`/`deleteQueued`/`steerQueued` 修改官方 inbox，`cancel` 固定 `keepInbox: true`，`snapshot` 返回绝对状态；实时事件为 `conversation.status`、`conversation.inbox` 和 `conversation.stream`。inbox 投影在超过事件帧上限时降级为有界预览并标 `truncated`；单次提交上限 32 KiB。
- `profile/install.mjs`：初始化 `$DSH_HOME/profiles/dsh-tui-nano` 并安装本包（`dsh plugin --profile ... install`）。
- `cordis.patch.yml`：bundle 的固定行集（bridge、`tool-ask-user`、关闭 HMR）。

运行前提：由 Rust `nano-runtime` 通过 `dsh --profile dsh-tui-nano --patch <runtime-patch>` 启动；`--patch` 提供每会话 storage 根和 `nano-bridge` 配置。手动直接运行该 profile 会因缺配置报 `nano.startupError: startup-failed`，这是预期行为。

诊断只写 stderr；协议帧只在 fd 3/4（或带 nonce 的 stdin/stdout 回退）上。
