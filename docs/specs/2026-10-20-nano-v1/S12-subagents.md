# [2026-10-20-nano-v1/S12] 子代理

- 组标识：`2026-10-20-nano-v1`；角色：子件 S12，单交付为子对话进入、续聊、返回与只读边界。
- 状态：已实施（PR #34）；执行单 #15。
- 总纲：[共享约定与依据](README.md)。上游：S02 地址与草稿，S03 transcript。下游：无。
- 合入前提：S02/S03 的地址、草稿和渲染接口可用；本份用真实 spawn/fork 子代理和真实一次性子代理验证导航与输入边界。发布由 S13 承担。

## 1. 边界

拥有 `crates/nano-subagents/`、`bridge/subagents/`。提供子代理目录、进入/返回导航、子草稿、续聊入口和只读边界展示。

不实现独立的子 Agent 内核、不复制 transcript 渲染器、不自行创建子会话、不伪造子代理消息。子代理的创建、所有权、容量、持久化和续聊由官方 subagent 服务拥有；S12 只消费官方目录和消息接口。

## 2. 子代理身份与种类

子代理属于发起委派的对话，不是与原对话并列的独立聊天。身份来自官方直接父地址：

| 种类 | 来源 | 可续聊 | 界面 |
| --- | --- | --- | --- |
| 本地可续聊（spawn/fork） | 官方本地 provider，有持久子会话 | 是 | 进入详情；parent live 时可输入（child 运行中也可） |
| 本地一次性历史（one-shot） | 官方已完成记录，无 live 子代理 | 否 | 进入只读详情 |
| 外部（ACP/SDK/Codex/Claude Code 等） | 官方 external，无本地子会话 | 否 | 不可导航的叶子项，不进入详情 |
| 未知或目录不可读 | 官方诊断 | 未证明 | 只读并显示原因；确认可续聊后才恢复输入 |

界面不得把一次性或外部子代理画成可输入；没有官方可续聊事实时不显示输入框或发送按钮。外部叶子项可以显示任务名和结果入口，但不能伪造一个详情页。

**可续聊 ≠ 一定可输入**：只有 exact parent 仍 live 时才保留普通输入，且 child 运行中输入/Send 仍可用；parent 不可用且 child 未运行时是只读 composer；parent 不可用且 child 运行中输入/Send 禁用，但独立的 Stop 仍可用。界面必须按这个前提显示，不能因为 mode=continuable 就无条件给输入框，也不能把 parent live 的运行中 child 误禁。

## 3. 目录与导航

- 原对话底部只列直接子代理，最多 4 个可直接展示，多余折叠为一个入口；不列父级对话。外部叶子项显示为不可导航项。底部 pill 显示 `N local agents`，不与 shell/monitor 混计。
- 底部状态灯按 S13 design-contract：running 用 accent 亮/暗两态、0.8s 间隔闪烁；completed 绿；aborted/refusal/max-tokens 黄；error 红；unknown/inactive 用 muted。reduced-motion 的检测方式（设置或环境）由 S13 contract 定义，开启时闪烁静止。Dock 显示 4 个直接子代理 + 1 个溢出入口；只有 `subagent/end` 终态约 1.5 秒后清理，可续聊子代理的普通回合完成不清理。
- 底部 dock 的键盘操作：↑↓ 选择，Enter/→/Space 进入，`f` 打开完整 transcript，`k` 停止，Esc 返回。停止只停当前回合，不清空排队消息（见 §6）。
- 全部列表显示递归发现的后代，带父级关系；不可读的分支显示诊断，不静默丢弃。
- 进入方式：点击、聚焦后 Enter/→ 或 Space；外部叶子项不响应进入。进入后隐藏原对话的同级任务列表，顶部只标识「子代理」和任务名称。
- 返回：不论焦点在输入框、正文、工具详情还是发送按钮，Esc 都直接退出；顶部退出按钮和空输入时 ← 执行同一返回。IME 组合态和问卷/审批焦点优先按 S03 的规则处理，不强行退出。
- 从底部进入的，退出后回原对话和对应入口；从全部列表进入的，退出后回该列表和选中行，再按 Esc 关闭列表。
- 原对话草稿、光标和阅读位置保留；已消失的入口回退到输入框。
- 首版按官方 `maxDepth`；默认 1 层。只有官方目录返回多层时才显示后代，不把多层栈当作已确认的交互承诺。
- 子代理详情只显示该子地址的内容；父对话、兄弟子代理和其他会话的消息不出现在子详情。切换子地址后旧草稿/消息不出现。
- 目录来自官方父会话投影，列目录不读取子会话正文、不激活冷子代理。

