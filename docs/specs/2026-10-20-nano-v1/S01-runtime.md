# [2026-10-20-nano-v1/S01] 专属宿主、版本化通信与退出

- 组标识：`2026-10-20-nano-v1`；角色：子件 S01，单交付为真实、可关停的 Harness 适配边界。
- 状态：已批准实施；执行单 #4。
- 总纲：[共享约定与依据](README.md)。上游：无。下游：D01、D06、D07 的全部消费者。
- 合入前提：本份公开服务无凭据契约测试通过；发布由 S13 承担。

## 1. 边界

拥有 `protocol/`、`bridge/runtime/`、`bridge/profile/`、`crates/nano-runtime/` 及构建根。提供进程、协议、官方服务接入、能力识别、数据分区与关停。不实现聊天、渲染、模型选择业务、Shell UI 或 Monitor 算法。

Rust 不嵌入 Node 引擎。经官方 `dsh --profile dsh-tui-nano` 入口启动一份精确版本的 profile，加载已安装桥接插件和官方包；不用 `dsh web` 开浏览器，不复制上游启动器源码，不使用能力不全的 SDK server。首版不打开 HTTP/WebSocket 监听端口。

官方 Connection 提供无 WebServer 时的 carrier-neutral 能力；Gateway 的公开 `invoke`/`stream`/`wireStream` 可复用。桥接创建显式本地 Peer，端点固定白名单并用生成契约校验，不把任意 endpoint、JavaScript 或文件导入能力暴露给 Rust。审批与问卷是两个独立官方 seam：审批走 `approval/request` waterfall，问卷走 `ctx.userQuestions` waterfall；各自只能注册一个回答者，不能共用一个。是否能在最小 profile 完整组合，需要本份用真实发行包证明。

## 2. 启动与身份

每个窗口新建随机 `runtimeId`。一次宿主启动再有 `generation`，重启后不能接受旧代事件、回执、审批或 PTY 输入。`rootSessionId` 是当前主会话的官方身份，不能由路径或标题推断。

启动顺序：

1. 解析 Node/dsh/桥接包的实际路径与版本、绝对工作目录、数据根、目标会话；不通过用户 Shell 拼接启动参数。新会话的 `rootSessionId` 由 Nano 预生成并原样传给官方 create；create 返回不同 ID 即硬失败。
2. 启动自己的宿主进程，保存精确子进程句柄和拥有范围；不扫描名称后批量杀 `node`/`dsh`。宿主进入独立进程组/session，SIGINT/SIGTERM/SIGHUP 统一转发到 `nano.shutdown`，不能先于桥接清理直接死亡。
3. 宿主在创建/恢复会话之前取得根会话认领（见 §2.1）；认领失败直接退出，不创建会话、不挂 schedule、不启动模型请求。
4. 设置本次 profile 的 roots：`session-persistence-jsonl.root = <数据根>/sessions`（共享会话日志，官方 per-session flock），`storage-json.root = <数据根>/runtime-data/<rootSessionId>/storage`（所有 storage-json domain 按根会话隔离，避免不同宿主并发写同一 unit）；按 S11 的配方 `ctx.provide(storageBackendServiceKey('nano-schedule-json'), backend)` 并完整替换 `storage-domain` 的 config（`backend: json` + `routes.schedule: nano-schedule-json`）。不能只写 routes，也不能靠 patch 行序。官方全局配置/凭据/指令来源保持显式，不改用户文件。
5. 创建或恢复准确主会话。已物化的会话确认官方可写锁；新建 blank 会话在首次 materializing write 前没有官方锁，此时以 §2.1 的认领作为前置互斥，物化后再确认官方锁。失败时不启动该会话的 schedule 或模型请求。会话重用返回了不同 ID、工作目录冲突、writer-held 或 profile 未加载必需服务都不是冷启动成功；报出失败并保留草稿，不能悄悄换会话。
6. 协议握手和 UI 决策通道就绪后，才启动该主会话的调度并接受动作。

### 2.1 会话挂载账本

同一台机器上，两个 Nano 窗口不得同时驱动同一个根会话。官方 session 写锁只在会话物化后生效，blank 会话没有可确认的跨进程锁；因此宿主在创建/恢复会话和挂 schedule 之前，必须先取得 Nano 自己的根会话认领。

