# [2026-10-20-nano-v1/S11] 调度

- 组标识：`2026-10-20-nano-v1`；角色：子件 S11，单交付为官方定时任务 CRUD、投递与补投展示。
- 状态：草案，未批准实施；Issue：待规格合入后创建。
- 总纲：[共享约定与依据](README.md)。上游：S02 入口，S01 会话认领与启动顺序。下游：S13（D09）；D10 是 S13→全部子件的发布条件，不是本份下游。
- 合入前提：S01 的会话认领与 `nano-schedule-json` 路由可用；S02 可运行（D02）；D05 不涉及本份。本份用真实定时任务验证创建、补投、删除和重复风险展示。发布由 S13 承担。

## 1. 边界

拥有 `crates/nano-schedule/`、`bridge/schedule/`。提供定时任务的创建、列表、修改、删除，以及投递历史和补投事实的展示。

不实现自己的计时器、不实现跨窗口任务争抢、不解析或改写官方存储文件。计时、持久化和投递由官方 schedule 服务拥有；S11 只做表单、调用和结果呈现。

`bridge/schedule/` 在宿主进程内运行，负责两件事：把 Rust 的 `nano.schedule.*` 请求转成官方 `ctx.schedule` 调用；在宿主启动时注册 `nano-schedule-json` 存储后端并设置 `schedule` domain 的路由。它不启动第二套调度器，也不直接读写 `schedule.json`。

## 2. 任务模型

任务绑定创建它的 Agent 会话，包含：

- 必填标题（去空白后非空，最多 120 字符）、必填指令文本。
- 恰好一个时间选择器：一次性延迟、绝对时间、固定间隔、每日、每周或 cron。
- 每日/每周/cron 使用显式 IANA 时区；间隔至少 60 秒；cron 为五字段 Vixie 表达式，不支持 `L`/`W`/`#`、月份英文名或 `@daily` 宏。
- 任务状态：活跃/已结束；已结束任务仍可查看，直到显式删除。
- 投递历史：最近回执与有界保存记录；回执确认的是「进入 inbox」，不是「模型已执行」。

界面不自行推导标题，不把指令首句当标题；缺少标题或标题非法时按官方错误显示。

## 3. 创建、修改与删除

- 创建走官方进程内接口：`bridge/schedule/` 调用 `ctx.schedule.create(sessionId, request, signal)`，不是官方 Remote 的 `create`（官方 Remote 只有 `catalog`/`list`/`history`/`update`/`delete`）。成功后刷新列表，不乐观插入一个本地任务冒充官方已创建。
- 修改使用「读取完整记录 → 用户编辑 → 带预期记录提交」的比较并交换；冲突返回 `schedule_conflict` 时提示刷新后重试，不能覆盖别人的修改。
- 修改可以更换时间类型；省略字段保留原值。`after_seconds` 只能用于创建，不能作为新的 timing change；已经存在的 `after` 任务仍可改标题/指令，也可以原地改成 `at`/`every`/`daily`/`weekly`/`cron`，保留 id 和历史。
- 删除停止未来投递，并删除任务行和投递历史；会话本身和已经排队的消息不受影响。删除后同一 ID 的查询返回 `schedule_not_found`。
- 关闭会话归档时如有活跃任务，官方会拒绝归档；若用户选择停止任务，则所有活跃任务被删除，取消归档不会恢复。S11 必须显示这一后果。
- 首版不提供暂停/恢复；官方没有暂停 API，界面不画一个看起来能暂停的开关。停止任务只能删除（或由官方归档的 session-stop 删除全部活跃任务）；`update` 不能把 active 改成 inactive。

## 4. 投递、补投与重复风险

