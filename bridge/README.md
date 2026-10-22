# @dsh-tui-nano/bridge

薄桥接：在官方 dsh profile 进程内装载，独占 `protocol/` 的 `nano.*` 契约、会话挂载账本和关停顺序。

- `runtime/`：Cordis 插件入口、帧解析、fd 3/4 传输、账本、订阅、关停。
- `controls/`：控制宿主。命令目录来自官方 `commands` 服务；模型目录调用官方导出的 `buildModelCatalog`（在注入 `llm` 的子上下文里），切换安装官方 `installModelSelection` 并写官方 `model/selection`；权限读取来自官方投影/预设目录，切换固定走官方 `/permission <preset>` 命令（不直接 `permissionPresets.set()`）。实时事件为 `controls.model` 与 `controls.permission`。两个官方 helper 由 `runtime/host.mjs` 启动时动态导入并注入，`controls/service.mjs` 不在模块加载期导入官方包；因此没有安装官方依赖的裸 checkout 也能跑桥接单测。
- `skills/`：技能与规矩宿主。`nano.skills.list` 只读官方 `ctx.skills.snapshot()` 并附官方命令名；`nano.skills.preview` 按官方 `path` 有界只读；`nano.instructions.snapshot` 折合官方 `agent-instructions` 持久消息的来源、时间与预算提示。只订阅 `skills/change`/`commands/change` 发刷新信号，不自建 watcher，也不解析指令文件。
- `conversation/`：对话宿主。`submit` 按官方 `user-rpc` 源写入 `requestId`，`editQueued`/`deleteQueued`/`steerQueued` 修改官方 inbox，`cancel` 固定 `keepInbox: true`，`snapshot` 返回绝对状态；实时事件为 `conversation.status`、`conversation.inbox` 和 `conversation.stream`。inbox 投影在超过事件帧上限时降级为有界预览并标 `truncated`；单次提交上限 32 KiB。
- `terminal/`：S09 用户终端。通过官方 `ctx.terminalController` 完成 `environment`/`shells`/`list`/`create`/`retain`/`release`/`attach`/`detach`/`rename`/`close`，只把实时帧作为 `nano.terminal.output|state|ack|ended` 通知转发；输入与缩放按 attachment-local 序列校验。它不注册 Agent 工具，也不进入 Monitor 投递路径。
- `schedule/`：调度宿主。`lease.mjs` 用 `schedule.lock` 保证一个根会话一个 scheduler（O_EXCL、0600、无心跳、死 pid 回收、损坏锁 30 秒门槛）；`backend.mjs` 注册 `nano-schedule-json`、`provide` 对应 service key、用 `nano_probe` 探针验证路由和落盘；`service.mjs` 把目录/列表/创建/比较并交换修改/删除/历史转发给官方 `ctx.schedule`，并转发 `schedule.changed`。存储或路由不成立时启动报 `unsupported`，不挂调度。
- `attachments/`：S06 附件宿主。`nano.attachment.begin|chunk|finish|discard` 以 48 KiB 原始 chunk 接收图片和通用文件；图片只暂存，提交时经官方 `ctx.attachments.admitPromptContent()` 校验、规范化和持久化；通用文件在 `finish` 时经官方 `saveFileStream()` 按原字节保存并生成 `FileAttachmentRef`，提交时校验归属并转交。回执在 inbox/durable 观察后退休，已 commit 未退休的回执不能再次提交。`nano.fileReferences.list` 调用官方 `ctx.fileReferences`，只给路径候选。
- `sessions/`：会话宿主。`list` 只读官方会话查询与投影缓存，不打开冷会话正文；`rename` 只接受当前活动根会话（冷会话会 resume/拿写锁，直接拒绝）；`context` 返回同一 `contextPressure` 投影的 `projectedTokens`/`contextWindow` 与独立的计费用量、启发式构成、记录时刻；`compact`/`compactCancel` 走官方 `/compact` 命令，原样返回官方稳定文本。实时事件 `sessions.context` 合并推送上下文参考值。
- `shell/`：`!command` 人工命令。每次调用直接走官方 `ctx.shell`，在单次 `ShellExecRequest` 上传 `ctx.sandboxPolicy.resolve({ mode: 'danger-full-access' })`，显式 `timeoutMs: 60000`，不改 profile 默认值；输出以 `<bash-input>/<bash-stdout>/<bash-stderr>` 合成 user 上下文（source `bash-mode`）走官方 `inject`，不唤醒驱动，也不分配 job ID。`!!` 走同一个 shell 路径但 `inject: false`，只在本窗口显示，不进入上下文。非零退出/信号/超时是结果，不是 RPC 错误；只有进程根本没起来才报 `nano/shell/exec-failed`。注入文本按 JSON 后的字节量化到 200,000 以下，必要时标截断并给 spill 路径。
- `jobs/`：官方 `ctx.jobs` 的只读观察层。`snapshot` 返回官方投影和桥接事件序号 `seq`，按官方 `job/registered` 补上所属调用；`follow` 只用 `readAt` 按绝对偏移非消费读取；`kill` 固定原因 `cancelled by the user`，只回答 `requested`/`already-finished`，只有 `unknown job`/foreign 才映射 `job/not-found`，producer 取消抛错等保持内部错误。事件为 `jobs.event` 与 `jobs.output`；不维护第二份任务表，不挂 job controller，也不发送完成通知（通知渠道仍由官方 `dsh-tool-jobs` 唯一拥有）。`!` 注入的合成消息在下一步被领取前会作为官方 inbox 的待输入项出现，这是官方投影，不是第二份队列。
- `cordis.patch.yml`：bundle 的固定行集（bridge、`tool-ask-user`、关闭 HMR），并额外挂载 `@deepseek-ai/dsh-file-reference-local`，因为最小 `dsh-base` 组合不包含 `@file` 发现 provider。
- `decisions/`：S03 的审批与问卷 answerer。`approval/request` 与 `user-questions/request` 各自等待 Rust 的答复，审批只回官方四种 outcome，问卷答案按 `{id, selected, custom?}` 原样回传；限时问卷会 claim 官方 wait 并发布 `remainingMs`，超时以 `ASK_TIMED_OUT` 结束、问题转为 continued；`nano.decisions.snapshot` 在重连后恢复 pending 卡与 `userQuestions` 投影。
- `profile/install.mjs`：初始化 `$DSH_HOME/profiles/dsh-tui-nano` 并安装本包（`dsh plugin --profile ... install`）。

运行前提：由 Rust `nano-runtime` 通过 `dsh --profile dsh-tui-nano --patch <runtime-patch>` 启动；`--patch` 提供每会话 storage 根和 `nano-bridge` 配置。手动直接运行该 profile 会因缺配置报 `nano.startupError: startup-failed`，这是预期行为。

诊断只写 stderr；协议帧只在 fd 3/4（或带 nonce 的 stdin/stdout 回退）上。