- 账本：`<数据根>/session-mounts.json`；锁：`<数据根>/session-mounts.lock`。目录 0700、文件 0600。
- 记录：`{ version, owners: [{ pid, startedAt, sessionIds, token }] }`。`pid` 是被认领的宿主进程 OS pid，不是 Rust 窗口进程；`token` 是 `pid-nonce`，用于 fencing；`startedAt` 只用于诊断。
- 认领：用 `wx` 原子创建锁文件并写入自己的 token；在锁内重新读取账本，清理 pid 已不存在的记录，确认目标根会话没有其他存活 owner，再以临时文件 + rename 原子写入自己的记录；提交前重读锁 token，若已不是自己的 token 则放弃提交，不能覆盖新 holder。最后只删除自己 token 的锁。
- 锁被回收只能因为内容可读且 `pid` 已死；读不到有效 `pid` 的锁要等超过 30 秒仍无有效内容才按 stale 回收。活着的 pid 无论锁多旧都不得回收。
- 存活只有 `process.kill(pid, 0)` 一个判据；`EPERM` 算存活。不写心跳时间戳，避免“活着但没能刷新”被误回收。
- 读失败不等于没人占用：账本不可读、JSON 损坏、版本未知、记录形状不对时拒绝启动并报修复路径，不能降级成空账本。
- 释放：正常退出在宿主清理完成后删除自己的记录和锁；宿主仍存活时不得提前释放。崩溃或强杀由 pid 存活判定回收。
- 同会话第二个窗口：报「被 pid X 占用」，保留草稿，不删除锁、不强接管、不新建替代会话。
- 账本只是同机可见层，不能跨机器，也不能防 pid 复用；pid 复用只会让空闲会话看起来被占用，代价是重启一次。官方 session 写锁仍是最终写者隔离。

同仓库多窗口是正常路径，不建项目排他锁；不同根会话各自由自己的宿主认领。相同根会话的第二个窗口在认领处被拒绝，不能靠官方 writer-held 事后兜底。每个根会话的调度表只存在自己的 `runtime-data/<rootSessionId>/storage`，不跨宿主共享。切换主会话须按 S07 停止旧宿主、释放认领、认领新会话、启动新宿主；不能在线替换认领或 storage 路由。

## 3. 本地通信 v1

使用专属 **fd 3/4**（不可用时才退回 stdin/stdout，并给每帧加 per-generation nonce）上的 UTF-8 NDJSON：每行一个 JSON-RPC 2.0 frame，正文中的换行 JSON 转义。stdout/stderr 仅诊断，不承载协议；第三方插件写 stdout 不能伪造事件。协议不等于官方 SDK 协议；命名空间为 `nano.*`，适配后调用官方服务。

| 类型 | 必需字段与语义 |
| --- | --- |
| 请求 | `jsonrpc`、字符串 `id`、`method`、对象 `params`；id 只做本连接关联 |
| 结果/错误 | 原请求 `id`；二选一 `result` 或 `error`，不能都缺或都有 |
| 事件 | `method: nano.event`；`runtimeId`、`generation`、`subscriptionId`、`sequence`、`kind`、`payload` |
| 流结束 | `nano.streamEnded`；明确正常/取消/失败，不能以 EOF 冒充正常结束 |
| 握手 | `nano.hello` → 协议 major/minor、`runtimeId`、`generation`、官方/桥接版本、实际 cwd、rootSessionId、能力与容量 |
| 启动失败 | `nano.startupError` 必须在 `nano.hello` 之前可解析；`code` 取 `occupied`/`startup-failed`/`cleanup-failed`/`unsupported`，可带 `holderPid`、`generation`、`message`；Rust 不能只靠 stderr 区分占用和崩溃 |
| 关停 | `nano.shutdown` → 拒绝新增动作、逐项清理结果、持久化结果，然后进程结束 |
| 流控制 | `nano.subscribe`、`nano.unsubscribe`；订阅目标必须带主/子会话准确地址 |
| 请求取消 | `nano.cancelRequest` 只取消可取消的等待，不宣称已接受动作未发生 |
| PTY 附着 | `nano.terminal.attach`：`sessionId`、可选 `terminalId`；返回 snapshot、`attachmentId`、output sequence 起点、`writable` |
| PTY 输入 | `nano.terminal.input`：`attachmentId`、attachment-local `sequence`、UTF-8 字节；不带认证 |
| PTY ack | `nano.terminal.ack`：对应 `sequence`，可带 `refusal: read-only|not-running`；拒绝也回 ack |
| PTY 状态 | `nano.terminal.state`：`attachmentId`、`writable`、可选 info；控制权转移/rename/resize/排水结果走这里 |
| 审批答案 | `nano.approval.answer`：`requestId`、`outcome: allowed-once|rejected`；不接受其他 outcome |
| 问卷答案 | `nano.question.answer`：`requestId`、逐题 `{ id, selected, custom? }` |
| 附件 chunk | `nano.attachment.chunk`：`receiptId`、`sequence`、base64 字节；单 chunk 原始字节上限 48 KiB |

