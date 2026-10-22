# S10 Monitor 证据（2026-10-22）

执行单：#13。规格：[S10 Monitor](../../specs/2026-10-20-nano-v1/S10-monitor.md)。PR：[#33](https://github.com/Strelizialeomon/dsh-tui-nano/pull/33)。

## 结论

- 桥接与 Rust 模型完成：有界、非消费的任务输出监听；行合并、字面/正则过滤、`notify`/`quiet` 投递、预算/有效期、停止监听、缺口/裁剪可见、终态只进 UI。
- 重审发现的 2 个 blocker 已修：流结束未换行尾部现在会补投；running job 选择「从开头」现在会在附着时立即重放 retained head。major 中的能力/事件显式失败、正则复杂度、旧代际投影、订阅重建刷新、官方完成通知证据也已处理。
- 真实模型会话验证了 `quiet` 与 `notify` 两种投递、running-job 重放、无换行尾部、官方完成通知恰好一条，以及 durable `user/message` 收件事实。
- 真实 tmux TUI 验证了附着前「已有 N 字节未读」、从开头重放、过滤、预算计数、`role=user / source=monitor`、`stream-ended`、停止监听不影响任务。
- 未验证：长时间空闲（数分钟以上）的 `quiet` 注入；本份只验证了空闲数秒后由普通用户回合唤醒。没有跑网络监听、文件 tail 或 MCP monitor（规格明确不在首版范围）。

## 真实模型证据

文件：[model-evidence.txt](model-evidence.txt)

- `CAPABILITY`：`monitor` 为 `supported`。
- running-job 重放：任务仍在 `running` 时（已产生 31 字节 retained 输出）以 `from:"earliest"` 附着，`QUIET_RUNNING_REPLAY` 在 settle 前就得到 `used.lines:1`、`stream:"watching"`、`offset:31`，通知首/末行都是 `monitor-line-1`。
- quiet：后台任务输出 4 条匹配行（含最后一条无换行的 `monitor-line-tail`）+ 1 条噪声行；过滤器 `monitor-line`，噪声行 `noiseDelivered:false`。3 条通知先为 `inserted`（未唤醒）。
- 普通用户回合后，3 条都变为 `accepted`；`QUIET_DURABLE_USER_MESSAGE.present=true`，3 个 monitor 消息 id 都出现在 durable `user/message` 中，且 `tailPresent:true`（无换行尾部确实进入模型收件内容）。
- 官方完成通知：第一个任务 `OFFICIAL_COMPLETION_NOTICE.count=1`，第二个任务也为 `1`；S10 没有额外发终态 model 通知，也没有抑制官方通知。
- notify：第二个后台任务输出 `notify-line-42`；监听以 `notify` 模式创建，回执为 `accepted`（durable user/message），没有用 `inserted`/`claimed` 冒充收件。
- 第一个任务 `settled` 为 `completed`，监听 `stream-ended`，`streamDetail: exit code: 0`。
- 停止监听后 `jobs.snapshot` 仍包含两个任务：停止监听没有停止任务。
- 费用记录：本次会话 `tokenUsage = {uncachedInputTokens: 14732, outputTokens: 1098, cacheReadTokens: 124928}`，`projectedTokens: 20970 / 1000000`（`nano.sessions.context` 在关机前读取）。

## 真实 TUI 证据

文件：[pty-evidence.txt](pty-evidence.txt)

- 模型在同一会话里先通过官方 `job_output` 读完了 5 行输出（含最后无换行的 `monitor-line-tail`），随后监听仍能从 `earliest` 重放同一批 retained 字节；这证明监听使用独立偏移，不依赖也不改写模型的消费游标。
- 附着表单显示 `已有 78 字节未读`，默认「只看新输出」；选择「从开头」后重放。
- 过滤器 `monitor-line` 排除了 `noise-ignore-me`。
- 4 条匹配行在 800 ms 合并窗口内合并成一条通知（`通知 1/50 · 行 4`），通知只带首行、末行和 `omitted=2`；队列里的可见文本是 `first@0: monitor-line-1 … last@61: monitor-line-tail`。
- 监听面板显示 `role=user / source=monitor（user-role 消息，不是 Agent 授权）`。
- 空闲时该通知为 `inserted`；普通用户回合后变为 `已收件（durable user/message） · 4 行 · 0..61`。
- 终态显示 `stream-ended（流结束；不代表条件满足）`，原因是 `exit code: 0`。
- 按 `s` 后显示「用户已停止监听；被观察任务未受影响」，任务列表仍有该任务。
- 预算只验证了计数显示（`通知 1/50`）；耗尽/过期/重置由单元测试覆盖，未在真实 TUI 里跑耗尽。

## 自动测试

- [cargo-test.txt](cargo-test.txt)：`cargo test --workspace` 全绿。
- [bridge-test.txt](bridge-test.txt)：`npm run test:bridge`，246 passed / 0 failed / 5 skipped。
- [host-e2e.txt](host-e2e.txt)：`NANO_E2E=1 node --test bridge/test/integration/host.test.mjs`，3/3 通过；包含 `nano.monitor.list` 空列表与不存在任务返回 `1028` 的断言。
- [versions.txt](versions.txt)：本机工具版本。