- 周期任务在关闭/重启期间只补最近一次遗漏；一次性任务过期后仍会尝试投递。
- 投递是把提醒作为 follow-up 消息放入原会话 inbox，并恢复冷会话；它必然会唤醒一次模型请求，并按当时权限策略执行。创建和投递界面都要说明会产生模型调用和费用；没有活人确认时，需要审批的动作仍会被官方策略拒绝或失败关闭。
- 投递成功以官方会话 flush 回执为准；回执写入任务记录是另一次持久化。两者之间崩溃或写失败可能导致同一条提醒再次投递；官方不保证严格一次。
- 界面必须展示「可能重复投递」的事实和最近回执时间，不能宣称「绝不重复」。
- 投递历史读取有界；`earlierRecordsUnavailable` 表示记录没有保存，`earlierRecordsPruned` 只表示被确认裁剪，两者不能合并成「更早记录不可用」。保留窗口是官方 `deliveryHistoryDays`（默认 30 天）加 `deliveryHistoryRecords`（默认 200 条）。
- 时钟回拨、夏令时空档和重叠按官方规则处理：缺失本地时间跳过，重叠取较早瞬间，每日/每周/cron 共享同一批投递时仍各自推进。
- 同一根会话只有一个宿主（S01 认领保证），因此只有一个 timer、一张任务表；不同根会话各写自己的 `runtime-data/<rootSessionId>/storage`，互不可见。窗口之间不通过进程内 `schedule/changed` 互相同步；`schedule/changed` 只刷新本宿主。

## 5. 数据隔离与所有权

调度表按根会话隔离，但不需要把整个 storage root 搬走。做法是只给 `schedule` 这一个 domain 换后端：

1. `bridge/schedule/` 在宿主启动时用公开的 `JsonStorageBackend` 注册第二个 JSON 后端 `nano-schedule-json`，root = `<Nano 数据根>/runtime-data/<rootSessionId>/storage`；除了 `ctx.storage.backend.register(...)`，还必须 `ctx.provide(storageBackendServiceKey('nano-schedule-json'), backend)`。`storage-domain` 是按 service key 注入后端的，只 register 不 provide 时 domain 永远 pending。
2. 通过每次启动的 `--patch` **完整替换** `storage-domain` 的 config：`backend: json` + `routes: { schedule: nano-schedule-json }`。不能只写 `routes`，否则必填的 `backend` 丢失；也不能靠 patch 的行序，行序没有加载语义，真正的门是 service key。
3. 启动时用**独立探针 domain**验证路由，而不是读写真实 schedule 任务：`bridge/schedule/` 定义一个只含单表的 `nano_probe` domain，`routes` 同时指向 `nano-schedule-json`，写一条探针记录、确认只落在 `<root>/nano_probe.json`、再删除。探针 domain 在 `dsh-schedule` open 之前完成，避免 `already-open` 和污染任务表。
4. 等待 `ctx.storageDomain` 注入并设启动超时：如果 `storage-domain` 因缺 service key 永不激活，超时后报 `unsupported` 并退出，不能挂起或静默不挂 schedule。
5. 所有 `storage-json` domain 都走 per-root root；workspace、session_projcache 等不再共享全局 root。workspace 是窗口本地视图，会话列表来自共享 `sessions/` 和 session-query。`nano-schedule-json` 是调度专用后端，作为 schedule 的隔离层保留。

所有权：

- S01 的会话挂载账本保证同一根会话只有一个宿主。因此每个根会话只有一个 schedule timer、一张任务表；第二个窗口在认领处被拒绝，调度不会提前挂载。
- 不同根会话（不同 worktree/会话）各有自己的宿主和 `runtime-data/<rootSessionId>/storage`，任务表互不可见，不存在跨会话任务串表。
- 切换主会话必须按 S07/S01 停止旧宿主、释放认领、认领新会话、启动新宿主；不能在线换 root 或把两张任务表拼在一起。
- 关闭窗口或宿主退出后不执行任务；重新打开同一会话时读取同一张表，并展示补投结果。
- S11 不直接读写 `schedule.json` 或会话日志；损坏存储按官方错误处理，不创建空表冒充成功。会话删除后留下的 `runtime-data/<rootSessionId>/storage` 是孤儿数据，首版不自动清理，也不把它并到别的会话。

### 5.2 调度租约锁

`nano-schedule-json` 保证每个根会话有自己的表，但还需要保证同一时刻只有一个宿主在驱动这张表。S11 在 schedule root 旁加一把租约锁，作为 S01 mount ledger 之后的第二道闸：

