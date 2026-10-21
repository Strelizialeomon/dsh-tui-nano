# [2026-10-20-nano-v1/S08] Shell 与任务

- 组标识：`2026-10-20-nano-v1`；角色：子件 S08，单交付为人工命令、后台任务观察与停止。
- 状态：已批准实施；执行单 #11。
- 总纲：[共享约定与依据](README.md)。上游：S02 操作入口，S01 执行服务，D08 发布闸（S03/S04 → S08/S10）。下游：S10（D05）、S13（D09）。
- 合入前提：S01 官方执行与进程所有权可用；本份用真实前台/后台命令验证输出、停止和通知。发布由 S13 承担。

## 1. 边界

拥有 `crates/nano-jobs/`、`bridge/shell/`、`bridge/jobs/`。提供 `!` 人工命令、任务列表、输出跟随、停止操作，以及前台命令超时转为后台的展示。

不实现 PTY 或交互终端（S09）、不实现 Monitor 监听与 Agent 投递（S10）、不实现命令日志伪终端、不替代官方 sandbox 策略。Agent 的 Bash 工具执行仍由官方工具链拥有；S08 只提供人类侧入口和观察。

## 2. 人工命令

按 Claude Code 的实现：`!command` 是用户命令，不是 Agent 工具，也不是 job。

| 输入 | 执行 | 输出去向 |
| --- | --- | --- |
| `!command` | 直接调 `ctx.shell`，per-call 传 `ctx.sandboxPolicy.resolve({ mode: 'danger-full-access' })` 生成的完整 policy；不改 mount、不改 profile 默认值 | 本窗口显示；同时作为**合成 user 上下文**注入会话，不立即唤醒模型；下一条消息能看到 |
| `!!` | 没有特殊模式 | 原样交给 shell 的普通文本，不触发任何入模路径 |

规则：

1. `!` 是人工命令，不是交互式终端；不分配 PTY、不保持 shell 状态。每条命令使用新 shell，工作目录为当前会话目录，`cd` 不跨命令保持。
2. `!` 以用户权限运行，和 S09 用户终端同级；操作系统/容器限制仍生效。它不经过 Agent 的 sandbox/approval，也不因为这种权限自动成为 Agent 的授权。界面在首次使用时明确提示这一点。
3. `!` **不注册成官方 job**：官方 job 表会被模型的 `job_list`/`job_output` 看见，人工命令不能进入那张表。桥接直接调用官方 shell seam，拿输出文件/退出码，不分配 job ID。
4. `!` 的输出以 `<bash-input>`、`<bash-stdout>`、`<bash-stderr>` 形式的合成 user 消息注入（走官方 `inject` 语义，不唤醒驱动）；模型把它当上下文，不当作用户批准。来源标记为 `bash-mode`，审批/权限服务不能把它当授权。
5. 非零退出码是结果，不是基础设施错误；显示真实退出码或信号。缺失退出码显示未知，不补零。
6. 输出被截断时显示官方截断提示和 spill 文件路径（如官方提供）；不把尾部当成完整输出。
7. 人工命令不能借用 Monitor、子代理或后台任务扩大自己的权限；这些入口也不能借人工命令绕过审批。`!` 的输出不是授权，Agent 不能据此跳过审批。
8. 用户需要模型立即处理输出时，发送一条普通消息即可，不需要 `!!`；`!!` 只是原样交给 shell 的普通文本，不是历史展开，也不触发入模路径。

## 3. 任务列表与输出

任务列表显示当前会话可见的任务，包括本会话自己的后台任务和官方允许看到的无主任务。

- 任务身份使用官方稳定 ID（`<kind>-<short-uuid>`，16 位小写 hex），不自行生成第二套编号。
- 每条任务显示种类、一行标签、状态（运行中/停止中/完成/被终止/失败）、进度摘要和所属调用（如有）。所属调用只能从官方 `job/registered` 事件读取，`JobView` 本身没有该字段。
- 输出按绝对字节偏移跟随，使用官方 `readAt`/`job.follow` 的非消费读取；不改变模型 `job_output` 的游标，也不消耗任务输出。
- 官方输出是内存 ring：`retainBytes`（默认 262144）、`settledRetainBytes`（默认 16384）、`pumpPollMs`（默认 150）。settled 后 ring 被裁，旧字节可能不可读；这是 lossy read，不是错误。只有 pull source 保留 spill 时才显示 `spillPaths`；不另建文件尾读、8MB 内存或 size watchdog。
- 输出帧可以合并；慢消费者遇到缓存淘汰或断流时显示 `gapBefore` 缺口，明确「中间有未显示内容」，不从缺口后假装连续。
- 状态以官方投影为准；运行中的最后一行不是完成事实。任务完成后保留当前 ring 和终态，不承诺旧字节仍可读。
- 任务由宿主进程拥有；关闭窗口或宿主退出时，本进程内的任务随宿主结束。首版不承诺跨重启恢复后台进程。