应用端点的方法与字段在 `protocol/` 的一份机器可校验契约定义，TypeScript/Rust 的类型或样本由该定义约束。不能两边手抄无人校验的枚举。普通结果使用命名字段；时间写 UTC 毫秒，持续时间用单调时钟；offset 与 Unicode 索引单位必须在字段名/定义中写清。

`nano.hello` major 不同直接拒绝；minor 新增可选字段可忽略，未知必需能力或事件必须错误，不猜兼容。首版只宣称官方 `0.2.1-rc.2` 的验证组合；其他版本不得自动降级字段。

### 3.1 容量、背压与响应性

初始 frame 上限 32 MiB，pending 请求上限 64。到达声明的上限必须及时错误，不能解析半截消息或无限缓存。批量附件经有大小上限的 chunk API，单 chunk 原始字节最多 48 KiB；禁止把大图片/日志写到 stderr。

事件 writer 按字节有界：控制/审批/错误优先于可以重新取得的预览；持久事件不丢弃、不换序。慢消费者无法及时接收时关闭对应订阅，显式报告需从快照恢复，不阻塞官方持久化或无限积压。高频 preview 可以合并，但标识、最终状态、tool result 和审批撤回不能被合并掉。

UI 的事件接收与绘制解耦；输入处理不能被一整段 JSON 转换、附件或历史分页堵住。frame 与累计队列限制都测，不能仅限制条数。

### 3.2 重试与未知结果

官方主会话 prompt 的 `requestId` 在用户提交时生成，桥接保持其原值；有明确官方幂等语义才能同 ID 重查/重试。RPC `id` 不是业务幂等身份。

Shell 启动、PTY write、schedule create、权限变更等没有可证明重放语义的操作，断流后标“结果未知”，先查询官方当前状态，不能自动重发。PTY 新 attachment 绝不回放未确认按键。

## 4. 生命周期

```text
stopped -> starting -> handshaking -> ready
starting|handshaking -> stopping -> stopped | cleanup-failed   # 启动半途取消
starting|handshaking -> failed -> stopped | cleanup-failed
ready -> disconnected   # 传输断开、宿主尚存；只重建订阅
disconnected -> ready
ready -> lost           # 宿主死去；需要显式重新连接/恢复
lost -> stopping -> stopped | cleanup-failed
ready -> stopping -> stopped | cleanup-failed
```

`disconnected` 和 `lost` 是两件事：前者宿主还活着，Rust 只重建可观察订阅，不自动另起会操作同一会话的宿主；后者宿主死去，需要一次明确「重新连接/恢复」，不自动恢复原命令或 Monitor。`failed` 和启动期取消都必须继续走到 `stopped`/`cleanup-failed`，不能悬空。

## 5. 退出与异常

正常退出由 Rust 发起 shutdown，宿主先拒绝新增工作，停止 schedule 新投递与 Monitor 新事件，取消并排空监听，终止自己拥有的 Shell/jobs/人工 PTY，取消主 Agent 和子树，等待持久化与官方树清理，再回复结果并退出。不要在执行者尚可写入时先关闭持久化。宿主清理完成后才删除自己的会话挂载记录；清理失败或宿主仍存活时保留认领并报告，不能提前释放。Rust 崩溃后由 pid 存活判定回收。