- 文件：`<数据根>/runtime-data/<rootSessionId>/schedule.lock`；目录 0700、文件 0600；内容 `{ version, sessionId, hostPid, acquiredAt }`。
- 获取：`wx` 原子创建；已存在时读取。同一个 `hostPid` 幂等；另一个存活 `hostPid` → 拒绝启动并显示「调度由 pid X 持有」，**绝不 unlink 活锁**；`hostPid` 已死 → unlink 后重试一次；内容损坏/不可读 → 看锁文件 mtime：不足 30 秒先拒绝并报「锁状态未知」，超过 30 秒才按 stale 回收并重试一次。不在进程内 sleep 等待，也不把损坏锁当空锁。
- 存活判定只有 `process.kill(hostPid, 0)`；`EPERM` 算存活。不写心跳。
- 释放：只删除内容仍属于自己的锁；不能删别人的锁。正常退出在宿主清理完成后释放；崩溃由 pid 存活判定回收。
- 宿主在挂载 `dsh-schedule` 之前必须同时持有 S01 的根会话认领和这把租约；任一失败就退出，不挂 schedule、不启动 timer。首版不在运行中动态抢锁；接管靠 S07 的受控重启。
- 租约只服务同机；跨机器并发不在首版范围。mount ledger 保证一个根会话一个宿主，租约保证一个宿主一个 scheduler；两者都失败才可能双 timer。

## 6. 失败与不变量

1. 任务、状态、回执和历史唯一来源是官方 schedule 服务；Rust 不维护第二份任务表。
2. 创建/修改/删除没有官方确认不显示成功；冲突不覆盖。
3. 回执不等于模型执行；投递不等于任务「完成」。投递必然唤醒一次模型请求并可能产生费用。
4. 补投只补官方规则允许的次数，不自行循环补跑。
5. 删除是不可逆的官方动作；确认文案必须说明会删除任务与历史。`update` 不能改 status，停止只有删除。
6. 无暂停 API 时不画暂停开关。
7. 同一根会话只有一个宿主和一个 timer；第二个窗口在 S01 认领处被拒绝，不能靠官方 writer-held 事后兜底。
8. 宿主必须同时持有 S01 根会话认领和 `schedule.lock` 才能挂 `dsh-schedule`；缺一退出，不启动 timer。
9. 所有 `storage-json` domain 走 per-root root；workspace/projection cache 是窗口本地视图，不跨窗口共享；会话列表来自共享 `sessions/`。两个根会话的调度列表互不可见。
10. 后端/路由缺失或不匹配时，不挂 schedule 并报 `unsupported`；不能回退到全局表，也不能假装没有任务。

## 7. 验收

- [ ] 六种时间选择器都能创建；标题/指令/时区/cron 非法时显示官方错误码与可读原因。
- [ ] 列表、已结束任务、最近回执和投递历史与官方记录一致；`earlierRecordsUnavailable` 与 `earlierRecordsPruned` 分开显示。
- [ ] 关闭期间错过多次周期任务，重开后只投递最近一次；一次性过期仍尝试投递。
- [ ] 投递回执显示为「已进入 inbox」，不显示成「模型已执行」或「任务完成」；投递前提示会产生模型调用和费用。
- [ ] 冲突修改返回 `schedule_conflict` 并保留用户编辑；刷新后可重试，不覆盖他人修改。
- [ ] 删除停止未来投递并移除任务与历史；已排队消息不受影响；`update` 不能停止任务。
- [ ] 归档 API 级行为（不是产品界面）：有活跃任务的会话被官方归档 API 拒绝；调用官方的 session-stop 后任务被删除且取消归档不恢复。首版没有归档界面，S11 不为它造界面。
- [ ] 两个宿主同时认领同一根会话，第二个在挂 schedule 之前被拒绝；失败时调度未提前执行。
- [ ] 两个宿主同时抢同一根会话的 `schedule.lock`，只有一个拿到；另一个不挂 schedule、不启动 timer。
- [ ] 持有者退出/崩溃后，新宿主能回收租约并接管；活着的持有者锁绝不删除；内容损坏时看 mtime，不足 30 秒报「锁状态未知」，超过 30 秒才按 stale 回收；不把损坏锁当空锁。
- [ ] 两个不同根会话的调度表互相不可见；同一根会话重启后读到同一张表；workspace/projection cache 按根会话隔离，会话列表来自共享 `sessions/`。
- [ ] 后端未 `provide` service key、`ctx.storageDomain` 启动超时、`nano_probe` 写不到 per-session root、或 `routes.schedule` 实际仍指全局 `json` 时，宿主不挂 schedule 并报 `unsupported`；不落回全局表。
- [ ] 读路径明确：活跃+已结束用 `catalog`，只列活跃用 `list`；`history` 用 limit 1-100、newest-first、`before` 排他游标；非法参数返回 `invalid_rule`。
- [ ] 人为制造 flush 后写失败时，任务仍 active、没有新回执、界面不显示成功；「可能重复投递」是常驻静态提示，不靠故障与否切换文案。
- [ ] 本份的真实补投/重复风险证据与失败夹具随交付提供；未测的 DST 边界明确标注。

