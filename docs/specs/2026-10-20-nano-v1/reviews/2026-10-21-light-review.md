# 轻审发现（2026-10-21，12 份改稿）

## S05（5 major / 3 minor，无 blocker）

- [major] S05:21 — 「文件监听 + skills/change 双通道」不是两条独立通道；官方文件 provider 自带 watcher，变化时调 invalidate()，注册表才发 skills/change，是一条因果链；S05 自建 watcher 无法清 completed catalog 缓存。
- [major] S05:30 — 把「技能被删除」列为必须显示明确错误，与官方 /name 语义相反：未知/已删除/被关闭用户调用一律按普通 prose 通过，宿主不报错。
- [major] S05:20 — 「可能不完整」的官方数据源没写清：只有进程内 ctx.skills.snapshot().complete，skills/list RPC 没有 completeness 字段；要用就得由 bridge/skills 供数并经 S01 契约暴露。
- [major] S05:31,69 — 技能卡片归属/形状错：渲染属 S03 transcript；用户显式 /name 没有 tool call 和参数，只是带 skill-invocation source 的 user 消息 + 注入正文。
- [major] S05:79-85 — 机制清单 5 行缺「预览」。
- [minor] S05:46 — 「最后是本地覆盖文件」不准确：官方按目录 broad→specific，每个目录先 base 再 local overlay。
- [minor] S05:20 — 把「名称冲突」归进「可能不完整」是误判：重名由官方确定性取第一，complete 仍为 true。
- [minor] S05:40 — 「超出读取上限」是自定细节，但没进 §9。

## S12（2 blocker / 5 major / 1 minor）

- [blocker] S12:48,89,107 — 说官方子代理有 agentId 和「输出文件」，但 DSH 没有 outputFile/子代理输出文件；官方标识是 childId/subagentId/childSessionId，正文就是 child Session。
- [blocker] S12:54,89,107 — 把 Claude Code 的 local_agent 任务模型写成官方状态：running/completed/failed/killed + notified；DSH 只有 live activity running|inactive，终态是 SubagentStopReason，无 local_agent/notified。
- [major] S12:49,89,108 — 2s/120s 自动转后台照搬 Claude Code 前台→后台；DSH 子代理工具一开始就异步，调用立即返回 childId，没有 foreground/background 状态可转。
- [major] S12:37,81,98 — 「父对话消息不出现在子详情」对 fork 不成立：fork 子代理带父已完回合的 seed；应改成「除官方 fork seed 外不串入其他地址内容」。
- [major] S12:107 — 机制清单写「复用 S08 任务模型」，把 S08 变成隐式上游；总纲 §6/§7 无 S08→S12，官方子代理不是 job。
- [major] S12:101-115 — 机制清单漏报 subagents/prompt queue/steer、审批 never、容量 8、附件准入/通用文件拒绝；§3:30 固定 f/k/Enter/→/Space，§10:121 又说键位由 S02 冻结。
- [major] S12:20,44,65,75 — 「本地可续聊」一律写成可输入，漏 exact-parent-live 前提：parent 不可用且 child 未运行时只读 composer，运行中输入禁用但 Stop 可用。
- [minor] S12:55-56,93,114 — lastTurnCompleted 在 child session 的 subagentTiming projection，不在 catalog；stopReason 在 subagent/end 的 SubagentRunEndInfo，不在 catalog/通知文案。

## S09（1 blocker / 7 major）

