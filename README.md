# dsh-tui-nano

通过 DeepSeek Harness 的公开能力接入，重新构建一个极简终端界面。

- [项目目的与边界](https://github.com/Strelizialeomon/dsh-tui-nano/issues/1)
- [规格总纲](docs/specs/2026-10-20-nano-v1/README.md)
- [设计资料索引](docs/design/README.md)
- [交互原型](docs/design/2026-10-17-dsh-tui-ux-research/preview/index.html)

## 当前状态

S01「宿主与通信」已实施（执行单 [#4](https://github.com/Strelizialeomon/dsh-tui-nano/issues/4)）：Rust 运行时能启动一份真实的 `dsh --profile dsh-tui-nano` 宿主，通过 fd 3/4 交换版本化 NDJSON，完成握手、会话创建/恢复、订阅、关停预算和会话挂载互斥。

S02「对话与输入」已实施（执行单 [#5](https://github.com/Strelizialeomon/dsh-tui-nano/issues/5)）：`nano` 终端应用可编辑多行输入、按地址保存草稿、提交/排队/插话/停止、同步官方 inbox、合并流式预览与持久消息；`bridge/conversation/` 提供 submit、queue 编辑/删除/插话、cancel、snapshot 与三类实时事件。transcript 的阅读层级与样式属 S03。

S04「模型与权限」已实施（执行单 [#7](https://github.com/Strelizialeomon/dsh-tui-nano/issues/7)）：`/model` 打开官方模型目录，按 provider 分组、只列模型声明的推理强度；切换走官方选择路径并写 `model/selection`。`/permission` 打开官方权限预设，切换走官方 `/permission <preset>` 命令（`setPolicy`；审批策略实际变化时由官方注入 changed-by-user 通知，no-op 不通知）。`Ctrl+P` 打开命令面板，命令列表来自官方命令服务；底部常驻权限/安全状态。

S11「调度」已实施（执行单 [#14](https://github.com/Strelizialeomon/dsh-tui-nano/issues/14)）：Ctrl+G 打开官方定时任务列表，六种时间选择器经真实 `ctx.schedule` 创建/修改/删除，展示最近回执与有界投递历史；`bridge/schedule/` 注册 `nano-schedule-json` 后端、`nano_probe` 路由探针和 `schedule.lock` 租约，按根会话隔离任务表。首版无暂停，投递会唤醒模型并可能产生费用。

S06「引用与图片」已实施（执行单 [#9](https://github.com/Strelizialeomon/dsh-tui-nano/issues/9)）：输入框支持 `@file` 补全（只插入路径，不读文件内容）、Alt+A 附加本地图片/通用文件、Alt+Backspace 移除本地卡片；图片在提交时交给官方 `ctx.attachments.admitPromptContent` 校验、规范化并持久化；通用文件经 chunk 上传，`finish` 时由官方 `saveFileStream` 生成 `FileAttachmentRef`，提交时校验归属并转交。附件的顺序与文本一致，失败保留草稿和卡片。

S07「会话与上下文」实施中（执行单 [#10](https://github.com/Strelizialeomon/dsh-tui-nano/issues/10)）：`Alt+O` 打开官方会话列表并可新建/恢复会话（受控重启、旧宿主完全退出后释放认领）、`n` 改名当前活动会话、`Alt+C` 走官方 `/compact`；标题栏显示 `contextPressure.projectedTokens / contextWindow` 的参考占用，面板分开展示计费用量、最近 provider 压力与启发式构成。冷会话列表只读官方投影/缓存，不打开正文。证据见 `docs/evidence/2026-10-21-s07/`。

S05「技能与规矩」已实施（执行单 [#8](https://github.com/Strelizialeomon/dsh-tui-nano/issues/8)）：`Alt+K` 打开官方用户可调用技能目录（含仅用户可调用标记与官方路径），`P` 只读预览 `SKILL.md`，`Enter` 把字面量 `/name` 填入输入框；官方 provider watcher 变化后只订阅 `skills/change` 并重拉目录。`Alt+R` 显示官方 `agent-instructions` 实际加载的全局链/项目链、去重后的来源、最近一次加载时间与预算省略/截断提示。与官方命令同名的 `/name` 在成为 prompt 前被拦下（命令入口属 S04，尚未接入）。

S10「Monitor」已实施（执行单 [#13](https://github.com/Strelizialeomon/dsh-tui-nano/issues/13)）：在官方任务面板选中任务按 `m`，先看到「已有 N 字节未读」再选择只看新输出或从 retained head 重放；监听使用自己的绝对字节偏移和非消费 `readAt`，不移动模型 `job_output` 游标。匹配行按合并窗口生成首行/末行/省略数量的 user-role 通知，支持 `notify`（官方 `followup`，唤醒并产生费用）与 `quiet`（官方 `inject`，不唤醒），界面显示 `role=user / source=monitor`、预算/有效期、缺口/已裁剪和真实投递回执；只有 durable `user/message` 显示为已收件。终态只进 UI，不额外发模型通知，也不抑制官方完成通知。证据见 `docs/evidence/2026-10-22-s10/`。

## 结构

| 路径 | 内容 |
| --- | --- |
| `protocol/` | `nano-protocol.json` 机器可校验契约；Rust 与 TypeScript 测试都读取它 |
| `bridge/runtime/` | Cordis 插件、fd 3/4 传输、会话挂载账本、订阅、关停；profile 安装器 |
| `bridge/conversation/` | 对话宿主：提交、官方 inbox、队列编辑/删除/插话、停止、快照、实时流 |
| `bridge/schedule/` | 调度宿主：`nano-schedule-json` 后端、路由探针、`schedule.lock` 租约、官方 schedule RPC |
| `bridge/attachments/` | 附件宿主：chunk 暂存、会话绑定回执、官方图片准入、通用文件保存、`@file` 发现 |
| `bridge/controls/` | 控制宿主：官方命令目录、模型目录/选择、权限预设读写与两类实时事件 |
| `bridge/skills/` | 技能目录/预览与规矩来源投影；只读官方 `ctx.skills`、官方命令名与 `agent-instructions` 持久消息 |
| `bridge/sessions/` | 会话宿主：官方列表、活动会话改名、上下文占用投影、官方 `/compact` |
| `bridge/monitor/` | 监听宿主：非消费任务输出、行合并/过滤、`followup`/`inject` 投递与回执、预算/有效期 |
| `crates/nano-runtime/` | Rust 侧：preflight、启动、握手、请求/事件、生命周期、关停四事实 |
| `crates/nano-conversation/` | 输入缓冲、按地址草稿、提交状态机、队列投影、流式预览、消息历史、场景/焦点 |
| `crates/nano-schedule/` | 定时任务目录、六种时间选择器表单、比较并交换修改、投递历史与调度覆盖层状态 |
| `crates/nano-attachments/` | `@file` 语法、图片本地检查、有序附件卡片、补全取消与陈旧响应防护 |
| `crates/nano-controls/` | 模型目录与强度、权限预设、命令面板与底部权限事实 |
| `crates/nano-skills/` | 技能目录（保留上次可用、可能不完整）、命令优先裁决、预览、规矩来源、`/name` 注入事实 |
| `crates/nano-sessions/` | 官方会话行、受控切换状态机、上下文占用、改名与压缩结果 |
| `crates/nano-monitor/` | 监听投影：偏移、预算、投递回执、缺口/裁剪与 UI-only 流终态 |
| `crates/nano-app/` | `nano` 终端应用：键位、渲染、宿主连接、受控重启与退出 |
| `docs/specs/2026-10-20-nano-v1/` | 已合并的规格 |

## 命令

```sh
# 桥接依赖（官方公开导出）
npm install --prefix bridge

# 契约与桥接单元测试（无需凭证）
node --test bridge/test/*.test.mjs

# 构建并启动 TUI（首次会安装 profile）；带初始路由会使用官方凭据并产生模型调用
cargo run -p nano-app -- --provider deepseek-official --model deepseek-chat

# Rust 单元与契约测试
cargo test

# 真实宿主集成测试（会运行 pnpm 安装 profile、启动真实 dsh）
cargo test -p nano-runtime --test integration -- --ignored --test-threads=1 --skip shutdown_budget_is_bounded_when_cleanup_hangs

# 10s + 3s 关停预算的慢测试
NANO_E2E_SLOW=1 cargo test -p nano-runtime --test integration -- --ignored --exact shutdown_budget_is_bounded_when_cleanup_hangs

# 桥接侧真实宿主测试（错误 JSON 后连接仍可用；对话 submit/去重/resume 重放；技能发现/预览/watcher/注入）
NANO_E2E=1 node --test bridge/test/integration/*.test.mjs

# 调度真实宿主测试（官方 CRUD/冲突/投递/130 秒冷补投/租约/路由拒绝/跨会话隔离）
NANO_E2E=1 node --test bridge/test/integration/schedule.test.mjs

# S07 真实宿主测试（会话列表/改名/上下文/压缩/认领拒绝/resume）
NANO_E2E=1 node --test bridge/test/integration/sessions.test.mjs

# 真实 PTY 证据（输入、粘贴、提交、失败可见、干净退出；NANO_SCHEDULE=1 走调度覆盖层）
python3 scripts/pty-evidence.py

# S06 真实 PTY 证据（@ 补全、附件选择、图片上传、顺序/重排回显、干净退出）
python3 scripts/pty-evidence-attachments.py

# S07 真实 PTY 证据（会话面板、改名、压缩、受控切换、账本释放）
python3 scripts/sessions-evidence.py

# S04 真实 PTY 证据（模型目录/强度选择、权限预设切换、官方持久事件）
python3 scripts/pty-controls-evidence.py

# S05 真实 PTY 证据（技能面板/预览/watcher/注入/规矩来源与预算提示；使用本机 mock 模型，无真实费用）
python3 scripts/pty-evidence-s05.py

# S06 真实宿主测试（官方附件服务、文件保存、顺序准入、恢复重放）
NANO_E2E=1 node --test bridge/test/integration/attachments.test.mjs
```

前提：本机 `node`、`dsh`（精确 `0.2.1-rc.2`）与 Rust `stable` 可用；桥接依赖 `@deepseek-ai/dsh-agent` 与 `@deepseek-ai/dsh-api-session-controller` 的公开导出（`npm install --prefix bridge` 或 profile 安装时由 pnpm 拉取）。集成测试使用临时 `DSH_HOME` 和临时数据根，不读写用户的 `~/.dsh` 会话。

## 已知边界

- 首版只宣称 macOS + Ghostty 全屏；其他环境不宣称可用。
- S01 不打开 HTTP/WebSocket 监听端口，不接入浏览器或 Web 页面。
- 调度（S11）只复用官方服务：没有暂停/恢复 API，停止任务只能删除；投递非严格一次，界面常驻提示。Monitor（S10）、PTY（S09）在 hello 里报 `unsupported`；审批与问卷的官方 seam 已组合，但 S01 不注册回答者，因此报 `unavailable`（回答者属 S03）。不是静默缺省。
- 官方升级不在验证矩阵内时启动直接拒绝；不猜字段、不降级。
- 模型目录与权限预设都来自官方服务；切换是「可失败的动作」，没有官方回执不显示成功。第二个窗口不能写同一根会话：会话挂载由 S01 认领，模型写入按 writer-held 处理、不接管。
- 未在真实服务上验证的第三方 provider 会以官方 `failures` 原因显示，不会被另一个模型顶替；本份证据只覆盖 `deepseek-official`。
- `/name` 与官方命令同名时命令优先；命令面板与命令执行属 S04。远程技能 provider 未验证；技能目录不完整时保留上次可用结果并标注。
- 对话区的纯文本 feed 是 S02 的可运行占位，不是 S03 的阅读层级；样式、折叠、差异与问卷/审批由 S03 接替。
- S07 的冷会话列表只显示官方 header 与投影缓存提示；由于 S01 把 projection cache 按根会话分区，别的根会话没有缓存提示时显示「未命名」「最近活动未知」，不会为补字段去打开冷会话正文。上下文占用需要一次真实 provider 请求才有 `projectedTokens`/`contextWindow`；没有时界面显示「未知」，不补零。
- 手动压缩走已组合的官方 `/compact`；没有命令适配器时只保留自动压缩。`Esc` 取消只停止等待，历史以后端结果为准。
- 诊断只保留版本、方法、状态、计数、延迟和错误分类；宿主 stderr 只保留有界尾部用于启动失败定位，不写入日志文件，不主动记录用户正文/参数/环境值。
- 凭据只在官方 provider 内解析；Nano 不复制凭据，不改 `$DSH_HOME` 的凭据、全局指令或 `storages/`。官方 profile 支持的凭据读取/迁移行为沿用官方实现，首次使用时应按官方说明核对。
- 强杀、断电和主动逃逸的后代进程不作清理保证；S01 只对本窗口拥有的进程组负责，不扫描进程名。
- S06 的 `@file` 补全来自官方 `dsh-file-reference-local`，只返回路径候选，选中后仍需模型调用文件工具才能读取内容；编辑器不把「已引用」显示成「已附上」。
- 图片格式/字节/像素/边长/每消息数量与总量以宿主的 `hello.attachmentLimits` 为准；本地先做魔数、类型、尺寸检查，官方准入仍是最终判定。当前模型明确不支持图片时，整条消息在发送前被拒绝，草稿保留。
- 通用文件按原字节保存，模型侧收到的是官方生成的文件名/字节数/摘要前缀句柄；执行文件系统能映射时才有只读路径。Nano 不自行计算摘要，也不把本地路径写进会话事件。
- 子代理入口属 S12；本份在协议校验层规定子代理只收图片、通用文件被拒绝，但当前窗口没有子代理输入入口。
