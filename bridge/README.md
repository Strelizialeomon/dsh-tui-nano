# @dsh-tui-nano/bridge

薄桥接：在官方 dsh profile 进程内装载，独占 `protocol/` 的 `nano.*` 契约、会话挂载账本和关停顺序。

- `runtime/`：Cordis 插件入口、帧解析、fd 3/4 传输、账本、订阅、关停。
- `conversation/`：对话宿主。`submit` 按官方 `user-rpc` 源写入 `requestId`，`editQueued`/`deleteQueued`/`steerQueued` 修改官方 inbox，`cancel` 固定 `keepInbox: true`，`snapshot` 返回绝对状态；实时事件为 `conversation.status`、`conversation.inbox` 和 `conversation.stream`。inbox 投影在超过事件帧上限时降级为有界预览并标 `truncated`；单次提交上限 32 KiB。
- `shell/`：`!command` 人工命令。每次调用直接走官方 `ctx.shell`，在单次 `ShellExecRequest` 上传 `ctx.sandboxPolicy.resolve({ mode: 'danger-full-access' })`，不改 profile 默认值；输出以 `<bash-input>/<bash-stdout>/<bash-stderr>` 合成 user 上下文（source `bash-mode`）走官方 `inject`，不唤醒驱动，也不分配 job ID。非零退出/信号/超时是结果，不是 RPC 错误；只有进程根本没起来才报 `nano/shell/exec-failed`。
- `jobs/`：官方 `ctx.jobs` 的只读观察层。`snapshot` 返回官方投影并按官方 `job/registered` 补上所属调用；`follow` 只用 `readAt` 按绝对偏移非消费读取；`kill` 固定原因 `cancelled by the user`，只回答 `requested`/`already-finished`。事件为 `jobs.event` 与 `jobs.output`；不维护第二份任务表，也不发送完成通知（通知渠道仍由官方 `dsh-tool-jobs` 唯一拥有）。`!` 注入的合成消息在下一步被领取前会作为官方 inbox 的待输入项出现，这是官方投影，不是第二份队列。
- `profile/install.mjs`：初始化 `$DSH_HOME/profiles/dsh-tui-nano` 并安装本包（`dsh plugin --profile ... install`）。
- `cordis.patch.yml`：bundle 的固定行集（bridge、`tool-ask-user`、关闭 HMR）。

运行前提：由 Rust `nano-runtime` 通过 `dsh --profile dsh-tui-nano --patch <runtime-patch>` 启动；`--patch` 提供每会话 storage 根和 `nano-bridge` 配置。手动直接运行该 profile 会因缺配置报 `nano.startupError: startup-failed`，这是预期行为。

诊断只写 stderr；协议帧只在 fd 3/4（或带 nonce 的 stdin/stdout 回退）上。