- [blocker] S09:19,21,47,50 — 每窗自持宿主下仍承诺跨窗口崩溃的关闭身份、离线终端列表和启动清理重试；官方 WebTerminalId 限定 one Session + one Host lifetime，closedIds 是 owner 内存 Set；崩溃后旧 id 可被重新创建，清理重试没有对象。
- [major] S09:46,74,87 — 关闭四态 cleanup-failed/unknown 不是官方状态；官方 client phase 是 idle/loading/creating/connecting/connected/disconnected/closing/closed/failed，close 失败标 failed，传输失败标 disconnected。
- [major] S09:27 — 「宿主只对可接受命令回 ack」与官方相反：控制权拒绝也回 ack，带 refusal: read-only|not-running；只有序列缺口/超 maxInputBytes/越界尺寸才抛错结束附件。
- [major] S09:31,68 — 「Host 侧错误不能把输出附件一起杀掉」与官方相反：越界 resize 抛错会 follower.fail() 关闭该附件的输出流；边界是 attachment 结束、进程和恢复屏幕保留。
- [major] S09:26,37,66,99 — 「超出 scrollback 显示缺口」没有官方数据源：snapshot 是 headless xterm serialize，无截断/缺失字段，旧行静默丢弃。
- [major] S09:5-6 — 与总纲 §6/§7 台账不一致：总纲 S09 是「S02 契约 / S02 / S13」，D07 就绪证据；本份只写 S01 PTY，下游无。
- [major] S09:18-20,42,48,82-94 — 机制清单仍未与正文一一对应；窗口持有没映射到官方 retain() hold；attach 不持有，先 retain 再 attach。
- [major] S09:98,100 — 越权把最终键位/主题交给 S02 并排除 S13；S13 拥有 design-contract 的统一键位/布局/主题。

## S13（1 blocker / 6 major / 1 minor）

- [blocker] S13:14 — design-contract 声称 S13 拥有并冻结统一键位/布局/主题，但 S02/S03/S07/S09/S12 把最终键位/主题交给 S02 应用壳，S02 又说 S13 不拥有键位表；同一判定两个 owner。
- [major] S13:5 — 「下游：无」与 S13 作为发布闸门及 design-contract 提供方相反；台账没有 S13→各子件契约边。
- [major] S13:28,88 — 未知版本「不提供离线只读模式」与总纲 README:174「允许离线查看、不得写会话」相反。
- [major] S13:72 — 阻断缺陷六类定义与正文用法不一致：缺失子件、键位/主题冲突都叫阻断项但不在六类；数据丢失/会话无法恢复/内容泄漏范围未定。
- [major] S13:45,90 — 跨会话隔离写成「任何内容不跨」，没有判定口径；合法同文内容会误报，S12 允许父对话显示 settlement notice。
- [major] S13:52,96,118 — 真实截图/录屏与「证据不含绝对路径/PII」矛盾，且没有脱敏方法/原件存放规则。
- [major] S13:96,112 — 证据绑定写「构建、提交或安装产物哈希」三选一；官方包只有 version，commit 无法从安装产物自证；应强制绑定安装产物哈希。
- [minor] S13:88 — 版本矩阵在 S01 和 S13 都出现，没有输入/输出分工。

## S02（5 major / 3 minor，无 blocker）

- [major] S02:44 — 把 `/` 命令和 `/skill` 调用都写成「由 S08 注册的入口处理」；owner 表是命令归 S04、技能归 S05、`!` 归 S08。
- [major] S02:127 — 声称排队/插话键位由 S02 自行冻结、S13 不拥有键位表；与 S13 design-contract 冲突。
- [major] S02:115 — 机制表把用户消息身份写成 source.rpcId；正文/官方用户消息主键是 seq，rpcId 只在 user-rpc 扩展源上。
- [major] S02:28 — 把 bash-mode 当官方来源；官方 MessageSourceMap 只有 user/model/tool/system-prompt，bash-mode 是本机作者标记，需进自定细节/机制清单；inject 收 UserMessage 并以 user-role 入历史。
- [major] S02:72 — 「停止并清空」只要求说明 claim 竞态，没定义竞态发生时的可观察行为；验收不可判定。
- [minor] S02:68 — 「先移除附件再编辑」没有官方队列动作；QueueAction 只有 edit/remove/steer，edit 整体替换文本。
- [minor] S02:26 — 工具结果身份写成结果块里的 toolCallId；V4 是 tool-role 消息顶层 data.message.toolCallId。
- [minor] S02:73 — 把 completed/killed/failed 当任务终态；这是 JobStatus，Agent turn 的 turn/end reason 是 completed/aborted/blocked/error/max-tokens/interrupted/forked。