## 4. 子对话与草稿

- 子对话使用与主会话相同的 transcript 渲染器（S03），不复制第二套展示逻辑；工具、差异、问卷和审批遵循同一规则。
- 每个子地址有独立草稿、光标和阅读位置；进入/退出只切换视图，不停止任务、不丢弃草稿。草稿按 S02 的地址契约保存，S12 不另建一份存储。
- 可续聊子代理：Enter 发送，Shift+Enter 换行；中文输入法组合态中 Enter 不提交。人类续聊走官方 `subagents/prompt`，显式选择 `queue` 或 `steer`；`sendMessage` 是 agent-to-agent 路径，不能拿来发送用户消息。
- 一次性子代理：只读显示最终记录；没有输入框、没有续聊按钮，不显示「等待输入」的假状态。外部叶子项没有详情可进，不伪造只读记录。
- 子代理消息中的图片等附件只有官方声明模型支持且附件准入成功时才发送；通用文件上传在子代理一律被官方拒绝，不提供入口。不支持时明确拒绝并保留草稿。
- 子代理里的审批策略由官方钉死为 `never`，需要审批的操作自动拒绝；S12 不渲染一个能点「允许」的审批卡，否则就是伪造授权。只显示官方的拒绝结果。
- 官方标识是 `childId`/`subagentId`/`childSessionId`，子代理正文就是 child Session；DSH 没有 `outputFile`、`agentId` 输出文件或 `local_agent` 任务表。S12 不另建输出文件或第二份 transcript。
- DSH 子代理一开始就异步：服务层 `startActivation` 返回 `{ childId, messageId?, result, dispose }`，模型工具结果规范是 `{ kind: 'activation', subagentId }`；`childId` 不是工具结果字段。结果走 completion notice，不存在 foreground→background 转换；界面显示控制面 `activity: running|inactive` 和 elapsed，不做 2s/120s 自动转后台。
- 未知模式的行可以点击；只有读取 child history 确认了支持的 mode 后才恢复输入。读取通过 S03 的展示接口，不直接读原始日志。
- 退出子对话只结束查看，不停止子任务；子任务的成功、失败、终止仍由官方记录和父对话保留。

## 5. 完成态与入口清理

- 状态分两层：持久 catalog 条目只有成员身份、不带执行状态；控制面 `list_agents`/`SubagentCatalogRow.activity` 才有 `running|inactive`。执行结果/`stopReason` 从 `subagent/end` 生命周期事件读取，`lastTurnCompleted` 从 child session 的 `subagentTiming` projection 读取。
- 只有官方 `subagent/end` 的 `SubagentRunEndInfo.stopReason` 才是完结态；该联合可被后端扩展，已知 completed/aborted/error/max-tokens/refusal 之外一律按 `unknown` 处理，不猜成功。
- 目录/投影没有给出终态时显示 `unknown`，不猜成功。
- 底部列表在官方终态后短暂显示再清理，约 1.5 秒为建议值，实际由实现定；可续聊子代理的普通回合完成不清理。
- 当前正在查看的子代理详情不会被定时关闭；父对话保留官方 settlement notice 作为结果入口，由 S03 渲染。
- 入口消失不等于记录丢失；从全部列表或父对话历史仍可找回。
- 子代理失败或被终止时，父对话显示明确结果和原因，不显示成成功；没有结果时不编造摘要。
- 父对话结束或宿主退出时，子代理随官方所有权清理；不留下独立后台子代理。