## 8. 机制清单

| 机制 | 解决什么 / 没有会怎样 | 确认与代价 |
| --- | --- | --- |
| 复用官方调度器 | 计时、持久化、补投都由后端负责；没有则要另写一套 Cron | 用户已确认「抄官方的」；无暂停、可能重复、投递必唤醒 |
| 调度专用 JSON 后端 + 路由 | 按根会话隔离 storage-json，避免跨宿主 last-wins；schedule 另有专用后端 | 作者建议；用到公开 `JsonStorageBackend`、`backend.register`、`storageBackendServiceKey`、`routes` 和 `--patch` |
| 会话认领先于调度 | blank 会话没有官方跨进程锁；没有则两个宿主各跑一个 timer | 作者建议（参照 dsh-TUI）；同机、pid 复用、需维护锁文件 |
| 调度租约锁 | mount ledger 之外的第二道闸；没有则 mount ledger 出 bug 时双 timer | 作者建议（参照 Claude Code `scheduled_tasks.lock`）；同机、pid 复用、需维护锁文件 |
| 带预期记录的修改 | 防止覆盖并发修改；没有则静默丢别人的编辑 | 官方契约；冲突需重试 |
| 删除即停止 | 官方没有暂停；没有则 UI 画假开关 | 官方限制；删除不可逆 |
| 重复风险可见 | 用户能预期补投/重投；没有则误以为严格一次 | 官方语义；需在界面长期标注 |
| 跨会话隔离 | 每个根会话只有自己的任务表；没有则串会话内容 | 用户硬要求；需在 S01/S11 都有验收 |

## 9. 自定细节

- `nano-schedule-json` 后端名、root `runtime-data/<rootSessionId>/storage`、完整 storage-domain config 和探针步骤由 S11 定义；启动顺序归 S01，S01 只保证宿主先认领会话再挂 schedule。
- 调度表单的布局、时区默认值和 cron 帮助文案由 S11 定；布局槽位、最终键位和主题由 S13 的 design-contract 冻结。
- `schedule.lock` 文件名、`{ version, sessionId, hostPid, acquiredAt }` 字段、O_EXCL、无心跳、接管靠重启，均为作者设计；参照 Claude Code 的 `scheduled_tasks.lock`。
- 调度表单的布局、时区默认值和 cron 帮助文案由 S11 定，最终键位由 S13 的 design-contract 冻结。
- 默认时区取用户本机 IANA 时区，但必须显式显示并允许修改；不静默使用系统缩写。
- 列表默认显示活跃任务，已结束任务折叠在「已结束」分组；保留条数由官方历史限额决定。
- 首版不提供任务模板、批量创建或跨会话复制。
- 不显示无法从官方记录证明的「下次执行倒计时」精确秒数；只显示官方目标时间。
- 会话删除后遗留的 `runtime-data/<rootSessionId>/storage` 不自动清理；需要清理时由用户显式删除整个会话数据目录，不能并到别的会话。

## 10. 审核修订记录

待本稿逐份过闸；上述验收均未执行。

2026-10-21 原型（独立 Node 脚本，非完整宿主）：官方 `JsonStorageBackend` 的两个 root 各写同一个 `schedule` unit，记录和文件互不可见；O_EXCL 租约两进程只有一个 owner，owner 退出后新进程可接管。完整 `storage-domain.routes.schedule` + `--patch` 组合和真实 `dsh-schedule` 投递仍待 S11 宿主验收。