## S07（5 major / 3 minor，无 blocker）

- [major] S07:36 — 恢复路径写「只调用 resume」，但官方 Remote 端点没有 session/resume；Host 入口是 ctx.sessionController.resolveAgent，存在性检查是 inspect；session/conflict 只在 create 侧。
- [major] S07:17 — 「不满足时官方会新建/报冲突，如实显示实际 ID」与 :37/:79「create 返回不同 ID 即硬失败」冲突；官方 create 对已开始/归属不符/被占用会 createMinted 新 ID。
- [major] S07:45,68 — 官方 rename 不是纯标题写入：session/rename 先 resolveAgent 再 titles.rename(liveSession)，冷会话会 resume、拿写锁并产生新 live Agent；不能在列表里给非活动会话改名。
- [major] S07:27,30,67,109 — S07 把「按地址保存草稿/光标/阅读位置」写进自己的切换流程；草稿归 S02，阅读锚点归 S03；S07 只发保存/恢复请求。
- [major] S07:31,80,81 — 切换流程无条件要求「认领成功后取得官方写锁」，但新建空白会话尚未物化，没有可确认的跨进程写锁；验收「第二个窗口不创建新宿主」与 S01 起宿主→认领→失败退出顺序矛盾。
- [minor] S07:52-53 — contextWindow 是 contextPressure 的字段（最新 request/context 容量），不是按当前模型现查的目录值；切模型过渡窗口可能错配。
- [minor] S07:5-6 — D06 方向写成「下游」；合入前提漏 README §6 的 S02 合入门槛。
- [minor] S07:20-21,93-100 — 机制清单缺「列表只读官方查询/投影、不打开冷正文、不激活子代理」；「未命名」占位没进自定细节。

## S08（6 major / 2 minor，无 blocker）

- [major] S08:49,72 — 「模型侧完成通知仍然应当到达」写成无条件；官方在模型正等待同一任务时 settlement 记为 awaited，不再发 notice，前台结果带 [stopped: reason]。应改成「恰好一个渠道」：后台走通知，等待中的前台调用走工具结果。
- [major] S08:89 — notified 去重照搬 Claude Code TaskStop，和官方人工停止契约相反；DSH 的 human kill 通知仍 due，没有 notified 字段。
- [major] S08:84 — 「单一任务输出源」描述的是 Claude Code TaskOutput（文件 fd/8MB/watchdog），不是 DSH jobs（内存 ring、retainBytes 262144、settledRetainBytes 16384、pumpPollMs 150、lossy read、可选 spillPath）。
- [major] S08:86 — 「卡住检测」没有正文/验收/owner，且 S08 边界不做 Agent 投递；要么删除，要么明确只给人看并补阈值/可关/误报验收。
- [major] S08:20,26,94 — `!` 的官方 Shell 用户权限路径没落到字段：必须显式传 sandboxPolicy: danger-full-access（或 mount dsh-bash-local）；inject 在运行中 Agent 的最近 step boundary 取走，idle 才等 follow-up，不是「下一条消息才能看到」。
- [major] S08:52,85,69 — 「自动转后台显示新任务 ID」不对：官方命令启动时就注册成 job，超时后 keeps running as the job it already was；promotion 是 best-effort，验收依赖配置/容量。
- [minor] S08:70 — 验收「所属调用与官方投影一致」不可执行：JobView 没有 toolCall；只能按 jobId 从 job/registered 关联。
- [minor] S08:5 — 依赖方向写错：D08 是 S03/S04 → S08/S10，S08 是消费方；且 S08 没有 D08 的负向验收。

## S11（2 blocker / 3 major / 3 minor）