## 6. 续聊、取消与容量

- 只有直接可续聊子代理允许续聊；冷启动续聊由官方管理，失败时显示官方错误，不新建替代子会话。
- 人类发送走官方 `subagents/prompt`：`queue` 排队到下一回合，`steer` 在最近步骤处理；空闲目标开始新回合。界面显示「已发送」不等于「已执行」；超时/结果未知按 S02 的未确认语义处理，不自动用新 requestId 重发。
- 用户 Stop 走官方 `interruptByParent`：只停当前回合，排队消息保留，已发布的后代继续跑。`dispose()` 会停整个 activation 及后代，但它属于 activation 持有者（委派父/工具），不是 S12 视图；S12 不能用它冒充「整棵子树取消」。
- 容量上限（默认 8 个活跃子代理）按每棵 activation 树计算，根父级不占名额；到达上限时显示官方拒绝原因，不排队、不顶替现有子代理。
- 子代理不能借续聊扩大自己的权限或重置父会话的策略；冷恢复只恢复 persona/toolFilter，不恢复权限字段，实际 scope 以官方记录为准。
- 一次性/外部子代理的执行结果只属于那次执行；不提供「再问一次」伪装成续聊。

## 7. 失败与不变量

1. 子代理目录、可续聊性和消息事实唯一来源是官方服务；Rust 不维护第二套目录。
2. 没有官方可续聊事实时不显示输入框；不把一次性子代理画成可输入。
3. 进入/退出不停止任务、不丢草稿、不丢阅读位置。
4. 目录不可读的分支显示诊断，不静默消失。
5. 子代理消息不能扩大权限或改变父会话授权。
6. 完成态自动清理只影响底部入口，不影响记录和父对话结果。
7. 子代理的创建、容量和清理由官方所有权管理；S12 不擅自创建或重启子代理。
8. 子代理详情、草稿和消息按地址归属；父对话、兄弟子代理、其他会话的内容不出现在当前子详情；切换地址后旧内容不残留。例外：官方 fork 子代理带父已完成回合的 seed，继承内容属于 fork 本地址，必须正常显示，不能当成串会话。
9. 外部子代理不可导航；一次性历史只读；不把 external 画成有子会话。
10. 可续聊子代理在 exact parent live 时保留输入（child 运行中也可）；parent 不可用且 child 未运行时只读，parent 不可用且 child 运行中输入/Send 禁用但保留独立 Stop。
11. 未知模式只有在读 child history 确认支持的 mode 后才恢复输入；读取走 S03 接口，不直接读原始日志。

## 8. 验收

