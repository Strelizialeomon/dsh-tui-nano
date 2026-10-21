# Claude Code 2.1.88 源码参考：滚动 / Monitor / Shell / Subagent / Skills

来源：本机 `/Users/sunchongsheng/Downloads/ClaudeCode/src`（Claude Code 2.1.88 解包源码）。
用途：只作为实现参考，不改变官方 DSH 契约；采纳项要写回对应 spec，未采纳项说明原因。

## 1. 滚动（S03 阅读 / S09 终端）

### 观察到的机制

- **命令式滚动**：`ScrollBox` 的 `scrollTo`/`scrollBy` 直接改 DOM 节点 `scrollTop`，`markDirty` + `scheduleRender`，不经过 React state；多次滚轮事件用 `queueMicrotask` 合并成一次渲染（`src/ink/components/ScrollBox.tsx:1-140`）。
- **sticky bottom**：`stickyScroll` 默认吸底；显式 `scrollTo`/`scrollBy` 会清掉吸底；渲染器在内容增长且原本在底部时重新吸底。
- **scrollToElement**：跳转到元素时不立刻算位置，而是把元素引用交给渲染阶段，在同一 Yoga pass 里读 `getComputedTop()`，避免 throttle 导致的位置过期（`ScrollBox.tsx:20-35`）。
- **虚拟化**：只挂载视口 + overscan 的行；未测量项按 3 行估高，覆盖计算按 1 行悲观估高；最多挂 300 项；每次提交最多新增 25 项；滚动位置按 40 行量化；渲染时把 `scrollTop` clamp 到已挂载范围，避免追帧时出现空白（`src/hooks/useVirtualScroll.ts:1-120`）。
- **滚轮曲线**：原生终端 1 事件=1 行；Ghostty 会在 SGR 层预乘 3；xterm.js 1 事件/刻度；对廉价编码器的反向抖动做 bounce 检测；xterm.js 路径用指数衰减 + 小数余量进位；常量和曲线按真实设备实测（`src/components/ScrollKeybindingHandler.tsx:1-120`）。
- **滚动抑制后台轮询**：`markScrollActivity()` 让 IDE/LSP/GC 等后台定时器跳过下一 tick，减少滚动掉帧（`ScrollBox.tsx:100-115`）。

### 建议采纳

1. S03/S09 写清「命令式滚动 + 微任务合并」，不要让每个滚轮事件触发一次完整重排。
2. S03 写清 sticky bottom 的语义：默认吸底，用户上滚即脱离，回到底部再吸底。
3. S03 写清 `scrollToElement` 的渲染期定位，避免「跳转到消息」用过期位置。
4. S03 写清虚拟化初始预算：overscan 80 行、未测量估高 3 行、悲观 1 行、挂载上限 300、单次新增 25、量化 40 行；这些是 Claude Code 实测值，实施时在 Ghostty 复测后冻结。
5. S03/S09 写清滚轮加速要按设备区分，禁止全局一个倍率；首版至少处理 Ghostty 与 xterm.js 两类。
6. S03 写清滚动期间抑制后台轮询，避免滚动和 GC/IDE 轮询抢事件循环。

### 不采纳

- 不照搬 Claude Code 的具体加速度常量（40ms 窗口、0.3 步进、6 倍上限等）；它们是 macOS/iTerm2/Ghostty 实测值，必须在本项目目标终端复测。
- 不引入 React/Ink；只采纳交互和预算语义。

## 2. Monitor（S10）

### 观察到的机制