- [blocker] S11:51-53 — 公开后端配方不成立：只 backend.register 不够，还要 ctx.provide(storageBackendServiceKey('nano-schedule-json'), backend)；--patch 的 insert 只追加根列表、config 整替换，不能把后端行排到 storage-domain 前，也不该靠改行序；应靠 inject 排序。
- [blocker] S11:69,99 — 租约损坏/接管语义互斥：69 说 pid 死或内容损坏就 unlink 重试，99 又要求损坏时拒绝挂 schedule、不当作空锁；必须选一条。
- [major] S11:53,86,101 — 「启动时校验 routes.schedule 指向 nano-schedule-json」没有公开 API：DomainFacility.config.routes 私有，backend.names() 只证明注册过名字；应改成写探针记录并检查 per-session 文件。
- [major] README:83,133 / S11:5,51-53,66,120 — owner/台账冲突：D06 把 schedule 后端/租约列为 S01 交付，S11 又说由 S11 定义；同一交付两个 owner。
- [major] S11:34,96 / S07:107 — 归档流程验收没有 owner：S07 首版不提供归档界面，S11 不应实现 S07 的归档界面；删除或降为官方 API 级测试。
- [minor] S11:102 — 「人为制造 flush 后写失败」不可观察：官方失败只写日志、不写失败标记；应断言任务仍 active、无新回执、无成功提示。
- [minor] S11:5 — 「下游：S13（D09/D10）」方向错：D10 是 S13→全部子件，不是 S11 下游；S11 只有 D09。
- [minor] S11:97 / S01:114 — S11 重抄 S01 的认领验收，同一判定两个 owner；标为联合验收或移除。

## S03（1 blocker / 6 major / 1 minor）

- [blocker] S03:28,59,112,136 — 「官方给出的增删统计」没有按工具调用的官方来源：FileDiff 只有 path/oldText/newText，DiffResultView 只有 diffs，FsDiffMeta 只有 diffs/operation；workspace/changes 是整轮 turn 摘要。Web 是由 diff primitive 从 oldText/newText 现算。
- [major] S03:94 — 「重连或重开后未决审批已取消/不可用」与官方重连语义相反：pending scoped waterfalls 在重连后 replay；只有进程重开/回合结束才 cancelled/unavailable。
- [major] S03:90,94,106 — 审批生命周期自相矛盾，且仍保留本地超时；§5 说无 timeout/无投影/重开不重建，§6 又说重开后仍可处理；本地超时应进 §9。
- [major] S03:76,81,102,117-118,141 — 把 userQuestions 投影当成所有问卷的持久/回看来源；官方投影只 fold timed schema，默认 blocking 不进投影；重连是 pending request 再投递，settled 只读卡由 transcript 记录重建；需开 tool-ask-user mode: timed。
- [major] S03:37,44-49,76,127-142 — 机制清单漏稳定阅读锚点、限时 continued、分设备滚轮/滚动抑制后台轮询；问卷只读回看向导归因错。
- [major] S03:148,150 — §9 越权替 S02/S13 分配键位和主题 owner；S13 拥有 design-contract。
- [major] S03:5,10,14 — 没定义 D04 导出给 S12 的只读/可交互展示接口形状与验收。
- [minor] S03:58 — 把 FileLocation.line 当差异坐标；它只在 GenericCallView.locations，DiffCallView 通常只有 path，DiffResultView 没有 locations。

## S10（1 blocker / 7 major）