## 4. 停止

- 停止调用官方 `job.kill`，原因记为「cancelled by the user」；结果只回答「已请求停止」或「已经结束」，不能显示成「进程已确认退出」。
- 停止请求发出后，任务进入「停止中」；收到官方终态后才显示「已终止」「完成」或「失败」。自然完成是合法终态，不能只显示「已终止/失败」。
- 人类停止的模型侧通知走「恰好一个渠道」：如果模型正在等待这个前台命令，官方 settlement 记为 `awaited`，不另发完成通知，前台结果带 `[stopped: reason]`；否则后台任务的完成通知仍 due，带人工停止原因。S08 不能吞掉，也不能重复发。
- 停止一个已经被其他窗口结束的任务返回「已经结束」，不报成错误，也不重复终止；任务已移除则返回 `job/not-found`，不猜成「已经结束」。
- 看不到的任务返回 `job/not-found`；不猜测 ID，不越权停止子会话或其他会话的任务。
- 前台命令在启动时就注册成 job；超时后是「继续跑的还是同一个 job」，不是「新任务 ID」。promotion 是 best-effort：`promoteOnTimeout: false`、缺 registry、容量拒绝都会退回 deadline kill，验收必须分别覆盖。

## 5. 失败与不变量

1. 任务状态、输出字节和停止结果唯一来源是官方任务服务；Rust 不维护第二份任务表。
2. 观察不消费；展示层的读取、合并、丢弃不能影响模型游标或完成通知。
3. 没有收到终态时不能显示任务已完成；停止请求不等于进程已退出。
4. 输出缺口、截断、spill、ring 裁剪和失败都必须可见，不能静默拼成连续文本；settled 后的裁剪是 lossy read，旧字节不可读要标出来。
5. `!` 的输出以合成 user 上下文进入会话，但不立即唤醒模型；它不能被当成用户批准，也不能让 Agent 借它跳过审批。没有 `!!` 特殊模式。
6. `!` 是用户权限命令，不是 Agent 工具；它不能扩大 Agent 的授权，Agent/Monitor 也不能借它绕过审批。
7. 宿主退出时本进程任务结束；不宣称 restart 后原进程仍在运行。
8. 人类停止的模型侧通知恰好一个渠道：awaited 前台工具结果或后台完成通知，二者不重复、不吞掉；不依赖官方不存在的 `notified` 字段。

## 6. 验收

- [x] `!` 命令以用户权限执行，输出本窗口可见并以 `<bash-input>/<bash-stdout>/<bash-stderr>` 合成 user 上下文注入；不立即触发模型请求，下一条消息能看到；`!` 不分配 job ID、不进入 `job_list`。
- [x] `!!` 只是原样交给 shell 的普通文本；`bash -c` 非交互、没有 history expansion，也不触发任何特殊「入模」路径。
- [x] 非零退出、被信号终止、缺失退出码分别显示正确；输出被 ring 裁剪时标「已裁剪」，有 spill 时显示路径。
- [ ] 前台命令启动时就是 job；超时后仍是同一个 job ID、同一份输出 ring；promotion 失败时按 deadline kill 处理并分别验收。
- [x] 后台任务列表、状态、进度与官方投影一致；所属调用按 `job/registered` 关联，不从 `JobView` 找。
- [x] 打开、滚动、重开任务详情不改变模型 `job_output` 的读取位置和完成通知。
- [x] 停止返回「已请求」或「已经结束」，终态到达后才显示终止；自然完成显示 completed；人类停止只走一个通知渠道（awaited 工具结果或后台通知）。
- [ ] `!` 输出不能作为审批授权：负向夹具证明 Agent 不能凭 `!` 输出跳过审批或改变沙箱（D08 发布闸）。
- [x] 看不到的任务、重复停止、其他会话任务分别按官方错误处理，不越权。
- [x] 输出缺口/淘汰显示 `gapBefore`；慢消费者不阻塞任务执行，也不无限缓存。
- [x] 关闭窗口或宿主退出后本进程任务结束；不显示成跨重启仍在运行。
- [x] 本份的真实前台/后台/停止证据与失败夹具随交付提供；未测的逃逸进程范围明确标注。

