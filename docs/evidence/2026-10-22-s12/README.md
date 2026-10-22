# S12 子代理实施证据（2026-10-22）

对应规格：`docs/specs/2026-10-20-nano-v1/S12-subagents.md`；执行单 #15。
环境与冻结版本见 `versions.txt`：官方 dsh 0.2.1-rc.2、Node v24.18.0、Rust 1.99.1。证据在合并 S10 Monitor 的 `origin/main` 之后重跑。

## 真实模型证据

命令：`NANO_EVIDENCE_MODEL=deepseek-flash node scripts/model-evidence-s12.mjs`。
原始输出：`real-model-subagents.txt`。

| 输出 | 证明 |
| --- | --- |
| `CHILD_STARTED` | 真实 `spawn` provider 创建本地子代理（`local:true`）；官方生命周期 `subagent/start` 到达 |
| `SNAPSHOT` / `DESCRIBED` | `nano.subagents.snapshot` 返回直接子代理，mode 为 `continuable`、activity 为 `running`；`describe` 从子会话自己的投影确认 mode，并返回 `seq`、`parentLive:true`、`settledMs`、`activeSince` |
| `CHILD_SUBSCRIBE` | 子地址 `nano.subscribe` 成功，回放从 0 开始，返回子会话 header/seq |
| `CHILD_END` | 官方 `subagent/end` 的 `stopReason:completed` 原样到达 |
| `CHILD_EVENTS` | 25 条子会话持久事件（descriptor、`user/message`、`assistant/message`、`tool/call`、`tool/result`、`turn/end`）按子地址回放，含子代理正文 `S12CHILD`；`sawParentSeed:false` 证明 spawn 子代理没有父回合 seed |
| `FORK_STARTED` | 真实 `fork` provider 创建本地子代理（`provider:"fork"`） |
| `FORK_DESCRIBED` / `FORK_SUBSCRIBE` | fork 子代理同样可续聊，`seq` 与父回合历史一致；子地址订阅成功 |
| `FORK_END` | fork 的官方终态 `completed` |
| `FORK_EVENTS` | 78 条事件，含 `session/end-seed` 与 `S12ROOTPROMPT`（父已完成回合的 seed）；`sawForkText:true` 证明 fork 本地址的正文与 seed 同属该子地址 |
| `FOLLOWUP` / `FOLLOWUP_END` | 人类续聊走官方 `subagents/prompt`，`accepted:true` 并返回 `messageId`；子代理再次以 `completed` 结束 |
| `INTERRUPT` | Stop 走官方 `interruptByParent`，返回 `accepted:true`；未使用 `dispose`。该次调用发生在 FOLLOWUP_END 之后，证明的是官方路径与接收语义，不是运行中回合的排队保留 |
| `FINAL_SNAPSHOT` / `LIST` | 两个直接子代理按父目录顺序返回；fork 行带 `lastTurnCompleted:true` 与 `settledMs`；递归列表带 `parentId`/`depth` |
| `SHUTDOWN` / `EXIT` | 六步关停 clean，退出码 0 |

## 真实 TUI 证据

命令：`python3 scripts/pty-evidence-s12.py`（120x40 PTY，真实 `nano` 二进制、真实官方宿主、真实 deepseek-flash）。
原始输出：`real-tui-subagents.txt`。

| 截屏 | 证明 |
| --- | --- |
| `SCREEN dock` | 真实 spawn 子代理出现在底部；pill 为 `1 local agents`；行显示 `s12-tui · 可续聊 · 运行中`；状态灯为 0.8s 闪烁的暗相 `○` |
| `SCREEN child` | 顶部为 `nano 子代理 · s12-tui · 可输入` 和 `[← 返回]`；只显示子地址自己的消息（`S12CHILD`、子代理的 system/context 事件），没有父对话或兄弟内容；输入框标题为 `子代理 · Enter 发送 · Shift+Enter 换行 · Alt+K 停止本轮 · Esc 返回`；底部状态栏显示 `子代理（审批由官方拒绝）`，不冒充根会话的 `workspace-write` 权限 |
| `SCREEN followup` | 在子代理输入框发送的 `S12TUI` 续聊出现在同一子视图，证明人类续聊走 `subagents/prompt` |
| `SCREEN return` | Esc 返回父对话；可续聊子代理的 `subagent/end:completed` 之后 dock 仍保留 `s12-tui` 入口（`未运行 · 2.3s`），没有被 1.5s 清理 |

## 自动化证据

| 文件 | 命令 | 结果 |
| --- | --- | --- |
| `cargo-test.txt` | `cargo test --workspace` | 全部通过（含 S01-S11/S10 回归、`nano-subagents` 契约测试、S12 app 交互测试） |
| `bridge-test.txt` | `npm run test:bridge` | 272 项：267 通过、5 跳过（S10/S13 预留）、0 失败；含子代理服务、one-shot descriptor 夹具、子地址订阅回放、父/子事件隔离、可达性与容量映射 |

S12 单元/契约覆盖：