- [blocker] S10:22 — 把任务终态直接当「流结束」，没规定 settled 之后再读一次 ring 最终 output；官方 settled 先发、最终 output 后发，settle 会裁到 settledRetainBytes=16384。
- [major] S10:39,71 — 把 claimed 当模型收件事实；claimed 消息在 step 被拒时既不 discarded 也不作为 user/message 出现，step acceptance 也不 commit。
- [major] S10:47 vs :22,:86 — 预算耗尽后停止投递 vs 终态通知永不合并/不能丢；没规定终态豁免预算或预留名额。
- [major] S10:47,87 — 预算只有通知数/间隔/行数，没有 UTF-8 字节或 token 上限；长行/少量超长行可撑爆上下文；官方有 outputLimitBytes 和 ring 262144 字节。
- [major] S10:19 — 初始偏移未定义：从 0 重放 retained head 还是从当前 total 只看新输出；附着时已有输出没有验收。
- [major] S10:42,47,86,106 — 机制清单「任务通知队列 + notified 去重」与正文不对应；notified 不是官方 JobView 字段；停止前标记与「停止监听≠停止任务」冲突；有效期无落点；next/sleep N 无正文。
- [major] S10:5-6,10,66-78 — 与总纲 §4.1/§7 不一致：未引用 D05/D08；D08 的审批/权限发布闸没有验收；通知投递 owner 没按 §4.1 写清（插件拥有监听/投递，Rust 只显示）。
- [major] S10:14,41 — 「不伪造用户回合」「来源不是 user」与官方 followup/inject 冲突：两者都投递 identified user-role 消息，source.kind=monitor 只是元数据；应写 role=user、source=monitor，不能把 source.kind 当授权边界。

## S01（2 blocker / 5 major / 1 minor）

- [blocker] S01:142 — 「认领/后端行必须排在 storage-domain 和 dsh-schedule 之前」不能由公开 patch 表达，也不是实际激活条件；真正缺的是 backend service key 的 provide（ctx.provide(storageBackendServiceKey('nano-schedule-json'), backend)）；row order 无加载语义。
- [blocker] S01:35 — §2.1 没定义锁的原子获取原语和 fencing：无 O_EXCL/wx，30 秒过期可回收活锁，pid-nonce 不在账本记录里，「被回收者不得提交」没有原子校验。
- [major] S01:26 — 第 5 步要求创建/恢复后确认官方可写锁，但新建 blank 会话在首次 materializing write 前没有官方锁；只能靠 mount ledger 作为前置互斥。
- [major] S01:24 — 认领失败早于握手且要求直接退出，但 §3 没有握手前的结构化错误帧；Rust 无法区分被占用/启动崩溃/清理失败。
- [major] S01:75 — D07/S09 要求的 PTY 每附件输入序列 + ack 没有在 S01 协议表/正文定义。
- [major] S01:25 — 只规定 schedule domain 路由，没有规定全局 KV root 和官方 session persistence root；profile 不覆盖时 workspace/projection cache 直接写 $DSH_HOME/storages，与官方 dsh 无锁并发。
- [major] README:133 — D06 把 schedule.lock 列进 owner S01，但 README:83/S11:66 说是 S11 的；S01 启动序列没提租约；S11 损坏锁语义矛盾。
- [minor] S01:117 — 验收越权写 S02/S03/S07/S09/S10/S11 的跨会话隔离和 PTY；机制清单缺 generation/生命周期状态机/四事实关停。

## README（1 blocker / 6 major / 2 minor）