- Monitor 不是独立进程执行器，而是 **`local_bash` 任务的一个 kind**：`kind: 'monitor'`。它和后台 shell 共用任务表、输出文件、停止入口（`src/Task.ts:66`、`src/tasks/LocalShellTask/guards.ts:9`）。
- **流式语义**：Monitor 是 streaming-only；脚本退出表示「流结束」，不是「条件满足」。完成通知文案是 `Monitor "<desc>" stream ended`，与后台命令完成的 `Background command ... completed` 区分（`src/tasks/LocalShellTask/LocalShellTask.tsx:129-145`）。
- **每行一条通知**：Bash 提示词明确写「each stdout line is a notification」（`src/tools/BashTool/prompt.ts:312-320`）。
- **通知走任务队列**：`enqueuePendingNotification` 把 `<task-notification>` 放进命令队列；普通用户输入默认 `later`，Monitor/后台任务通知默认 `next`；同优先级 FIFO（`src/utils/messageQueueManager.ts:126-142`）。
- **去重**：任务状态有 `notified` 标志；`TaskStop` 会先标记 `notified`，避免停止时再发一条多余通知（`LocalAgentTask.tsx:224-260`、`LocalShellTask.tsx:110-125`）。
- **状态栏分型**：底部 pill 把同一任务类型拆成 `N shells, M monitors`，不把 monitor 混进 shell 数量（`src/tasks/pillLabel.ts:17-30`）。
- **反 sleep 轮询**：`sleep N`（N≥2）作为首命令会被拒绝，提示改用 `run_in_background` 或 Monitor；一次性的等待用后台命令通知，不要 sleep 轮询（`BashTool.tsx:318-340,525-535`、`BashTool/prompt.ts:312-320`）。
- **MCP 变体**：另有 `monitor_mcp` 任务类型，v1 不需要（`Task.ts:12`）。

### 建议采纳

1. S10 把 Monitor 简化为「后台任务的一种流式视图」：S08 的任务是唯一执行者，S10 只订阅输出、把匹配行变成通知；不要另起进程执行器。
2. S10 写清「流结束 ≠ 条件满足」：脚本退出/任务终态只产生 `stream ended`，不宣称监控目标达成。
3. S10 写清通知队列：`task-notification` 优先级高于普通输入，同优先级 FIFO；合并只发生在队列层，不在 S10 自造第二套队列。
4. S10 写清去重：任务有 `notified` 标志；停止/终态只发一次；重复通知要能被识别。
5. S10 写清底部状态分型：`N shells, M monitors`，不要把 Monitor 混进 shell 计数。
6. S08/S10 写清反 sleep 轮询：Agent 侧长等待用后台任务 + 通知；`sleep N≥2` 引导到后台/Monitor，不进入 sleep 轮询。

### 不采纳

- 不照搬 `MonitorMcpTask`（MCP 通知）到首版；S10 只订阅任务输出。
- 不把 Monitor 做成独立调度器或独立进程执行器；它必须复用 S08 的任务和输出。

## 3. Shell（S08）

### 观察到的机制

- **单一输出源**：`TaskOutput` 是命令输出的唯一真源。bash 命令的 stdout/stderr 直接写文件 fd，不过 JS；进度靠轮询文件尾部（1s、4KB），内存上限 8MB，超限溢写磁盘（`src/utils/task/TaskOutput.ts:1-140`）。
- **任务状态**：`pending/running/completed/failed/killed`；任务 ID 带类型前缀；任务有 `outputFile`、`outputOffset`、`notified`（`src/Task.ts:20-60`）。
- **自动转后台**：前台命令超时后自动转后台任务，并返回任务 ID；后台任务没有执行超时（`ShellCommand.ts:1-160`）。
- **输出上限 watchdog**：后台任务直接写文件，每 5s 检查文件大小，超限就杀，避免磁盘被写满（`ShellCommand.ts:50-70`）。
- **卡住检测**：后台命令输出停止增长超过 45s，且尾部看起来像交互提示（`(y/n)`、`Press Enter`、`Continue?` 等），发一条「可能等待交互输入」通知，建议改用管道/非交互参数（`LocalShellTask.tsx:26-100`）。
- **停止**：`tree-kill` 终止进程树；`TaskStop` 先标记 `notified` 再杀，避免重复通知。
- **用户 `!` 命令**：Claude Code 的 `!` 直接调用 BashTool，`dangerouslyDisableSandbox: true`，结果包成 `<bash-input>/<bash-stdout>/<bash-stderr>` 合成消息，`shouldQuery: false`（不立即触发模型）；它走的是用户权限，不是 Agent 沙箱（`src/utils/processUserInput/processBashCommand.tsx:60-130`）。

