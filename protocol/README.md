# Nano protocol v1

`nano-protocol.json` 是 Rust 运行时与 TypeScript 桥接之间唯一的机器可校验契约。两边的测试都读取它，核对：

- 方法名、方向、实现状态（`implemented` / `reserved`）；S02 的对话方法为 `nano.conversation.submit|editQueued|deleteQueued|steerQueued|cancel|snapshot`；S04 的控制方法为 `nano.controls.state|modelCatalog|selectModel|setPermission|executeCommand`；S05 的 `nano.skills.list|preview` 与 `nano.instructions.snapshot`；S06 的 `nano.attachment.begin|chunk|finish|discard` 与 `nano.fileReferences.list`；S07 的会话方法为 `nano.sessions.list|rename|context|compact|compactCancel`；S08 的 `nano.shell.exec` 与 `nano.jobs.snapshot|follow|kill`；`session.event` 载荷可带官方 `time`、`surfaceOp`、`sourceEventSeqs`；
- 事件 kind：`conversation.status`、`conversation.inbox`（完整投影；超帧上限时降级为有界预览，带 `truncated`/`total`/`omitted`）、`conversation.stream`（start/chunk/end 帧）、`controls.model`（持久模型投影的绝对替换）、`controls.permission`（官方当前预设与进程目录的绝对替换）、`skills.changed`（仅刷新信号）、`instructions.changed`（完整投影）、`sessions.context`（上下文占用参考值）、`jobs.event`（官方任务投影的 registered/progress/stopping/settled/removed）与 `jobs.output`（只通告绝对偏移，不推输出内容）；`nano.jobs.follow` 用 `readAt` 按绝对字节偏移非消费读取；`jobs.event`/`jobs.output`/`nano.jobs.snapshot` 都带桥接事件序号 `seq`，客户端用它拒绝过期快照；`nano.shell.exec` 的 `inject: false` 表示 `!!`（本窗口显示、不注入上下文）；
- 帧必需字段、错误码、生命周期边、事件 kind；
- 容量上限（frame、pending、chunk、背压队列、退出预算、单次提交 32 KiB）；S06 的 `hello.attachmentLimits` 是可选的部署图片限额（类型、字节、像素、边长、每消息数量与总量、chunk 大小）与 `fileAdmission` 能力声明；
- `fixtures` 里的样本必须能被双方解析，包括 `conversationSubmit`、`conversationSubmitAttachments`、`conversationEvent`、`skillsListResult`、`skillPreviewResult`、`instructionsEvent`、`shellExec` 与 `jobsEvent`。

`@dsh-tui-nano/protocol` 是它最小的 Node 包装（只读、深冻结、零依赖）。Rust 侧在 `crates/nano-runtime/tests/contract.rs` 里用 `include_str!` 读取同一份 JSON。

## 订阅语义

`nano.subscribe` 的 `fromSeq` 省略时只接收订阅之后的新事件，结果是当前 `seq` 基线；提供时先按顺序重放 `[fromSeq, 当前 seq)` 的持久事件，再切到实时事件。重放以官方持久化读取为准，实时监听先注册并缓冲，重叠的 seq 去重。慢消费者会被显式关闭（`nano.streamEnded`，`outcome: failed`），不会静默丢事件。

## 改协议

先改 JSON，再跑两边的契约测试；不要只改一侧的枚举。`bridge/test/*.test.mjs` 与 `cargo test --test contract` 会逐项核对方法、限制、错误码、生命周期边、事件 kind 与样本。
