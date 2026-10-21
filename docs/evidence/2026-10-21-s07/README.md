# S07 会话与上下文：实施证据

- 执行单：#10
- 规格：`docs/specs/2026-10-20-nano-v1/S07-sessions-context.md`
- 环境：macOS + Ghostty 目标矩阵；本机 `dsh 0.2.1-rc.2`、Node v24、Rust stable
- 日期：2026-10-21

## 1. 跑了什么

| 命令 | 结果 | 记录 |
| --- | --- | --- |
| `node --test bridge/test/*.test.mjs` | 120 项通过（含 11 项 S07 单元测试；S04/S05 套件同跑） | `unit-and-contract.txt` |
| `cargo test --workspace` | 242 项通过（75 app + 27 controls + 2 controls contract + 57 conversation + 5 conversation contract + 21 sessions + 20 skills + 5 skills contract + 18 runtime + 10 runtime contract + 2 preflight；15 项真实宿主测试默认 ignored） | `unit-and-contract.txt` |
| `NANO_E2E=1 node --test bridge/test/integration/sessions.test.mjs` | 通过 | `real-host-sessions.txt` |
| `NANO_E2E=1 node --test bridge/test/integration/*.test.mjs` | 4 项通过（host/conversation/sessions/skills 同跑） | 本轮终端输出 |
| `cargo test -p nano-runtime --test integration -- --ignored --test-threads=1 --skip shutdown_budget_is_bounded_when_cleanup_hangs` | 14 项通过（S01 真实宿主回归，含 writer-held 映射） | 本轮终端输出 |
| `python3 scripts/sessions-evidence.py`（真实 PTY） | 20 项检查全部通过 | `pty-sessions.txt` |

## 2. 真实宿主证据（Node，临时 DSH_HOME/数据根）

测试 `official sessions list, rename, context, compact, occupancy, and resume` 用真实 `dsh --profile dsh-tui-nano` 验证：

- `nano.sessions.list` 返回当前会话行：`current=true`、`agentAvailable=true`、`blank=true`、`formatStatus=current`、`cwd` 为真实工作目录。
- `nano.sessions.rename` 接受并归一化标题；再次 list 读到新标题；重开第二个 generation 后标题仍在（官方日志持久化）。
- 冷会话改名返回 `1028 nano/sessions/not-active`，没有 resume、没有拿写锁；恢复后的宿主对共享 `sessions/` 里真实存在的另一个冷会话（`otherId`）再次验证了这一点。
- `nano.sessions.context` 返回 `approximate=true` 与官方计费用量；没有 provider 请求时 `projectedTokens`/`contextWindow` 缺省，界面显示「未知」而不是 0。官方投影缺失 `tokenUsage` 时字段整体缺省，Rust 侧显示「未知（官方投影未提供）」，不补零。
- 合并 S04 后跑其真实 PTY 证据（一次极小的真实请求）：S07 的标题栏显示 `ctx 19.2k/1000k (1%) · 参考`，数值来自同一 `contextPressure` 投影；屏幕原文在 S04 的 `pty-controls-evidence.py` 输出中。
- `nano.sessions.compact` 走官方 `/compact`：空会话返回 `No compactable history yet.`；带参数返回官方 `Usage: /compact (no arguments)`。
- 同一根会话的第二个宿主在认领处被拒绝：`nano.startupError { code: "occupied", holderPid: <第一个宿主 pid> }`；退出后 `sessions/` 的日志数不变，证明它没有创建会话。
- 恢复不存在的会话返回 `startup-failed`，消息含 `session/not-found`，没有创建同 ID 空会话。
- 在共享 `sessions/` 里物化第二个会话并改名后，从当前根会话 list：冷会话行存在，但标题为 `null`、`updatedAt` 缺省——本窗口的 projection cache 按根会话分区，S07 不为补字段去打开冷会话正文。列表顺序保持官方 `sessionQuery` 的最新优先，不按合成 `updatedAt` 重排。
- 跨目录会话在列表里标「目录 X（不可切换）」，选中时在拆旧宿主之前就拒绝，不会先杀掉当前会话再必然启动失败。
- 压缩 summary 不改人类 transcript：模型面的 `surfaceOp: replace` 只影响模型视图；append-origin 的旧消息保留，checkpoint 不作为聊天消息显示，`compaction/summary`/`compaction/prune` 生成「已压缩/裁剪 N 条较老历史（约 M tokens）」边界提示。这与官方 chat 端的 `isAppendSurfaceEvent && !isCompactionCheckpoint` 过滤一致。