- [ ] 真实 spawn/fork 子代理出现在底部，最多 4 个直接展示，多余折叠；全部列表显示后代与父级关系。
- [x] 点击、Enter、→、Space 都能进入本地子代理；Esc、顶部按钮、空输入 ← 都能返回，草稿与阅读位置保留。（真实 TUI 验证 Enter/Esc；→/Space/点击/←/顶部按钮由单测覆盖）
- [x] 本地 one-shot 历史用历史 descriptor 夹具验证只读详情；不要求当前 provider 产生 one-shot；外部子代理为不可导航叶子。
- [x] 状态分两层：持久 catalog 只有成员身份；控制面 `list_agents`/`SubagentCatalogRow.activity` 才报 running|inactive；`stopReason` 从 `subagent/end` 读取，未知值按 `unknown`；`lastTurnCompleted` 从 `subagentTiming` 读取。
- [x] exact parent live 时输入/Send 可用（child 运行中也一样）；parent 不可用且 child 未运行时只读 composer；parent 不可用且 child 运行中输入/Send 禁用但 Stop 可用。
- [x] 未知模式的行可点击，读 child history 确认 mode 后才恢复输入；不直接读原始日志。
- [x] 底部状态灯：running 0.8s 亮/暗闪烁；completed 绿；aborted/refusal/max-tokens 黄；error 红；unknown/inactive muted；reduced-motion 开启时静止；dock 显示 4 个直接子代理 + 1 个溢出入口。（真实 TUI 只看到 running 暗相；其余灯与相位由单测覆盖，reduced-motion 未做真实设置）
- [x] 只有 `subagent/end` 终态约 1.5 秒后清理；可续聊子代理的普通回合完成不清理，记录仍可从全部列表找回。
- [x] 底部 dock：↑↓ 选择、Enter/→/Space 进入、`f` 看 transcript、`k` 停止、Esc 返回；pill 显示 `N local agents`，不混 shell/monitor。
- [x] 子对话中的工具、差异、问卷与主会话渲染一致；审批策略被官方钉死为 `never`，不出现可放行的审批卡。
- [x] 子代理草稿独立；退出再进入恢复，不影响父草稿；中文输入组合态不误发。（组合态沿用 S02 的 InputBuffer 门控，未在本份重复真实 IME 截图）
- [x] 人类发送走 `subagents/prompt` 的 `queue`/`steer`；Stop 走 `interruptByParent`，排队消息保留、后代继续跑；不把 `dispose` 当 UI 取消。（真实 stop 在回合结束后调用；排队保留以后端语义为准）
- [x] 冷续聊、容量上限（每棵 activation 树默认 8）、目录不可读、子代理已销毁分别显示官方错误，不新建替代子会话。
- [x] fork 子代理显示官方继承的父回合 seed；除 seed 外，父/兄弟/其他会话内容不出现在子详情。
- [ ] 子代理消息和图片附件按官方续聊与附件规则处理；通用文件在子代理被官方拒绝，不提供入口。
- [x] 本份的真实子代理证据、只读边界和失败夹具随交付提供；未测的外部 provider 明确标注。

## 9. 机制清单

| 机制 | 解决什么 / 没有会怎样 | 确认与代价 |
| --- | --- | --- |
| 按官方种类区分可续聊 | 避免把一次性执行画成可输入；没有则用户发消息无人接收 | 官方契约；需要目录字段 |
| exact-parent-live 输入前提 | 防止 parent 不可用时画一个发不出去输入框；没有则发送必被拒 | 官方契约；需区分只读/禁用/Stop |
| 外部叶子不可导航 | 外部没有本地子会话；没有则伪造详情或空页 | 官方契约；界面少一层入口 |
| 递归后代发现 + 诊断 | 全部列表能看到后代和不可读分支；没有则静默丢任务 | 官方 listChildren；需显示诊断 |
| 只读父投影、不读子日志 | 列目录不激活冷子代理；没有则列表变慢或串会话 | 官方契约；子正文走 S03 |
| 控制面 activity vs 持久 catalog | 持久 catalog 无状态，控制面才有 running/inactive；没有则状态来源错 | 官方两层；需分别读取 |
| catalog activity + `subagent/end` | 不把「回合完成」当子代理完结；没有则三态靠猜 | 官方 lifecycle/projection；需订阅事件 |
| 附件准入/通用文件拒绝 | 子代理只收图片且需模型支持；没有则发送必失败 | 官方契约；不提供通用文件入口 |
| 从属导航 + 单层返回 | 子对话属于父对话，返回落点可预期；没有则串话或丢位置 | 用户已确认样稿方向；默认 1 层 |
| `subagents/prompt` queue/steer | 人类续聊走正确通道；没有则伪装成 agent-to-agent | 官方契约；sendMessage 只给 agent |
| 审批钉死 `never` | 不伪造可放行的审批卡；没有则用户以为能授权 | 官方契约；只显示拒绝结果 |
| 子地址独立草稿（D02） | 父子互不串稿；没有则切换丢输入 | 作者建议；仅进程内 |
| 容量 8（每棵 activation 树） | 不排队、不顶替；没有则误判可用 | 官方契约；根父级不占名额 |
| `interruptByParent` 只停当前回合 | 不误杀后代和排队消息；没有则 Stop 变成整树取消 | 官方契约；与 `dispose` 区分 |
| fork seed 例外 | 官方 fork 继承父已完成回合；没有则误判串会话 | 官方契约；只显示 seed 内内容 |
| 复用同一 transcript 渲染器 | 不产生第二套展示和交互；没有则行为分叉 | D04 契约；S12 不复制渲染 |
| 底部入口限量清理 | 不占用主视图；没有则长期堆叠 | 用户已确认完结态规则；只有官方终态才清理 |