### 建议采纳

1. S08 写清 `TaskOutput` 单一输出源：文件 fd + 尾部轮询 + 8MB 内存上限 + 溢写磁盘 + 大小 watchdog；Rust 侧只读文件/偏移，不维护第二份输出。
2. S08 写清任务状态和 `notified` 去重；任务终态只发一次通知。
3. S08 写清自动转后台：前台超时 → 后台任务 + 任务 ID；后台无执行超时。
4. S08 写清卡住检测：输出长时间不增长 + 尾部像交互提示 → 通知「可能等待输入」，建议管道/非交互。
5. S08 写清 `!` 不能注册成官方 job：官方 job 表会被 `job_list` 看见，人工命令必须走独立的本地执行路径。Claude Code 的 `!` 也证明它可以绕过 job 表。
6. S08 需要你拍板 `!` 的权限：Claude Code 让 `!` 走用户权限、关闭沙箱；我们当前 spec 写的是会话沙箱。二选一，见正文「待拍板」。

### 不采纳

- 不照搬 `sleep N` 的硬拒绝到所有场景；它适合 Agent 侧，不适合用户 `!` 命令。
- 不照搬 8MB/1s/4KB 作为最终值；作为初始预算，实测后冻结。

## 4. Subagent（S12）

### 观察到的机制

- `AgentTool` 支持 `run_in_background`，异步返回 `{ status: 'async_launched', agentId, description, prompt, outputFile, canReadOutputFile }`；2s 后显示后台提示，120s 自动转后台（`src/tools/AgentTool/AgentTool.tsx:60-120`）。
- `LocalAgentTask` 把子代理作为任务：`local_agent` 类型、`agentId`、`outputFile`（符号链接到子代理 transcript）、`notified` 去重、completed/failed/killed 通知（`src/tasks/LocalAgentTask/LocalAgentTask.tsx:224-260,467-560`）。
- `SendMessageTool` 支持按名字/广播/`uds:`/`bridge:` 地址发消息；人类 UI 走 `subagents/prompt` 的 queue/steer，不走 agent-to-agent 的 `sendMessage`（我们 spec 已按这个修正）。
- 后台任务导航：`useBackgroundTaskNavigation` 用 Shift+↑/↓ 在 leader/teammates 间切换，Enter 确认，`f` 看 transcript，`k` 杀；`AgentNavigationFooter` 显示「↑↓ navigate · Enter select · Esc back」（`src/hooks/useBackgroundTaskNavigation.ts:1-120`、`src/components/agents/AgentNavigationFooter.tsx:1-40`）。
- 任务 pill：`1 local agent` / `N local agents`；后台 shell 和 agent 在同一任务列表（`src/tasks/pillLabel.ts:50-60`）。

### 建议采纳

1. 只采纳导航、pill 分型和发送路径；**不采纳** `agentId`/`outputFile`/`notified`/`local_agent` 任务模型——DSH 没有这些字段，子代理正文就是 child Session。
2. **不采纳** 2s/120s 自动转后台——DSH 子代理一开始就异步，没有 foreground→background 转换。
3. S12 写清导航：↑↓ 选择、Enter 进入、Esc 返回、`f` 看 transcript、`k` 停止；与现有从属导航一致。
4. S12 写清发送路径：人类续聊走 `subagents/prompt` 的 queue/steer；`sendMessage` 只用于 agent-to-agent，UI 不调用。
5. S12 写清 pill 分型：`N local agents`，不混进 shell/monitor 计数。

### 不采纳

- 不照搬 `isolation: worktree/remote` 和 Agent Teams 到首版；README 明确不做自动 worktree 编排。
- 不照搬 120s 自动转后台的最终值；作为初始值，实测后冻结。

## 5. Skills（S05）

### 观察到的机制