## 3. 真实 PTY 证据（`nano` 二进制，120×40）

`scripts/sessions-evidence.py` 在真实 PTY 中驱动按键并读取挂载账本：

- `Alt+O` 打开官方会话面板；面板显示「新建会话」、当前会话行、上下文「未知」、计费用量、当前标记。
- `↓` 选中当前会话，`n` 进入改名，输入「证据会话」后 Enter，官方接受并显示「会话已重命名」。
- `Alt+C` 返回官方 `No compactable history yet.`。
- 切换前在输入框留下草稿「草稿A」；`Alt+O` 后 Enter 在「新建会话」上执行受控重启：旧宿主退出并释放认领，新宿主认领新的 `rootSessionId`；账本中旧/新会话 ID 不重叠，新会话不显示旧草稿。
- 重启后重新打开面板：新会话显示「未命名」「当前」；再选中原会话并 Enter 返回，账本恢复原会话认领，输入框恢复「草稿A」。
- `Ctrl+C` 退出码 0，账本 `owners` 为空。

## 4. 未测与已知边界

- **真实模型下的占用数值**：S07 自己的脚本不发起模型请求；合并后借用 S04 的既有真实 PTY 取证（用户此前授权的一次极小请求），观察到 `ctx 19.2k/1000k (1%)`。连续两次请求后的「最近记录」时刻与实际切换模型后的短暂错配仍只用单元测试覆盖。
- **真实历史的手动压缩成功**：`Compacted N history items (~M tokens).` 只用单元测试覆盖；真实 PTY 只验证了官方「无可压缩历史」路径。
- **压缩取消的真实时长路径**：官方 registry 的 `withAbort` 会在 abort 时先 reject（handler 的 `Compaction cancelled.` 被丢弃）；桥接现在捕获 abort 并返回官方文本，app 在取消时立即停止等待、显示 `Compaction cancelled.` 并重读官方状态。单元测试按官方 reject 行为模拟；真实压缩进行中的取消仍未测（需要模型额度与较长历史）。
- **手动压缩入口归属**：S04 的命令路径尚未实施；S07 直接消费已组合的官方 `ctx.commands` 注册表（`Alt+C` / 面板 `c`）。S04 落地后 `/compact` 文本入口应路由到同一 bridge 方法，届时需要一次归属对齐。`/compact` 的传参用法错误目前只能从协议层触发，TUI 没有暴露参数输入。
- **自动压缩边界**：由官方决定，S07 只展示；本轮未触发。
- **切换失败后的真实终端「无宿主」屏**：`StartupFailed { occupied, holderPid }` 到「被 pid X 占用 / Alt+R 重试 / Alt+B 返回」的映射有单元测试，认领拒绝有真实宿主证据；但没有在真实 PTY 里制造第二个占用窗口来截屏这一屏。
- **S03 阅读锚点**：S03 尚未实施，S07 不持有锚点；切换会重置显示滚动位置，草稿按地址保留。这是规格 §3.1/§3.4「保存/保留阅读位置」的已知前置依赖，PR 正文已报告。
- **跨目录会话的真实终端证据**：列表标记与「选中前拒绝」有单元测试，真实宿主验证了 `cwd` 字段与 resume 的目录检查；未在真实 PTY 里造两个目录做截屏。
- **冷会话的标题/最近活动**：S01 把 projection cache 按根会话分区，别的根会话的缓存不在本窗口；S07 不打开冷会话正文，所以列表对无缓存提示的冷会话显示「未命名」「最近活动未知」。恢复/打开后标题与最近活动来自官方实时投影。
- **规格与 profile 组合的偏差**：S07 规格写 `ctx.sessionController.resolveAgent(sessionId)`，但 Nano 最小 profile 没有组合 `@deepseek-ai/dsh-api-session-controller`。实现改用同一官方数据链：`ctx.sessionPersistence.stat`（存在性/格式）、`ctx.agents.resume`（拿受拥有的 AgentHandle，保住四事实关停）、`ctx.sessionQuery`（列表）、`ctx.sessionProjections`（占用/标题）、`ctx.sessionTitle.rename`（改名）、`ctx.commands.execute('/compact')`（压缩）。`SessionAlreadyOwnedError` 映射为 `occupied` + `session/writer-held` 文本。
- **关闭窗口的草稿**：不跨进程恢复，符合 S07 §6.7。

## 5. 复现

```sh
cargo build -p nano-app
node --test bridge/test/*.test.mjs
cargo test --workspace
NANO_E2E=1 node --test bridge/test/integration/sessions.test.mjs
python3 scripts/sessions-evidence.py
```
