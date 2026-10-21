# [2026-10-20-nano-v1/总纲] 极简、可维护的原生 TUI

- 组标识：`2026-10-20-nano-v1`。
- 角色：总纲，设计约束和依赖的唯一汇总入口，不是另一张全范围执行单。
- 状态：S01 已实施（#18）；S02 实施中（#5）；S04 已实施（#7）；其余子规格待逐份审核。
- 项目目的：[dsh-tui-nano #1](https://github.com/Strelizialeomon/dsh-tui-nano/issues/1)。
- 执行 issue：S01 执行单 #4、S02 执行单 #5、S04 执行单 #7；其余子件按合并顺序创建。
- 适用环境：首版正式支持 macOS + Ghostty，全屏模式；其他环境不宣称可用。
- 调查日期：2026-10-20；官方 Harness 基线 `0.2.1-rc.2`，上游源码 `687f282e8625c6b8d11ff58211a90524be2e4317`。

## 1. 目标与完成含义

这是一个长期维护的独立产品，不继承旧 dsh-TUI fork 的实现、兼容包袱和 Git 历史。Rust 负责终端交互，官方 DeepSeek Harness 负责 Agent、模型、工具、权限、会话与持久化。维护目标是让官方升级主要影响一层适配代码，而不是让每个界面理解官方内部实现。

首版必须让用户在真实终端完成：选模型与推理强度、引用文件和图片、调用技能、核对项目规矩、对话及工具执行、处理问卷与审批、恢复会话和压缩上下文、手动 Shell、管理任务、操作完整交互终端、让 Monitor 通知 Agent、配置定时任务，以及进入子代理对话再返回。

完成是有限承诺：约定功能、正常/异常/边界/回归测试通过，真实终端证据齐全，没有未处理的已知阻断缺陷。既不承诺以后没有 bug，也不以“再观察几天”无限延后完成。

不接受这些替代交付：只有漂亮假数据、用日志滚动冒充 Monitor、用命令输出框冒充交互终端、用本地勾选冒充权限切换、用浏览器截图冒充原生 TUI 验收。

## 2. 已确认决定

| 决定 | 本轮确认内容 | 影响 |
| --- | --- | --- |
| 实现边界 | Rust TUI + 薄 TypeScript 桥接插件 + 版本化本地通信 | 不重写 Agent，不借简版 SDK 缺失能力降级需求 |
| 范围 | 原候选 01-08、10、13，加 Shell、任务管理、Monitor、完整终端与调度 | 方案模式、检查点回退、MCP 管理不进首版 |
| 生命周期 | 随开随关，关闭 TUI 不继续工作 | 无常驻服务、开机任务或跨退出进程恢复 |
| 多窗口 | 同项目必须可开多个窗口，兼容 spec-flow 的多工作树开发 | 不锁整个项目，不因同仓库而拒绝启动 |
| 调度 | 沿用官方补投 | 周期只补最近一次；一次性过期仍尝试投递；不保证严格一次 |
| Monitor | 事件唤醒 + 通知限量合并都纳入 | 需要模型侧真实收件，限量不能静默丢失 |
| 人工命令 | `!` 在单次 `ShellExecRequest` 上传 `sandboxPolicy: ctx.sandboxPolicy.resolve({ mode: 'danger-full-access' })`；不改 mount、不改 profile 默认值；输出作为合成 user 上下文注入，不立即唤醒模型；没有 `!!` 特殊模式 | 作者选择；首次使用必须提示这是用户权限、不是 Agent 授权；`!` 输出不构成审批授权 |
| 平台 | macOS + Ghostty，全屏 | 不增加行内渲染或 Windows 验收承诺 |
| 拆分 | 1 总纲 + 13 执行规格 | 合入后配 13 执行 issue + 1 仅跟踪父 issue |
| 原样稿 | 最新子代理样稿优先于旧 Peek/Attach 草稿 | 进入子对话，不做同级主代理切换 |

“同项目只开一个窗口”的先前建议已被否决，不得在实现中以目录锁、项目单实例或全局端口冲突重新引入。

`!` 的 bypass 只作用于**单次** `ShellExecRequest`：`sandboxPolicy: ctx.sandboxPolicy.resolve({ mode: 'danger-full-access' })`。不要 mount `dsh-bash-local` 或改 profile 默认值，那会让 Agent 自己的 bash 一起失去沙箱。`!!` 只是原样交给 shell 的普通文本；`bash -c` 非交互、没有 history expansion，也不触发任何入模路径。

## 3. 不做什么

首版不做多 Agent 内核切换、旧 fork 插件兼容、通用插件市场、远程宿主、跨机器所有权、语音、WebSocket Monitor 直连、完整 xterm/GUI 功能对等、跨退出恢复原进程、无人值守自动部署、文件检查点回滚、自动 Git worktree 编排。

Agent 可继续使用当前官方配置已经提供的工具，但这不等于本产品承诺相应管理界面。使用官方预配置 MCP 工具，不等于实现 `/mcp` 安装/认证管理。项目规矩是文件指令，不等于自动跨会话记忆；首版不增加自动记忆写入。

长期维护不是预建所有扩展框架。新增模型后端、远程协议或插件系统须有独立需求，再扩边界。

## 4. 运行结构与责任

```text
Ghostty
  Rust nano 窗口
    ├─ 输入、焦点、阅读位置、选择复制、视图状态
    ├─ 一个受拥有权约束的本地子进程连接
    └─ 官方 dsh CLI + Nano profile
         ├─ 薄桥接插件：协议、边界校验、资源清理
         ├─ 官方会话控制 / 历史 / 投影 / 权限 / 附件 / 子代理
         ├─ 官方 Shell / jobs / 用户交互终端 / schedule
         └─ Nano Monitor 扩展：订阅输出、合并事件、投递公开 Agent 收件接口
```

每个窗口拥有自己的宿主，不寻找或接管用户已经打开的官方 dsh。不同窗口可在同仓库不同 worktree，或同一 checkout 的不同会话工作；文件冲突仍须靠 worktree 隔离，TUI 不宣称多个 Agent 修改同文件会自动合并。

每个宿主只驱动一个主会话及其受管理子代理。主会话切换由 S07 完成受控重启，不能留下旧主会话偷偷运行。该限制不限制项目窗口数量。

### 4.1 真正的所有者

| 对象 | 事实/执行所有者 | Rust 允许保存的内容 |
| --- | --- | --- |
| 会话、消息、工具结果 | 官方 session 与持久化 | 可丢弃的页面缓存、展示状态 |
| 模型与推理强度 | 官方模型选择与 request/header | 当前值/待确认值的展示 |
| 权限与审批结论 | 官方权限服务与审计事件 | 焦点、正在提交的请求 ID |
| 主会话输入 | 官方收件箱与接收回执 | 未发送草稿、待确认发送状态 |
| 子代理身份与续聊能力 | 官方 catalog、descriptor、控制接口 | 导航栈、子草稿与阅读锚点 |
| Shell/jobs/PTY | 官方执行、任务或终端服务 | 输出视窗、操作中的状态 |
| 定时任务 | 官方 schedule 服务和持久化 | 表单草稿；不能自己计时并重复派发 |
| Monitor | 插件拥有监听与投递，官方拥有被观察任务 | 显示插件确认的状态，不自行触发推理 |

所有方法名称、字段与错误码的线上约定归 S01 的通信契约；后续规格不另造第二套同义协议。文件路径规划是新项目建议，不表示相关源码已经存在。

### 4.2 多窗口与数据边界

共同的 Nano 数据根暂定 `~/.dsh-tui-nano`，可显式指定其他绝对路径。官方会话存储共用该根下的 `sessions/`。`storage-json` root 按根会话分区到 `runtime-data/<rootSessionId>/storage`，每个根会话一套 workspace/projection cache/schedule；不同窗口的 workspace 视图不共享，会话列表来自共享 `sessions/` 和 session-query。会话 ID 是不透明身份，不用工作目录名替代。

同一根会话的可写所有权由 S01 的会话挂载账本（宿主 pid 认领）保证；官方 session 写锁是会话物化后的第二道防线。调度另有 S11 的 `schedule.lock` 租约，保证一个根会话只有一个 scheduler。争用时明确提示已有窗口占用（pid），不删除锁、不强制接管、不降级成新会话。不同根会话的 storage-json/调度表/workspace 视图不共享；会话列表来自共享 `sessions/`。

**跨会话内容隔离是硬约束**：每个地址的 transcript、草稿、队列、附件、审批、问卷、终端、Monitor、调度都不跨根会话或父子地址出现。例外只有官方 fork seed 和父对话 settlement notice；其余情况一律按串会话阻断处理。

Nano 不直接解析或改写官方存储文件，不复制官方凭据。配置、凭据与全局指令的实际来源由 profile 显式解析并在状态报告中说明；数据路径覆盖不改变用户的 `DSH_HOME` 或其他 harness 配置。原子写入和权限检查沿用官方能力。不得在业务文件夹落秘密、日志或全局偏好。

S01 必须用真实官方服务验证：挂载认领在激活主会话期间确实持有；认领失败时调度尚未运行；两个根会话的 storage-json/调度表互不可见。S11 负责验证后端 `provide`、`routes.schedule`、探针和租约。无法证明就阻断对应能力，不能用 UI 禁用按钮代替互斥。

## 5. 故障和安全的共同规则

1. 请求超时、通信断开、已接受、已执行、已持久化是不同状态。没收到回执不等于操作没发生；不得自动重发有副作用命令。
2. 用户原文、代码和日志不因简化界面而改写。控制字符只影响安全展示；复制、原始记录与展示归一化必须区分。
3. 审批由真实请求产生，不增加“所有 Bash 必须确认”，也不把关闭弹窗当允许。无有效回答时拒绝/取消/不可用，绝不默认允许。
4. 人工终端拥有系统用户权限，Agent Shell 受官方策略约束。Agent 和 Monitor 不能借人工终端绕过审批。
5. 退出先停止新增工作，再停止监听、命令与 PTY，取消 Agent/子代理并排空持久化，最后结束自己拥有的宿主。失败逐项报告；不影响别的窗口。
6. Rust 异常退出或通信 EOF 触发宿主清理；强杀、断电和逃逸进程的保证按 S01 的能力证据说明，不能承诺所有 OS 场景必无残留。
7. 官方升级、未知必需事件、损坏存储和缺失能力都须显式失败，不自行猜字段或创建替代成功结果。
8. 不向日志输出密钥、完整环境、图片 base64、未经用户批准的模型请求内容；分享诊断时默认是无内容的结构与计数。
9. 只有明确的用户输入可以扩大授权或重置自动唤醒预算，外部日志和 Monitor 事件不能冒充用户授权。
10. 有界缓存不是删历史。出现裁剪、落盘失败、慢消费者或缺页必须标明，历史事实仍由官方记录查询。

## 6. 交付物地图

各子件聚焦一个完整成果，内部编码/测试/审核不另列里程碑。S01 已实施（#18），S02 实施中（#5）；其余子件在各自规格审核通过后开工。“可并行”指契约和前置成果满足后的实施关系。

| 子 spec | 产出 | 消费 | 文件 owner | 不碰 | 开始 / 合入 / 发布 | 波次 |
| --- | --- | --- | --- | --- | --- | --- |
| [S01 宿主与通信](S01-runtime.md) | 可启动、可关停的真实适配及共享协议 | 官方公开服务 | `protocol/`、`bridge/runtime/`、`bridge/profile/`、`crates/nano-runtime/`、构建根 | 各功能 UI | 无前置 / 独立 / S13 统一发布 | A |
| [S02 对话与输入](S02-conversation.md) | 真实对话和输入宿主 | S01 协议与生命周期 | `crates/nano-app/`、`crates/nano-conversation/`、`bridge/conversation/` | 其他功能目录 | 契约冻结后可写夹具 / S01 / S13 | B |
| [S03 阅读与决策](S03-reading-decisions.md) | 可读 transcript、差异、问卷和审批 UI | S02 消息与焦点接口 | `crates/nano-transcript/`、`crates/nano-decisions/`、`bridge/decisions/` | 模型/权限写路径 | S02 契约 / S02 / S13 | C |
| [S04 模型与权限](S04-controls.md) | 命令、模型、effort、权限读写 | S02 命令入口，S01 服务 | `crates/nano-controls/`、`bridge/controls/` | 审批 UI、执行策略内核 | S02 契约 / S02 / S13 | C |
| [S05 技能与规矩](S05-skills-instructions.md) | 技能发现/调用与规矩来源 | S02 输入；官方技能/指令服务 | `crates/nano-skills/`、`bridge/skills/` | 自建指令解析器、记忆写入 | S02 契约 / S02 / S13 | C |
| [S06 引用与图片](S06-references-attachments.md) | 文件补全、引用与图片发送 | S02 有序输入，S01 附件接口 | `crates/nano-attachments/`、`bridge/attachments/` | 技能解释、会话真源 | S02 契约 / S02 / S13 | C |
| [S07 会话与上下文](S07-sessions-context.md) | 会话选择/重启、命名、上下文与压缩 | S02 草稿；S01 生命周期 | `crates/nano-sessions/`、`bridge/sessions/` | 调度规则、直接改日志 | S02 契约 / S02 / S13 | C |
| [S08 Shell 与任务](S08-shell-jobs.md) | 人工命令、jobs 观察与停止 | S02 操作入口；S01 执行 | `crates/nano-jobs/`、`bridge/shell/`、`bridge/jobs/` | PTY、Monitor 投递 | S02 契约 / S02 / S13 | C |
| [S09 交互终端](S09-terminal.md) | 真实 PTY、输入/缩放/屏幕恢复 | S02 场景接口；S01 双向流 | `crates/nano-terminal/`、`bridge/terminal/` | 命令日志伪终端、Agent 终端内核 | S02 契约 / S02 / S13 | C |
| [S10 Monitor](S10-monitor.md) | 有界监听及真实模型收件 | S08 任务/输出/停止契约 | `crates/nano-monitor/`、`bridge/monitor/` | 自建进程执行器、调度器 | S08 契约 / S08 / S13 | D |
| [S11 调度](S11-schedule.md) | 官方定时 CRUD、投递与补投展示 | S02 入口；S01 会话归属 | `crates/nano-schedule/`、`bridge/schedule/` | 自写 Cron、跨窗口争抢任务 | S02 契约 / S02 / S13 | C |
| [S12 子代理](S12-subagents.md) | 子对话及父子导航 | S02 地址与草稿；S03 transcript | `crates/nano-subagents/`、`bridge/subagents/` | 独立子 Agent 内核 | S02/S03 契约 / S02、S03 / S13 | D |
| [S13 发布验收](S13-release.md) | 安装、版本矩阵、design-contract、跨功能证据 | 全部成果 | `tests/e2e/`、`docs/release/`、`packaging/` | 代其他子件补写功能 | 测试契约后可备数据 / 所有子件 / 本份负责发布 | E |

构建根由 S01 独占；功能注册 API 由 S02 独占并允许显式静态贡献。子件不各改一遍全局路由或依赖版本；需要新增根引用时由相应 owner 在其工作区接收唯一变更。没有通用动态插件发现。

## 7. 有方向的依赖台账

| ID 与提供方 → 消费方 | 类型 / 产物 | 就绪条件、证据、owner 与阻塞位置 |
| --- | --- | --- |
| D01 S01 → S02-S12 | 契约：版本化请求/流/错误、根会话身份、能力快照（含 `tool-ask-user` mode） | S01 契约及正常/失败夹具冻结可开始；真实宿主套件通过才合入。owner S01；B/C/D |
| D02 S02 → S03-S12 | 契约：输入路由、场景、焦点、草稿地址、功能注册接口 | S02 提交接口与夹具后可并行写功能；S02 可运行才合入。owner S02；C/D |
| D03 S02 → S03 | 数据：统一消息/工具身份、流式与历史合并 | S02 相同输入得到稳定 ID 的测试通过。owner S02；C |
| D04 S03 → S12 | 契约：同一 transcript 渲染器与决策入口 | S03 导出只读/可交互展示接口；子对话不复制渲染器。owner S03；D |
| D05 S08 → S10 | 契约：job ID、输出字节游标、非消费观察、停止结果 | S08 真实任务及跨 chunk/结束/取消夹具通过；Monitor 不碰模型的消费游标。owner S08；D |
| D06 S01 → S07/S11 | 数据/运行时：主会话挂载认领、启动顺序、受控重启 | 双窗口同仓不同会话可用；同根会话第二个宿主在挂 schedule 前被拒绝。schedule 后端/租约由 S11 自己的机制提供，不在本边。owner S01；C |
| D07 S01 → S09 | 契约：PTY 双向流、sequence/ack、背压和失效 | 完整官方 snapshot/output/input 示例与断流证据齐全。owner S01；C |
| D08 S03/S04 → S08/S10 | 配置/验收：真实审批响应、权限切换不能被旁路 | 功能可用默认策略先独立开始；扩大权限的真实路径通过后才发布。owner 写路径 S04、审批呈现 S03；E |
| D09 S01-S12 → S13 | 发布/验收：单项结果、真实接口证据和已知限制 | 子件逐项提交测试与人工记录，S13 交叉核对，不靠“全部看起来正常”。owner S13；E |
| D10 S13 → 全部子件 | 发布/验收：安装包、design-contract 冻结、同版本集成验收 | 各子件可先独立合入，不独立宣称首版已发布；同批发布由 S13 承担。owner S13；E |
| D11 S08 → S03 | 契约：job output 非消费观察、字节游标、终态 | S08 观察接口冻结；S03 不另建输出。owner S08；C |

D10 是发布条件，不是实施循环：S13 测试契约可先写，发布结论等待单项实现；不让子件等 S13 关闭才开工。父 issue 仅表达归属，不作为执行前置。

## 8. 调研依据与证据级别

以下链接固定到本次核对的官方提交。包文档、导出类型与本机安装包已读取；没有启动带凭据的 Agent 或声称已完成 Rust 集成。

| 编号与当场确认的事实 | 一手入口 |
| --- | --- |
| E01 简版 SDK 无 prompt-cancel/session-close，也没有现成交互审批请求 | [SDK protocol](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/sdk/protocol/README.md) |
| E02 官方前端通道与业务选择是 React 无关的，支持非页面/同进程 carrier | [Connection](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/client/connection/README.md)、[Gateway](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/api/gateway/README.md) |
| E03 会话跟随含 durable events、投影和非持久化助手流；回执只是接收 | [Session controller](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/api/session-controller/README.md) |
| E04 用户终端有完整双向 attach；与 Agent 终端是两个能力族 | [User terminal](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/api/terminal-controller/README.md)、[Agent terminal](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/experimental/tool-terminal/README.md) |
| E05 jobs 有事件、非消费读取与完成通知，但完成通知不等于逐事件 Monitor | [Jobs](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/jobs/jobs/README.md)、[Tool jobs](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/jobs/tool-jobs/README.md) |
| E06 Agent 有公开 send/followup/steer/inject；inject 不唤醒 | [Agent](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/core/agent/README.md) |
| E07 调度记录持久化、最新遗漏补投；投递非原子、有重复风险，无暂停 API | [Schedule](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/schedule/schedule/README.md) |
| E08 主会话模型选择使用真实模型目录与模型自身的 effort 元数据 | [Model selection](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/client/ui-model-selection/README.md) |
| E09 `/skill-name` 由后端确定性注入；命令重名优先走命令 | [Skill UI contract](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/client/ui-skill/README.md) |
| E10 项目指令由后端加载，刷新依赖结构化文件操作；不解析 `@path` 导入 | [Instructions](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/context/agent-instructions/README.md) |
| E11 权限 preset 不等于自行改两个 UI 开关；审批只有一次性允许 | [Presets](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/interaction/permission-presets/README.md)、[Approval](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/interaction/user-approval/README.md) |
| E12 子代理有续聊、一次性、外部/未知差异，不能都画可输入 | [Subagent](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/subagent/subagent/README.md) |
| E13 当前持久化提供跨进程 session 写锁；KV 域变更仍是进程内可见 | [JSONL](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/session/session-persistence-jsonl/README.md)、[Storage domain](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/storage/storage-domain/README.md) |
| E14 取消、清理和可观察进程范围依赖平台；macOS 不能保证追到所有逃逸后代 | [Subprocess local](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/subprocess/subprocess-local/README.md) |
| E15 手动压缩只在空闲时可用，无任意参数 | [Compact](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/compaction/command-compact/README.md) |
| E16 上下文和 Token 有估算/服务端采样区别，CJK 估算有误差 | [Token meter](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/packages/llm/token-meter/README.md) |
| E17 旧项目在进程内挂 Harness，正常退出释放资源；不提供长期常驻担保 | [dsh-TUI lifecycle](https://github.com/ccch1mneyyy/dsh-TUI/blob/a40c305a99ded8c8215da6e5d277f23abce4984a/docs/session-mount-runtime.en.md) |
| E18 Rust 终端可以复用现有绘制和 VT 解析，不需要手写模拟器 | [Ratatui](https://github.com/ratatui/ratatui)、[tui-term](https://github.com/a-kenji/tui-term) |

E18 仓库链接是当前项目入口，不是已锁定的构建版本；实施 S01/S09 时冻结依赖和兼容样本。本机目前 `rustc`/`cargo` 不在 PATH，本稿没有编译证据。

历史设计资料位于主工作区 `docs/design/2026-10-17-dsh-tui-ux-research/`，尚未提交。新稿把必要行为写成文字契约，不制造不存在的远端图片链接，不依赖旧项目路径运行验收。旧报告中与最新子代理详情、极简底部冲突的内容不继承。

## 9. 维护、升级与未验证项

- 官方包仅从公开导出消费，固定一套精确版本；不深导入 `src/`、私有类或发布包偏移，不复制 `runProfile` 启动源码。
- 原始官方事件与 Nano 视图字段有明确转换边界。升级时在桥接侧更新并用相同事件录制回归 Rust，不在 UI 散落版本判断。
- 支持矩阵只列实际验证过的组合。未知版本拒绝执行；首版不提供未知版本的离线只读模式。
- 每份功能只需自己的测试与官方依赖；真实组合无凭据测试优先。少量真实模型检查需用户授权预算，未跑不得宣布通过。
- 重要未知：最小 profile 无监听端口的完整组合；默认凭据/指令来源不被覆盖；真实双窗口/写锁与 KV 隔离；PTY 快照和 Rust VT 解析兼容；中文输入/选择复制；流式中审批/取消；Monitor 空闲唤醒及费用上限；退出清理的 macOS 范围。
- 上述未知是明确的证据要求，不是任意技术选型占位。S01/S09 无法证明公开能力满足需求时停止报告，不在实施期静默缩范围。

## 10. 机制清单

| 机制 | 解决什么 / 没有会怎样 | 确认与代价 |
| --- | --- | --- |
| 薄桥接 + 版本化通信 | 把官方类型变化限制在适配层；没有则 Rust 直接承担官方协议变化 | 用户已确认；维护两种语言及通信契约 |
| 随开随关的资源拥有权 | 关窗口不再工作，且不误杀其他窗口；没有则遗留进程或互相终止 | 用户已确认多窗口与退出目标；具体数据分区为本稿设计 |
| 会话挂载账本 | blank 会话没有官方跨进程锁；没有则两个宿主驱动同一会话 | 作者建议（参照 dsh-TUI）；同机、pid 复用、需维护锁文件 |
| 调度专用后端 + 租约（S11） | 按根会话隔离 schedule 表，并保证一个 scheduler；没有则双 timer 或串表 | 作者建议；`provide` + 完整 storage-domain config + `schedule.lock` |
| 人工命令合成注入 | `!` 用用户权限执行，输出作为合成 user 上下文注入且不立即唤醒；没有则人工命令变审批死路 | 按 Claude Code 实现；用户权限需明确提示，输出不是授权 |
| 跨会话内容隔离 | 任何 transcript/草稿/队列/任务/终端/Monitor 不跨地址出现 | 用户硬要求；需在 S01/S02/S03/S07/S09/S10/S11/S12 验收 |
| Monitor 事件唤醒 | 让日志变化进入 Agent；没有则只有日志查看 | 用户已确认；自动唤醒会花费模型额度 |
| Monitor 通知限量与合并 | 避免高频日志占满上下文；没有则费用与输入队列失控 | 用户已确认；通知可能延迟，预算耗尽必须可见 |
| 官方 schedule | 复用计时、保存和最新遗漏补投；没有则需另写调度内核 | 用户已确认；不提供暂停、执行成功或严格一次保证 |
| 子会话从属导航 | 保留子草稿、阅读位置和可靠返回；没有则串话或丢位置 | 用户已选择子代理能力并沿用新样稿；非全部子任务可续聊 |

界面级机制（有界阅读、原位展开、稳定锚点、单列问卷与焦点分离、虚拟化/滚轮曲线）由 S03 §8 定义，不在总纲重复列。

不引入项目单实例锁、全局常驻守护、通用插件容器、第二份 transcript 数据库、另一套调度内核或自动记忆机制。

## 11. 自定细节

这些是作者建议，不伪装为用户逐项指定：

- `crates/nano-*`、`bridge/`、`protocol/` 目录以及主会话驱动的宿主重启方式。
- 一个窗口只驱动一个主会话树；不同窗口不限制仓库/worktree，调度 KV 按 rootSessionId 分区。
- Nano 自有会话数据根，不自动导入官方或旧 fork 历史；现有凭据与指令来源通过公开 profile 配置使用。
- 通信容量、输出预算、忙碌指示、快捷键与性能验收初始门槛，分别由其唯一 owner 子规格定义，不凭旧测试数字冒充实测。
- 各功能以最终样稿的文字行为为基线；原始素材首次入库的提交与公开发布需要单独核对，当前不动主工作区。
- 当前仓库没有首个提交或远端主干。规格在独立 orphan 工作树形成，尚无可用 PR；不制造空提交、不推保护分支来绕过流程。

## 12. 审核修订记录

尚未选择本稿审核档位。自动文档核查只证明链接/结构等机械项，不证明产品设计正确；未审核、未实施、未完成真实终端验证。