协议 ACK、进程已结束、子进程范围已清空、会话已保存是四个不同事实。关停报告逐项给 `stopped`、`failed` 或 `unknown`。初始优雅清理总预算 10 秒；逾期提示后，仅对本窗口拥有的范围 TERM，再等待最多 3 秒，必要时 KILL；失败仍返回非零，不输出“全部已停止”。

Rust 退出/崩溃导致专属管道 EOF，桥接触发同一清理。SIGINT/SIGTERM/SIGHUP 都进入一次性退出路径；重复信号可强制结束但必须说明未验证清理。启动失败时同样清理迟到的分配，不留半启动宿主。

macOS 官方执行器用可观察进程树/进程组，无法证明抓到所有主动逃逸后代。不能靠 PID 名称清理，也不承诺 SIGKILL/断电能保存内存；S13 记录受测普通命令范围和不可保证范围。不自动接入 tmux、launchd 或第三方 supervisor 弥补非需求。

## 6. 数据与隐私

Nano 会话根与 KV 根不覆盖官方 dsh 数据。profile 启动时用 `--patch` 设置 `session-persistence-jsonl.root = <数据根>/sessions`（共享会话日志，官方 per-session flock）和 `storage-json.root = <数据根>/runtime-data/<rootSessionId>/storage`（所有 storage-json domain 按根会话隔离，避免不同宿主并发写同一 unit）；不写 `$DSH_HOME/storages`，也不让两个宿主共享同一 storage root。workspace/projection cache 因此是窗口本地视图，不跨窗口共享；会话列表来自共享 `sessions/` 和 session-query，不依赖 workspace 域。session.jsonl 等仍由官方格式和迁移管理；Rust 不读写压缩流。诊断保留版本、方法、状态、计数、延迟和错误分类，默认不含用户正文/参数/环境值。

本地目录初始 POSIX 0700，诊断/偏好文件 0600。会话挂载账本只含 pid、启动时间和 sessionId，不含用户正文；账本损坏时拒绝启动，不当作空账本。凭据仅在官方 provider 内解析，不跨协议回传。官方发行版支持的凭据读取与迁移行为要在首次使用说明中准确披露；不宣称同系统用户文件权限能隔离 Agent。

stdout 被第三方插件污染时协议失败，错误只显示净化后的诊断，不把随机输出解析成助手内容。不为兼容静默剥除 stdout 杂行。

## 7. 验收

- [x] 无凭据加载真实官方 profile 与导出服务，握手、订阅和关闭能跑；不是所有服务都 mock。
- [x] 证明无 WebServer 组合可用、没有浏览器/监听端口、没有简版 SDK 偷换。
- [x] 同项目两个 worktree 窗口、同 checkout 两个不同会话均正常；关闭 A 不终止 B。
- [ ] 同会话双 writer 拒绝，调度未提前执行；writer-held 不改成新会话（S01 认领 + S07 联合）。
- [ ] 两个宿主同时认领同一根会话，第二个在创建/恢复会话和挂 schedule 之前被拒绝；blank 会话也成立。
- [x] 账本损坏/不可读时拒绝启动，不当作空账本；正常退出立即可被认领；宿主崩溃后由 pid 存活判定回收；pid 复用只导致误占用，不导致双写。
- [ ] 所有 `storage-json` domain 走 per-root root；两个根会话互相看不到对方的调度任务和 workspace 视图；会话列表来自共享 `sessions/`（S01 + S11 联合）。
- [ ] 切换根会话后，transcript、草稿、队列、审批、问卷、终端、Monitor、调度列表都不出现上一个会话的内容（S01 + S02/S03/S07/S09/S10/S11 联合，S13 交叉核对）。
- [x] 分片 UTF-8/多 frame/超限/错误 JSON/未知 major/未知必需能力/旧 generation/流中断均明确处理；minor 可选字段可忽略，未知必需能力必须拒绝。
- [ ] Shell/PTY/schedule/权限变更断流后标结果未知，先查官方状态，不自动重发；PTY 新 attachment 不回放未确认按键。
- [ ] 背压时控制和审批可响应，最终事件没有静默丢失；附件不堵输入。
- [ ] EOF、正常退出、启动半途取消、TERM、Rust 崩溃均用真实短命令和 PTY 验证拥有范围（S01 + S09 联合）。
- [x] 故障注入持久化/清理失败，报告 unknown/failed 和非零退出，不误杀独立 dsh；实测 10s 优雅清理 + 3s TERM/KILL 预算。
- [x] 后端与 Rust 契约夹具、实际版本行、实际进程/文件证据随交付提供；发布矩阵归 S13；明确未测强杀/逃逸范围。

