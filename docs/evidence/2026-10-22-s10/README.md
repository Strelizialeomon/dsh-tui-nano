# S10 Monitor 证据（2026-10-22）

执行单：#13。规格：[S10 Monitor](../../specs/2026-10-20-nano-v1/S10-monitor.md)。

## 结论

- 桥接与 Rust 模型完成：有界、非消费的任务输出监听；行合并、字面/正则过滤、`notify`/`quiet` 投递、预算/有效期、停止监听、缺口/裁剪可见、终态只进 UI。
- 真实模型会话验证了 `quiet` 与 `notify` 两种投递，以及 durable `user/message` 收件事实。
- 真实 tmux TUI 验证了附着前「已有 N 字节未读」、从开头重放、过滤、预算、`role=user / source=monitor`、`stream-ended`、停止监听不影响任务。
- 未验证：长时间空闲（数分钟以上）的 `quiet` 注入；本份只验证了空闲数秒后由普通用户回合唤醒。没有跑网络监听、文件 tail 或 MCP monitor（规格明确不在首版范围）。

## 真实模型证据

文件：[model-evidence.txt](model-evidence.txt)

- `CAPABILITY`：`monitor` 为 `supported`。
- quiet：后台任务输出 3 条匹配行 + 1 条噪声行；监听从 `new` 开始，过滤器 `monitor-line`。3 条通知先为 `inserted`（未唤醒），噪声行 `noiseDelivered:false`。
- 普通用户回合后，3 条都变为 `accepted`，并且 `QUIET_DURABLE_USER_MESSAGE.present=true`，3 个 monitor 消息 id 都出现在 durable `user/message` 中。
- notify：第二个后台任务输出 `notify-line-42`；监听以 `notify` 模式创建，回执为 `accepted`（durable user/message），没有用 `inserted`/`claimed` 冒充收件。
- 第一个任务 `settled` 为 `completed`，监听 `stream-ended`，`streamDetail: exit code: 0`；S10 没有额外发终态 model 通知。
- 停止监听后 `jobs.snapshot` 仍包含两个任务：停止监听没有停止任务。
- 费用记录：本次会话 `tokenUsage = {uncachedInputTokens: 10229, outputTokens: 295, cacheReadTokens: 88192}`，`projectedTokens: 20078 / 1000000`（`nano.sessions.context` 在关机前读取）。

## 真实 TUI 证据

文件：[pty-evidence.txt](pty-evidence.txt)

- 模型在同一会话里先通过官方 `job_output` 读完了 4 行输出（`monitor-line-1`、`noise-ignore-me`、`monitor-line-2`、`monitor-line-3`），随后监听仍能从 `earliest` 重放同一批 retained 字节；这证明监听使用独立偏移，不依赖也不改写模型的消费游标。
- 附着表单显示 `已有 61 字节未读`，默认「只看新输出」；选择「从开头」后重放。
- 过滤器 `monitor-line` 排除了 `noise-ignore-me`。
- 3 条匹配行在 800 ms 合并窗口内合并成一条通知（`通知 1/50 · 行 3`），通知只带首行、末行和 `omitted=1`。
- 监听面板显示 `role=user / source=monitor（user-role 消息，不是 Agent 授权）`。
- 空闲时 3 条为 `inserted`；普通用户回合后变为 `已收件（durable user/message）`。
- 终态显示 `stream-ended（流结束；不代表条件满足）`，原因是 `exit code: 0`。
- 按 `s` 后显示「用户已停止监听；被观察任务未受影响」，任务列表仍有该任务。

## 自动测试

- [cargo-test.txt](cargo-test.txt)：`cargo test --workspace` 全绿。
- [bridge-test.txt](bridge-test.txt)：`npm run test:bridge`，234 passed / 0 failed / 5 skipped。
- [host-e2e.txt](host-e2e.txt)：`NANO_E2E=1 node --test bridge/test/integration/host.test.mjs`，3/3 通过；包含 `nano.monitor.list` 空列表与不存在任务返回 `1028` 的断言。
- [versions.txt](versions.txt)：本机工具版本。

## 实施期确定的值（规格 §9 留给实施）

- 过滤器语法：字面 substring；`regex` 为 `true` 时按 JavaScript RegExp 编译。pattern 上限 512 UTF-8 字节。
- 默认合并窗口 800 ms；最小投递间隔 1000 ms；单条最多 20 行；单条最多 8192 UTF-8 字节 / 2048 估算 token（`ceil(bytes/4)`）。
- 默认最多 50 条通知；有效期 30 分钟；单个宿主最多 32 个监听；单条长行超过 16384 字节时按块处理并标记 `long-line block`。
- 预算耗尽/过期后仍继续读取并保留一个有界「暂存」摘要（首/末行 + 计数），UI 显式显示；只有用户通过 `nano.monitor.resetBudget` 才能重置。
- 同一任务的活动监听重复创建返回现有记录；已停止/已结束的记录在再次 `create` 时被新的监听代际替换。停止监听不会删除记录。
- 终端态映射：`completed → stream-ended`、`killed → stopped`、`failed → failed`；任务记录被移除映射为 `stream-ended` + `任务记录已从官方列表移除`。
- UI 位置：Monitor 暂作为 S08 任务面板的子视图（选中任务按 `m`），不新增全局键位或主题；面板内 `n/a/t/f/r/s/b` 为临时操作键，待 S13 design-contract 冻结时统一。

## 复跑

```sh
cargo build -p nano-app
NANO_EVIDENCE_MODEL=deepseek-flash node scripts/model-evidence-s10.mjs
NANO_EVIDENCE_MODEL=deepseek-flash scripts/pty-evidence-s10.sh
```

两个脚本都安装临时 profile；PTY 脚本使用临时 `DSH_HOME` 和用户凭据副本，不修改 `~/.dsh` 的现有 profile。