- [blocker] README/S11:51-52 — `nano-schedule-json` 挂载方案按公开 API 写不完整：storage-json 注册 json 后还 ctx.provide('storage.backend.json', backend)，storage-domain 用 ctx.inject(['storage.backend.<name>']) 等这个 key；只 register 时 domain form 永远 pending，schedule/workspace/projection cache 一起等；--patch 覆盖整段 config，只写 routes 会丢必填 backend: json；row order 无加载语义。
- [major] README:133 — D06 把 schedule 独立后端与 schedule.lock 租约写成 S01 提供物，owner/方向错；README:83 说 lock 是 S11 的，S01:143/S11:120 也说由 S11 定义。
- [major] README:181-190 — §10 机制清单与正文不一一对应：人工命令新语义、挂载账本、schedule.lock、nano-schedule-json 路由都没行；§10 的「稳定锚点」「单列问卷」在总纲正文没有对应。
- [major] README:31 — 「`!` 走官方 Shell 的用户权限路径」不是官方现有能力：一个 composition 只 mount 一个 provider，bash-sandbox 是 deny-only；实际入口是 ShellExecRequest.sandboxPolicy + ctx.sandboxPolicy.resolve({mode:'danger-full-access'})；S08:95 没写这条。
- [major] S11:69 — 损坏 schedule.lock 的处理与 S11:99/README:83/S01:37 矛盾。
- [major] S13:23-26 — 支持矩阵把未测组合写成「已测/本份验证」：0.2.1-rc.2、CJK、深浅主题/60-100 列；同文件 124 行说验收未执行。
- [minor] README:31 — 「`!!` 是 bash 历史展开」在 fresh `bash -c` 下不成立；本机实测 `bash -c 'echo !!'` 原样输出；无 `!!` 特殊模式是对的，但子 spec 文案会误导。
- [minor] README:128,136 — 依赖台账漏 S01→S13。

## 处置状态（2026-10-21 全改）

已按 12 份发现改完：

- S01：`ctx.provide(storageBackendServiceKey)` + 完整 storage-domain config；挂载锁补 `wx`/token/提交前重读/fencing；blank 会话认领为前置互斥；补 `nano.startupError` 和 PTY input/ack/state；Nano 自有 sessions/storages root；验收标联合。
- S11：后端 provide/完整 patch/探针验证；租约损坏改为「活锁不删、死 pid 回收、损坏锁等 30 秒」；修 D10 方向、归档验收 owner、重复 S01 验收。
- S09：删除跨窗口崩溃恢复/离线列表；关闭状态改官方 phase；ack refusal 与 Host 错误路径修正；scrollback 不伪造缺口；补 retain/创建/回收机制。
- S12：删除 outputFile/local_agent/notified/2s/120s；改回 childId/subagentId/activity/subagent/end/subagentTiming；补 exact-parent-live、fork seed 例外、subagents/prompt、审批 never、容量 8。
- S03：官方统计改为本地从 oldText/newText 计算并标注；审批重连 replay；userQuestions 只覆盖 timed，blocking 走 pending/transcript；补 D04 接口；键位/主题归 S13 design-contract。
- S10：settled 后读最终 output；claimed 不等于收件；终态豁免预算；补字节/token 上限和初始偏移；通知写 role=user/source=monitor；补 D05/D08。
- S08：人类停止恰好一个通知渠道；移除 notified；输出改官方 ring；移除卡住检测；`!` 用 danger-full-access + inject 真实时机；自动转后台同一个 job；补 D08 负向闸。
- S05：watcher 改官方因果链；未知 `/name` 不报错；完整性只认 snapshot().complete；技能卡片归 S03；补预览机制和大小上限。
- S07：恢复改 resolveAgent/inspect；create 不同 ID 硬失败；冷会话改名拒绝；草稿/锚点归 S02/S03；blank 会话认领为前置互斥；contextWindow 源写清；补 D02/D06。
- S02：命令/技能 owner 改 S04/S05；bash-mode 标为本地标记；用户消息主键 seq；停止清空竞态可观察；编辑带附件禁止；turn 终态改 turn/end reason。
- S13：design-contract 唯一 owner；阻断缺陷补全；跨会话隔离用唯一标记；证据绑定产物哈希 + 合成 home/脱敏；版本矩阵 owner 写清；支持矩阵不再写「已测」。
- README：`!` trusted bypass 写清；D06 owner 修正；D09 覆盖 S01；§10 补新机制；未知版本离线口径统一。

S04/S06 的重审 blocker 也已按官方契约修正（权限走 `/permission`/重读未知态；图片 wire base64、文件回执分开、模型图片支持、子代理通用文件拒绝）。

仍未做：真实宿主集成验证、复跑审核、提交/PR/issue。