## 8. 机制清单

| 机制 | 解决什么 / 没有会怎样 | 确认与代价 |
| --- | --- | --- |
| 薄桥接 + 版本化通信 | 把官方类型变化限制在适配层；没有则 Rust 直接承担官方协议变化 | 总纲已确认；维护两种语言及通信契约 |
| 随开随关的资源拥有权 | 关窗口不再工作，且不误杀其他窗口；没有则遗留进程或互相终止 | 用户已确认；具体数据分区为本稿设计 |
| 会话挂载账本 | blank 会话没有跨进程写锁；没有则两个宿主可同时驱动同一会话并重复调度 | 作者建议（参照 dsh-TUI）；同机、pid 复用、需维护锁文件 |
| 调度专用后端路由（S11） | 让 schedule 表按根会话隔离，所有 storage-json domain 也不跨宿主共享；没有则两宿主争同一 unit | S11 提供 `provide` + 完整 storage-domain config；S01 只保证启动顺序 |
| generation + 生命周期状态机 | 重启后拒绝旧代事件/回执/审批/PTY；没有则旧窗口串新宿主 | 作者设计；需逐条验收状态边 |
| 四事实关停报告 | 区分 ACK、进程结束、范围清空、会话保存；没有则把「已发停止」当「已退出」 | 作者设计；10s+3s 预算 |
| 审批/问卷各一条 answerer | 两个官方 seam 不能共用一个回答者；没有则一边永远无人回答 | 官方能力；需分别注册并处理失败关闭 |
| 有界 writer + 慢消费者订阅关闭 | 背压时不阻塞持久化，也不无限缓存；没有则内存或持久化被拖死 | 作者建议；关闭后需从快照/游标恢复 |
| 未知结果不自动重发 | 断流后写入可能已提交；没有则重复副作用 | 总纲已确认；需先查官方状态 |
| 能力三态 | `unsupported`/`unavailable`/`error` 不能伪装成可用；没有则 UI 撒谎 | 总纲已确认；每个功能自证 |

## 9. 自定细节

初始目录、profile 名、NDJSON、32 MiB frame、48 KiB chunk、64 pending、10+3 秒退出预算和每窗口一个主会话树均为作者设计。其余子件不能自行另改。实施证据发现上游公开组合不成立时，先报告并修订此规格，不偷用私有源码。

- 会话挂载账本文件名 `session-mounts.json`、锁名 `session-mounts.lock`、锁 token `pid-nonce`、无有效 pid 的锁 30 秒过期窗口，均为作者设计；记录 owner 用宿主 pid。
- `nano.*` 命名空间、方法名、`runtimeId`/`generation`/`subscriptionId`/`sequence`/`attachmentId`、major/minor 兼容规则、`nano.startupError` 码表、生命周期状态名、0700/0600 权限，均为作者设计；见 §2/§3/§4。
- 认领必须在任何会话 action 和 schedule 挂载之前完成；后端激活靠 S11 的 `ctx.provide(storageBackendServiceKey(...), backend)`，不靠 patch 行序。
- `nano-schedule-json` 后端名、root `runtime-data/<rootSessionId>/storage` 和完整 storage-domain config 由 S11 定义；S01 只保证启动顺序和失败关闭。
- 宿主进入独立进程组并转发 SIGINT/SIGTERM/SIGHUP；这是作者设计，用于保证桥接清理先于宿主死亡。
- 账本只服务同机多窗口；跨机器并发不在首版范围，也不承诺 pid 复用下的自动回收。

## 10. 审核修订记录

待本稿逐份过闸；上述验收均未执行。

2026-10-21 原型（独立 Node 脚本，非完整宿主）：同一套 `session-mounts` 算法下，两个进程抢同一会话只有一个成功，另一个报 `session-owner`；不同会话可并行认领；owner 退出后新进程可回收。完整 profile、宿主启动顺序和真实 `nano.*` 握手仍待 S01 验收。