## 7. 机制清单

| 机制 | 解决什么 / 没有会怎样 | 确认与代价 |
| --- | --- | --- |
| `!` 用户权限 + 合成上下文 | 用户能直接跑命令，又不把它冒充成 Agent 授权；没有则要么审批死路，要么授权旁路 | 按 Claude Code 实现；用户权限需明确提示 |
| `!` 不注册 job | 防止 `job_list` 把人工输出带进模型；没有则 `!` 边界失效 | 官方 job 表契约；需独立 shell 路径 |
| 官方 jobs ring + 非消费 readAt | 输出唯一来源是官方任务服务；没有则另建文件/第二份输出 | 官方契约；settled 裁剪是 lossy |
| 自动转后台（同一个 job） | 前台超时命令继续运行并可停止；没有则长命令被静默杀掉 | 官方 `promoteOnTimeout`；best-effort，需覆盖失败 |
| 恰好一个停止通知渠道 | awaited 工具结果或后台通知，二者不重复；没有则吞掉或重复 | 官方 settlement；不依赖 `notified` |
| 停止只表示请求 | 不把「已发停止」冒充「已退出」；没有则误判任务结束 | 官方语义；需等待终态 |
| 任务列表复用官方 ID/状态 | 不维护第二套任务表；没有则状态漂移 | 官方契约；宿主退出即结束 |
| D08 负向闸 | `!` 输出不能作为审批授权；没有则人工命令变旁路 | 发布前置；需负向夹具 |

## 8. 自定细节

- `!` 直接调 `ctx.shell`，在 `ShellExecRequest` 显式传 `sandboxPolicy: ctx.sandboxPolicy.resolve({ mode: 'danger-full-access' })`；不改 mount、不改 profile 默认值。首次使用必须提示「这是用户权限命令，不是 Agent 授权」。
- `!` 的输出用官方 `inject` 语义注入，来源标记 `bash-mode`；不唤醒驱动。运行中的 Agent 在最近的 step boundary 取走，空闲 Agent 要等下一次 follow-up/steering；不是「下一条消息才能看到」。
- `!` 不分配 job ID、不进入 `job_list`；桥接直接调用官方 shell seam，不走 `dsh-tool-bash` 的 job 注册。
- `!!` 不是本项目的特殊模式，也不是 bash 历史展开（`bash -c` 非交互）；它只是原样交给 shell 的普通文本，不触发任何入模路径。
- `!` 的输入解析在行首触发，具体转义和取消方式由 S08 定，最终键位由 S13 的 design-contract 冻结。
- 任务列表默认只显示运行中与最近结束的任务；历史任务的具体保留条数由实测定。
- 输出跟随的合并窗口和渲染尾部上限由本份实现时按实测定，不在规格先写死；官方 ring 的 `retainBytes`/`settledRetainBytes`/`pumpPollMs` 是唯一输出预算来源。
- 首版不提供任务输出导出；完整输出以官方 ring/spill 为准，不另建文件。

## 9. 审核修订记录

实施 PR #20 重审（规格符合性 + 对抗找 bug，2026-10-21）。完整发现见 PR 评论 6318238191 / 6318339357；下表记录处置。

