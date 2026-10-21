# @dsh-tui-nano/bridge

薄桥接：在官方 dsh profile 进程内装载，独占 `protocol/` 的 `nano.*` 契约、会话挂载账本和关停顺序。

- `runtime/`：Cordis 插件入口、帧解析、fd 3/4 传输、账本、订阅、关停。
- `conversation/`：对话宿主。`submit` 按官方 `user-rpc` 源写入 `requestId`，`editQueued`/`deleteQueued`/`steerQueued` 修改官方 inbox，`cancel` 固定 `keepInbox: true`，`snapshot` 返回绝对状态；实时事件为 `conversation.status`、`conversation.inbox` 和 `conversation.stream`。inbox 投影在超过事件帧上限时降级为有界预览并标 `truncated`；单次提交上限 32 KiB。
- `sessions/`：会话宿主。`list` 只读官方会话查询与投影缓存，不打开冷会话正文；`rename` 只接受当前活动根会话（冷会话会 resume/拿写锁，直接拒绝）；`context` 返回同一 `contextPressure` 投影的 `projectedTokens`/`contextWindow` 与独立的计费用量、启发式构成、记录时刻；`compact`/`compactCancel` 走官方 `/compact` 命令，原样返回官方稳定文本。实时事件 `sessions.context` 合并推送上下文参考值。
- `profile/install.mjs`：初始化 `$DSH_HOME/profiles/dsh-tui-nano` 并安装本包（`dsh plugin --profile ... install`）。
- `cordis.patch.yml`：bundle 的固定行集（bridge、`tool-ask-user`、关闭 HMR）。

运行前提：由 Rust `nano-runtime` 通过 `dsh --profile dsh-tui-nano --patch <runtime-patch>` 启动；`--patch` 提供每会话 storage 根和 `nano-bridge` 配置。手动直接运行该 profile 会因缺配置报 `nano.startupError: startup-failed`，这是预期行为。

诊断只写 stderr；协议帧只在 fd 3/4（或带 nonce 的 stdin/stdout 回退）上。
