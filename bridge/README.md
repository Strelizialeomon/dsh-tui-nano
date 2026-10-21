# @dsh-tui-nano/bridge

薄桥接：在官方 dsh profile 进程内装载，独占 `protocol/` 的 `nano.*` 契约、会话挂载账本和关停顺序。

- `runtime/`：Cordis 插件入口、帧解析、fd 3/4 传输、账本、订阅、关停。
- `skills/`：技能与规矩宿主。`nano.skills.list` 只读官方 `ctx.skills.snapshot()` 并附官方命令名；`nano.skills.preview` 按官方 `path` 有界只读；`nano.instructions.snapshot` 折合官方 `agent-instructions` 持久消息的来源、时间与预算提示。只订阅 `skills/change`/`commands/change` 发刷新信号，不自建 watcher，也不解析指令文件。
- `conversation/`：对话宿主。`submit` 按官方 `user-rpc` 源写入 `requestId`，`editQueued`/`deleteQueued`/`steerQueued` 修改官方 inbox，`cancel` 固定 `keepInbox: true`，`snapshot` 返回绝对状态；实时事件为 `conversation.status`、`conversation.inbox` 和 `conversation.stream`。inbox 投影在超过事件帧上限时降级为有界预览并标 `truncated`；单次提交上限 32 KiB。
- `schedule/`：调度宿主。`lease.mjs` 用 `schedule.lock` 保证一个根会话一个 scheduler（O_EXCL、0600、无心跳、死 pid 回收、损坏锁 30 秒门槛）；`backend.mjs` 注册 `nano-schedule-json`、`provide` 对应 service key、用 `nano_probe` 探针验证路由和落盘；`service.mjs` 把目录/列表/创建/比较并交换修改/删除/历史转发给官方 `ctx.schedule`，并转发 `schedule.changed`。存储或路由不成立时启动报 `unsupported`，不挂调度。
- `profile/install.mjs`：初始化 `$DSH_HOME/profiles/dsh-tui-nano` 并安装本包（`dsh plugin --profile ... install`）。
- `cordis.patch.yml`：bundle 的固定行集（bridge、`tool-ask-user`、关闭 HMR）。

运行前提：由 Rust `nano-runtime` 通过 `dsh --profile dsh-tui-nano --patch <runtime-patch>` 启动；`--patch` 提供每会话 storage 根和 `nano-bridge` 配置。手动直接运行该 profile 会因缺配置报 `nano.startupError: startup-failed`，这是预期行为。

诊断只写 stderr；协议帧只在 fd 3/4（或带 nonce 的 stdin/stdout 回退）上。