| # | 严重度 | 发现 | 处置 |
| --- | --- | --- | --- |
| 1 | blocker | `!` 输出窗口取头部 2,000 字符，官方截断/spill 提示被截掉 | 改为尾部渲染（4,000 字符）并显式标「显示已截断」；官方提示在尾部保留 |
| 2 | blocker | 任务详情取 tail buffer 的头部 200,000 字符，最新输出被静默丢掉 | 改为尾部渲染（200,000 字符）并显式标截断；与 §8「渲染尾部上限」一致 |
| 3 | major | 前台超时转后台（promotion）成功路径未执行 | 真实 TUI 会话补齐：tool result 出现同一 job id 的 `moved to background job`，面板/详情/停止均覆盖；三条失败路径仍未执行（见下） |
| 4 | major | 真实终端证据没覆盖 jobs UI 本身 | 真实 TUI 会话打开任务面板、详情、跟随 `s08-promo`、k 停止并看到官方 `被终止` |
| 5 | major | 宿主退出后面板仍可能显示「运行中」 | `TransportClosed`/`Fatal`/`ProtocolFailure`/`StartupError` 清空任务投影，运行中的本地 `!` 记录标未知；面板显示宿主已断开 |
| 6 | major | 每请求一线程让所有请求失去写出顺序 | 普通请求恢复串行写入/等待；只有带 deadline 的 `nano.shell.exec` 走独立并发 lane |
| 7 | major | 所有 registry 异常都被映射成 `job/not-found` | 只把官方 `unknown job`/foreign 映射 not-found；producer 取消抛错等保持 `jsonrpc/internal-error` |
| 8 | major | follow 在飞时到达的输出事件被静默丢弃 | `follow_dirty` + 读完按 `total` 补读；运行中与终态都补 |
| 9 | major | 订阅重建后不刷新 jobs，gap 内事件丢失 | `Ready`/`Resubscribe` 成功后重读 jobs snapshot；jobs 事件 live-only 的事实写入注释 |
| 10 | major | 任务列表超过一屏时选中项不可见、前面任务不可达 | 列表按 selected 计算 offset，Up/Down 滚动视口；tail+scroll 只用于详情 |
| 11 | minor | D08 负向闸未执行 | 保持未勾；S08 夹具只证明 source 为 `bash-mode` 且不触碰 approval，真实审批负向夹具归 S03/S04 发布闸 |
| 12 | minor | 证据声称真实宿主验证 danger-full-access，但测试没断言 | 真实宿主 E2E 增加 `result.policy.mode === 'danger-full-access'` 断言 |
| 13 | minor | 官方事件不可解析时静默、`last_error` 是死代码 | 面板/状态行显示 `jobs.last_error`；解析失败不再静默 |
| 14 | minor | 详情 ring 总量/最早保留字节陈旧 | `jobs.output` 事件与 follow 都更新投影 `output.total` |
| 15 | minor | `attachController('nano-jobs')` 是清单外机制，会改变缺 controller 的组合行为 | 移除；S08 只观察，不改变官方「无 controller 退回 deadline kill」 |
| 16 | minor | not-found 提示用 message 匹配，永远不命中 | `RequestError` 保留 JSON-RPC code，按 1028 判定；Unknown 单独显示「结果未知」 |
| 17 | minor | 75s 等待上限与可配置 executor deadline 脱钩 | 桥在请求上显式 `timeoutMs: 60000`，Rust 75s 等待对应这个 deadline |
| 18 | minor | S02 契约测试被放宽；跨 owner 改动未说明 | 契约测试恢复精确事件集合；跨 owner 改动仅限协议/runtime/app 接入，未实现 S03/S04/S10 |
| 19 | minor | 使用已废弃的 `session.snapshotEvents()` | 移除初始历史索引；job/registered 只走 live `session/event`，桥在握手前已挂载 |
| 20 | minor | 证据声称 progress/200 条有夹具，实际没有；通知计数口径弱 | 补 progress/spill/200 条测试；真实证据改为按 notice message id 去重计数 |
| 21 | minor | `!!` 是否注入的规格歧义 | `nano.shell.exec` 增加 `inject: false`；`!!` 仍执行并显示，但不进入上下文；协议与 README 写明 |
| 22 | minor | 详情滚动无上限，移除后泄漏到列表 | 滚动按渲染行数封顶；详情关闭/任务移除时归零 |

修复后 `cargo test --workspace`、`npm run test:bridge`、`NANO_E2E=1 node --test bridge/test/integration/host.test.mjs` 全绿；真实模型会话与真实 TUI 证据见 `docs/evidence/2026-10-21-s08/`。未再派第二轮重审（用户未点）；promotion 三条失败路径与 D08 负向闸仍未执行，对应验收保持未勾。