1. 直接子代理最多 4 个 + 溢出入口；`N local agents` 不计 external。
2. 状态灯：running 0.8s 亮/暗（模运算相位，单测覆盖两个半周期）、completed 绿、aborted/refusal/max-tokens 黄、error 红、unknown/inactive muted；`lastTurnCompleted` 只作为文字，不驱动终态颜色。
3. one-shot/external 的 `subagent/end` 约 1.5 秒后只隐藏底部入口；可续聊子代理的普通回合完成保留 dock 入口；记录仍留在 catalog/全部列表。
4. 四种输入边界：parent live 可输入、parent 不可用且 child 空闲只读、parent 不可用且 child 运行仅 Stop、one-shot 只读、unknown 先只读、external 不可进入。
5. 草稿按地址隔离：describe 返回前 gate 为 Resolving，父草稿不会出现在子视图或被子地址发出；describe 后切换子草稿，退出再进入恢复；失败恢复写回发起消息的子地址。
6. 子地址订阅只回放该子会话；fork seed 作为该子地址历史保留；兄弟/根事件不进入子订阅。
7. 生命周期未知 stopReason 归一为 `unknown`，不猜成功。
8. 子地址 live 帧按地址过滤：child-a 订阅收不到根或 child-b 的 `conversation.stream`。
9. `describe`/`prompt`/`interrupt` 的 parent 必须是本宿主 root 或官方目录可达的后代；外部父级被拒。
10. 容量拒绝（官方 `subagent/delivery-unavailable` + cause `ACTIVATION_LIMIT_REACHED`）映射为 `nano/subagents/capacity`；客户端取消映射为 `nano/cancelled`。
11. 子视图安装 Interactive transcript（continuable）或 ReadOnly（one-shot），退出再进入复用缓存视图与阅读位置。

## 实现期自定默认值（待 S13 design-contract 冻结）

- dock 聚焦条件：输入草稿为空且无更高优先级覆盖层；`↑↓` 选择，`Enter/→/Space/f` 进入，`k` 停止选中，`l` 打开全部列表，`Esc` 回输入。
- 空 dock 时 `Alt+L` 仍可打开全部列表；全部列表 `↑↓` 选择、`Enter` 进入、`r` 刷新、`PageUp/PageDown` 翻页、`Esc` 关闭。
- 子视图：`Esc` 返回，空输入 `←` 返回，顶部 `[← 返回]` 点击返回；`Enter` 发送、`Shift+Enter` 换行、`Alt+Enter` 插话、`Alt+K` 停止本轮。
- 状态灯周期 800ms（亮/暗各 400ms）；reduced-motion 通过 `NANO_REDUCED_MOTION=1` 或 `true` 静止。
- 子视图缓存最多 16 个地址；退出时超出清空。时长格式为 `<1s` 用 ms、`<60s` 用一位小数秒、更长为 `XmYYs`。
- 子代理 label/诊断/错误文本渲染前把控制字符替换为 `?`，防止转义注入。

## 未测 / 未保证

- **外部 provider（ACP / SDK / Codex / Claude Code）**：当前 profile 组合未启动外部 provider；external 叶子只由桥接单测的目录夹具覆盖，未做真实 provider 证据。
- **one-shot 历史**：`bridge/test/fixtures/subagent-one-shot.json` 提供 `subagent/descriptor` + 历史事件夹具，bridge 测试验证 describe→回放；app 单测验证只读 gate。仍没有真实 one-shot provider 运行。
- **容量上限（默认 8）**：容量 cause 的映射由桥接单测覆盖；本轮没有创建 8 个以上子代理。
- **目录不可读 / 子代理已销毁 / 冷启动失败**：诊断与错误映射是单测覆盖；未构造真实损坏目录或已销毁子代理。
- **图片续聊**：子代理图片走官方 `subagents/prompt` 附件准入；本轮只发送文本，未真实上传图片。
- **鼠标点击 dock / 顶部返回按钮**：代码路径存在（`subagent_dock_hits`、`subagent_exit_area`），本轮真实 TUI 证据用键盘 Down/Enter/Esc；鼠标路径未在真实终端点击验证。
- **TUI 键位证据**：`f` 与 Enter 进入同一完整视图；dock 的 `k` 与子视图 `Alt+K` 的停止路径由代码/桥接单测覆盖，TUI 截屏未单独按键验证；`l`/`Alt+L` 全部列表入口未在真实终端按键验证。
- **reduced motion 的真实设置**：S13 design-contract 尚未冻结；本轮按 `NANO_REDUCED_MOTION=1` 环境开关实现；`blink_on` 相位有单测，但 reduced-motion 的终端绘制未做真实截屏。
- **4 个以上直接子代理的折叠**：dock 截断/溢出由单测覆盖；真实模型本轮只创建 1 个 spawn，模型证据脚本创建了 2 个（spawn+fork），未真实创建 5 个。
- **嵌套多层子代理**：桥接 `listDescendants` 与 all-list 缩进由单测覆盖；真实模型未创建 depth ≥ 2 的树。
- **跨根会话子代理隔离**：S12 的 describe/prompt/interrupt/订阅现在都校验 parent 可达本宿主 root；跨根失败由桥接单测覆盖，没有双宿主真实进程证据。
- **activity 来源**：snapshot/describe 的 activity 来自官方 live Agent 注册表（与官方 `list_agents` 的 children 作用域一致），list 的 activity 来自控制面 `listDescendants`；两处对 resident-idle 的观测可能短暂不一致，这是官方两个投影的差异，未在 S12 另造判定。
- **子视图的根 `!` 记录**：已在子视图隐藏，避免父对话的本地 bash 回显串入；`!!` 记录本身的退休策略沿用 S08，不在 S12 改。