## 10. 自定细节

- 底部最多 4 个直接子代理 + 1 个溢出入口、`subagent/end` 终态约 1.5 秒清理、可续聊普通回合不清理，均按 S13 design-contract 的初始值执行；改值需更新契约，不能由实现任意调整。
- 底部 pill 显示 `N local agents`，不与 shell/monitor 混计；状态灯颜色/闪烁按 S13 design-contract（running accent 0.8s 亮暗、completed 绿、aborted/refusal/max-tokens 黄、error 红、unknown/inactive muted），reduced-motion 时静止。
- 全部列表的默认排序按父事件顺序；S12 不另造最近使用排序。
- 子代理输入框的提示语、只读标记和错误文案由 S12 定；最终键位由 S13 的 design-contract 冻结，不交给 S02。
- 首版不提供跨父会话的子代理全局搜索。
- 首版按官方 `maxDepth` 默认 1 层；多层导航不作为已确认的产品承诺。
- 嵌套深度由官方 `maxDepth` 决定，S12 不额外限制，但只渲染官方目录返回的层。

- 实现期临时键位（待 S13 design-contract 冻结）：输入草稿为空时 ↑↓ 聚焦 dock；Enter/→/Space/f 进入，`k` 停止选中，`l` 打开全部列表，Esc 回输入；空 dock 时 `Alt+L` 仍可打开全部列表。全部列表 ↑↓ 选择、Enter 进入、`r` 刷新、PageUp/PageDown 翻页、Esc 关闭。子视图 Enter 发送、Shift+Enter 换行、Alt+Enter 插话、Alt+K 停止本轮，Esc/空输入 ←/顶部按钮返回。
- 实现期默认值（待 S13 冻结）：running 灯 800ms 亮/暗；终态清理 1.5s；子视图缓存最多 16 个地址；reduced-motion 通过 `NANO_REDUCED_MOTION=1`/`true` 静止；dock 时长显示 `<1s` 为 ms、`<60s` 为一位小数秒、更长为 `XmYYs`。
- 子代理 label、诊断 id/reason 和错误文本渲染前把控制字符替换为 `?`。

## 11. 审核修订记录

2026-10-22 实施 PR #34 重审（用户点重审；2 个只读 agent：规格符合性 + 对抗找 bug）。原始结论见 PR #34 评论。合并 S10 Monitor 后重跑了真实模型/TUI 证据。处置如下：