新增/加强的回归覆盖：

- 流结束无换行尾部、超长行尾部不丢；running job 的 earliest 立即重放。
- 灾难性回溯正则被拒绝；缺 `readAt`/事件流时返回 `nano/monitor/unavailable`。
- 旧代际迟到回执不能覆盖新监听；`deliveries` Map 有上限。
- stop 后 settled 的匹配行计入 `droppedMatches`，不再伪装成可恢复的 held。
- 过期停止投递、显式 reset 恢复；`pre-step reject` 与 `turn/end blocked` 归类为投递失败。
- 订阅 owner 过滤、空 chunks 但 `next` 前进标为可见缺口。
- Rust 端：缺 `monitor` 投影是显式错误；notice/error 同时到达都不丢；过滤编辑按 UTF-8 字节封顶；监听面板 PageUp/PageDown 与顶部锚定视图一致；create 响应先于 live 事件时延迟选中。

## 实施期确定的值（规格 §9 留给实施）

- 过滤器语法：字面 substring；`regex` 为 `true` 时按 JavaScript RegExp 编译。pattern 上限 512 UTF-8 字节；拒绝反向引用、嵌套量词（如 `(a+)+`）和超过 64 个量词记号，避免灾难性回溯。
- 默认合并窗口 800 ms；最小投递间隔 1000 ms；单条最多 20 行；单条最多 8192 UTF-8 字节 / 2048 估算 token（`ceil(bytes/4)`）。超过 20 行的窗口会拆成多条，每条仍带首/末行和省略计数。
- 默认最多 50 条通知；有效期 30 分钟；单个宿主最多 32 个监听；未换行的长行超过 16384 字节时按块处理并标记 `long-line block`；流结束时剩余未换行尾部补投。
- 预算耗尽/过期后仍继续读取并保留一个有界「暂存」摘要（首/末行 + 计数），UI 显式显示；只有用户通过 `nano.monitor.resetBudget` 才能重置。重置后的首次补投是一个摘要，可能覆盖多个窗口；内容仍只有首/末行且受字节/token 上限约束。
- 同一任务的活动监听重复创建返回现有记录；已停止/已结束的记录在再次 `create` 时被新的监听代际替换，旧代际的迟到回执不会覆盖新投影。停止监听不会删除记录。
- 终端态映射：`completed → stream-ended`、`killed → stopped`、`failed → failed`；任务记录被移除映射为 `stream-ended` + `任务记录已从官方列表移除`。
- 可用性：capability 与 `assertAvailable` 都要求官方 jobs 暴露 `readAt` 与 `events.subscribe`，且 Agent 暴露 `followup`/`inject`；否则返回 `nano/monitor/unavailable`（1057），不返回一个永远看不见新输出的监听。
- UI 位置：Monitor 暂作为 S08 任务面板的子视图（选中任务按 `m`），不新增全局键位或主题；面板内 `n/a/t/f/r/s/b` 为临时操作键，待 S13 design-contract 冻结时统一。

## 重审处置

完整发现原样贴在 PR #33 的两条评论（规格符合性 1 blocker / 4 major / 7 minor；对抗找 bug 11 条 + 2 条待复核）。本次处置：

- blocker：流结束未换行尾部丢失 → `flushCarry`；长行尾部同样补投；真实模型 + TUI 证据包含 `monitor-line-tail`。
- blocker：running job 的 earliest 不重放 → `create` 对 `from:"earliest"` 立即 `drain`；真实模型证据在 settle 前得到 replay。
- major：能力/事件缺失静默 → capability 与 `assertAvailable` 显式检查 `readAt` / `events.subscribe`，抛 1057。
- major：正则复杂度 → 拒绝反向引用、嵌套量词与量词过多。
- major：旧代际迟到回执覆盖新投影 → `publish` 只接受当前代际、替换时清理旧 delivery 条目、Map 封顶。
- major：订阅重建不刷新监听 → Resubscribe 成功分支补 `refresh_monitor_snapshot()`。
- major：官方完成通知证据缺失 → 真实模型日志记录两个任务各恰好一条官方通知。
- minor：stop 后 settled 计入 dropped、ready 队列注释、create 响应早于事件、notice/error 同时保留、过滤 UTF-8 字节封顶、PageUp/PageDown 方向、reset 忽略 mergeWindowMs、过期测试、blocked/reject 归类、空 chunks 缺口、证据措辞。
- 保留并记录：重置后的 held 摘要可能超过 `maxLinesPerNotification`（摘要仍受字节/token 上限约束）；临时键位待 S13 收编；长时间空闲 quiet 未实测。

## 复跑

```sh
cargo build -p nano-app
NANO_EVIDENCE_MODEL=deepseek-flash node scripts/model-evidence-s10.mjs
NANO_EVIDENCE_MODEL=deepseek-flash scripts/pty-evidence-s10.sh
```

两个脚本都安装临时 profile；PTY 脚本使用临时 `DSH_HOME` 和用户凭据副本，不修改 `~/.dsh` 的现有 profile。