- `skillChangeDetector` 用 chokidar 监听 skill/command 目录：1s 稳定性、500ms 轮询、300ms 防抖；Bun 下改用 stat 轮询（2s），规避 Bun fs.watch 死锁；变化时清缓存并通知（`src/utils/skills/skillChangeDetector.ts:1-120`）。
- `useSkillsChange` 订阅 detector；变化时清命令缓存、重新扫描；GrowthBook 刷新时只清 memo（`src/hooks/useSkillsChange.ts:1-60`）。
- `SkillTool` 输入 `{ skill, args? }`；输出分 inline（commandName/allowedTools/model）和 forked（agentId/result）两种；一次只跑一个 skill/command（`src/tools/SkillTool/SkillTool.ts:291-350`）。
- 技能可以带 `allowedTools`、`model` override，也可以 fork 成子代理执行。

### 建议采纳

1. S05 写清技能目录的文件监听：新增/改名/删除/正文编辑要触发下一次目录刷新；用防抖合并，不能只靠 preset/plugin/connection 事件。
2. S05 写清 `skills/change` 订阅 + 本地 watcher 双通道；清除缓存后重新拉官方目录。
3. S05 写清技能调用结果分 inline/forked；首版可以只做 inline，forked 作为后续。
4. S05 写清 `allowedTools`/`model` override 的展示来源；没有官方字段就不显示。

### 不采纳

- 不在 Nano 里自建 skill 解析器或第二份目录；仍以官方 skill registry 为准，只补 watcher/refresh。
- 不照搬 GrowthBook；DSH 没有对应机制。

## 6. 对 spec 的落点

| spec | 建议改动 |
| --- | --- |
| S03 | 滚动交互、sticky、scrollToElement、虚拟化预算、滚轮分设备、滚动抑制后台轮询 |
| S05 | 技能目录 watcher + `skills/change` + 防抖刷新；inline/forked 结果 |
| S08 | TaskOutput 单一输出源、状态/notified、自动转后台、卡住检测、`!` 独立本地路径 |
| S09 | 终端滚动的命令式路径、sticky、scrollToElement、分设备滚轮；滚动抑制后台轮询 |
| S10 | Monitor 作为 S08 流式任务、每行通知、任务队列优先级、去重、pill 分型、反 sleep 轮询 |
| S12 | 导航、pill、发送路径；不采纳 task model/2s/120s（DSH 没有） |

## 7. 待拍板

`!` 命令的权限模型：

- **A. 用户权限（Claude Code 同款）**：`!`/`!!` 走用户 shell，不受 Agent 沙箱约束；`!` 只本地显示、不进 transcript/模型；`!!` 才把命令与输出交给模型。安全边界与 S09 用户终端一致。
- **B. 会话沙箱**：`!` 仍受当前会话 sandbox 约束；但官方 `ctx.approval` 只在 open turn 可用，空闲时无法审批，需要 fail-closed 并明确「被沙箱拒绝」，且 `!` 不能注册成 job。
- 推荐 A：与 Claude Code 一致，避免「人工命令审批」死路；代价是 `!` 与用户终端同级，需要明确提示。

## 8. 拍板结果与落地（2026-10-21）

用户选择：`!` 按 Claude Code 的实现。

落地口径：

- `!command` 以用户权限执行（不套 Agent 沙箱，等价 `dangerouslyDisableSandbox`）。
- 输出以 `<bash-input>/<bash-stdout>/<bash-stderr>` 合成 user 上下文注入（官方 `inject` 语义），不立即唤醒模型；下一条消息能看到。
- 来源标记 `bash-mode`，不是用户批准，也不是 Agent 授权。
- `!` 不注册成官方 job，不进入 `job_list`。
- `!!` 不是本项目的特殊模式，只作为 bash 历史展开。
- README §2、S08、S02、S03、S13 已同步。

其余采纳：

- S03：命令式滚动、sticky bottom、`scrollToElement`、虚拟化预算、分设备滚轮、滚动抑制后台轮询。
- S05：技能目录 watcher + `skills/change` + 防抖刷新。
- S09：终端命令式滚动 + 选择优先。
- S10：重写为 S08 流式任务视图、每行通知、流结束语义、`notified` 去重、`notify`/`quiet`。
- S12：只采纳导航 `f`/`k`、pill 分型、`subagents/prompt`；不采纳 `agentId`/`outputFile`/`notified`/2s/120s（DSH 没有这些）。