| # | 严重度 | 发现 | 处置 |
| --- | --- | --- | --- |
| 1 | 阻断 | 子视图停在 ReadOnly 占位，continuable 无法展开，退出再进入丢阅读位置 | 已修：describe 后按模式安装 Interactive/ReadOnly 视图；单测断言 source |
| 2 | 高 | describe 返回前 gate 已 Enabled，父草稿可能被子地址发出 | 已修：resolved 前固定 Resolving；父草稿不显示、不可发送；回归测试 |
| 3 | 高 | 子输入裸 `k` 与消息输入冲突，StopOnly 非空草稿无法停止 | 已修：子视图改 Alt+K；dock 的 `k` 保留 |
| 4 | 高 | 子视图 status_hint 永远显示 gate，官方错误/回执不可见 | 已修：error/notice/根审批优先，再退回 gate；单测 |
| 5 | 高 | 可续聊子代理 `subagent/end:completed` 后 dock 被清空，全部列表不可达 | 已修：continuable 保留入口；空 dock 用 Alt+L 打开列表；单测 |
| 6 | 中 | describe/prompt/interrupt 未校验 parent 可达本宿主 root | 已修：统一 assertReachable；跨根拒绝单测 |
| 7 | 中 | 子地址 live 帧未按地址过滤，兄弟/根流进入子订阅 | 已修：publish 按订阅地址过滤；单测 |
| 8 | 中 | in-flight subscribe 无记账，Esc/重进产生重复订阅与历史 | 已修：generation 标记；过期响应立即 unsubscribe；单测 |
| 9 | 中 | 子 prompt 失败恢复写到当前草稿而非发起地址 | 已修：per-address backup + restore；单测 |
| 10 | 中 | 子 prompt 未接入 S02 未确认提交语义 | 已修：track_submission + accepted/failed/unknown；child transcript 有回显 |
| 11 | 中 | `lastTurnCompleted` 被当成终态绿灯 | 已修：终态只认 `subagent/end`；单测 |
| 12 | 中 | 根审批卡在子视图仍吃键/粘贴，确认会带子地址 | 已修：子视图挂起 decisions 键/粘贴；状态行提示根审批 |
| 13 | 中 | 子视图 `@` 文件补全走 root-only 接口 | 已修：子视图取消补全 |
| 14 | 中 | 挂载子视图不清空根面板/队列编辑/焦点 | 已修：mount 重置焦点并关闭根面板/编辑 |
| 15 | 中 | 子视图显示根 `!` 本地记录；`!!` 记录泄漏 | 已修：子视图不追加根 bash records |
| 16 | 中 | 协议 describe 字段、diagnostics shape、hasChildren 三层不一致 | 已修：补齐 shape、可选 hasChildren、删除未发布的 changed |
| 17 | 中 | one-shot descriptor 夹具缺失 | 已补：`bridge/test/fixtures/subagent-one-shot.json` + 测试 |
| 18 | 中 | ReadOnly 提示把 one-shot 误写成父不可用 | 已修：提示中文化 |
| 19 | 中 | label/诊断/错误文本未过滤控制字符 | 已修：safe_text 替换为 `?` |
| 20 | 中 | 容量/取消错误映射不可达或误映射 | 已修：cause 映射 capacity；`gateway/cancelled` 映射 `nano/cancelled`；单测 |
| 21 | 低 | 子视图状态栏显示根 `workspace-write` | 已修：显示「子代理（审批由官方拒绝）」 |
| 22 | 低 | `blink_on` 只亮前 400ms 不再循环 | 已修：800ms 模相位；轮回单测 |
| 23 | 低 | 与 S12 无关的 rustfmt 噪音 | 已撤销 |
| 24 | 低 | 证据 README 覆盖说法强于实际、未绑定 HEAD | 已修：刷新证据、补未测项、绑定 evidence commit |
| 25 | 记录 | snapshot activity 用 live 注册表，list 用控制面，resident-idle 可能短暂不一致 | 保留并写明：与官方两个投影的差异，不另造判定 |
| 26 | 记录 | 实现期临时键位/默认值未入 §10 | 已补 §10，标注待 S13 冻结 |
| 27 | 记录 | 外部 provider、真实 one-shot provider、8+ 容量、图片续聊、鼠标点击、5+ 折叠、真实多层树未测 | 证据 README 逐项标注；不宣称已测 |

验收已按本表勾选；第 1/15 项保留未勾，未测项见证据 README。