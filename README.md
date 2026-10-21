# dsh-tui-nano

通过 DeepSeek Harness 的公开能力接入，重新构建一个极简终端界面。

- [项目目的与边界](https://github.com/Strelizialeomon/dsh-tui-nano/issues/1)
- [规格总纲](docs/specs/2026-10-20-nano-v1/README.md)
- [设计资料索引](docs/design/README.md)
- [交互原型](docs/design/2026-10-17-dsh-tui-ux-research/preview/index.html)

## 当前状态

S01「宿主与通信」已实施（执行单 [#4](https://github.com/Strelizialeomon/dsh-tui-nano/issues/4)）：Rust 运行时能启动一份真实的 `dsh --profile dsh-tui-nano` 宿主，通过 fd 3/4 交换版本化 NDJSON，完成握手、会话创建/恢复、订阅、关停预算和会话挂载互斥。

S02「对话与输入」实施中（执行单 [#5](https://github.com/Strelizialeomon/dsh-tui-nano/issues/5)）：`nano` 终端应用可编辑多行输入、按地址保存草稿、提交/排队/插话/停止、同步官方 inbox、合并流式预览与持久消息；`bridge/conversation/` 提供 submit、queue 编辑/删除/插话、cancel、snapshot 与三类实时事件。transcript 的阅读层级与样式属 S03，模型选择属 S04；当前可用 `--provider/--model` 传入初始路由。

S05「技能与规矩」实施中（执行单 [#8](https://github.com/Strelizialeomon/dsh-tui-nano/issues/8)）：`Alt+K` 打开官方用户可调用技能目录（含仅用户可调用标记与官方路径），`P` 只读预览 `SKILL.md`，`Enter` 把字面量 `/name` 填入输入框；官方 provider watcher 变化后只订阅 `skills/change` 并重拉目录。`Alt+R` 显示官方 `agent-instructions` 实际加载的全局链/项目链、去重后的来源、最近一次加载时间与预算省略/截断提示。与官方命令同名的 `/name` 在成为 prompt 前被拦下（命令入口属 S04，尚未接入）。

## 结构

| 路径 | 内容 |
| --- | --- |
| `protocol/` | `nano-protocol.json` 机器可校验契约；Rust 与 TypeScript 测试都读取它 |
| `bridge/runtime/` | Cordis 插件、fd 3/4 传输、会话挂载账本、订阅、关停；profile 安装器 |
| `bridge/conversation/` | 对话宿主：提交、官方 inbox、队列编辑/删除/插话、停止、快照、实时流 |
| `bridge/skills/` | 技能目录/预览与规矩来源投影；只读官方 `ctx.skills`、官方命令名与 `agent-instructions` 持久消息 |
| `crates/nano-runtime/` | Rust 侧：preflight、启动、握手、请求/事件、生命周期、关停四事实 |
| `crates/nano-conversation/` | 输入缓冲、按地址草稿、提交状态机、队列投影、流式预览、消息历史、场景/焦点 |
| `crates/nano-skills/` | 技能目录（保留上次可用、可能不完整）、命令优先裁决、预览、规矩来源、`/name` 注入事实 |
| `crates/nano-app/` | `nano` 终端应用：键位、渲染、宿主连接与退出 |
| `docs/specs/2026-10-20-nano-v1/` | 已合并的规格 |

## 命令

```sh
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

# 真实 PTY 证据（输入、粘贴、提交、失败可见、干净退出）
python3 scripts/pty-evidence.py

# S05 真实 PTY 证据（技能面板/预览/watcher/注入/规矩来源与预算提示；使用本机 mock 模型，无真实费用）
python3 scripts/pty-evidence-s05.py
```

前提：本机 `node`、`dsh`（精确 `0.2.1-rc.2`）与 Rust `stable` 可用。集成测试使用临时 `DSH_HOME` 和临时数据根，不读写用户的 `~/.dsh` 会话。

## 已知边界

- 首版只宣称 macOS + Ghostty 全屏；其他环境不宣称可用。
- S01 不打开 HTTP/WebSocket 监听端口，不接入浏览器或 Web 页面。
- 调度（S11）、Monitor（S10）、PTY（S09）在 hello 里报 `unsupported`；审批与问卷的官方 seam 已组合，但 S01 不注册回答者，因此报 `unavailable`（回答者属 S03）。不是静默缺省。
- 官方升级不在验证矩阵内时启动直接拒绝；不猜字段、不降级。
- 当前没有模型选择界面（S04）；`--provider/--model` 只是启动时的初始路由，是用户批准的最小范围例外（S04 仍拥有模型选择业务与切换）。没有路由或凭据时，官方回合会明确失败，界面不会伪造回答。
- 命令面板与命令执行属 S04，当前未接入；`/name` 与官方命令同名时会被明确拦下并保留草稿，不会误发成技能。远程技能 provider 未验证；技能目录不完整时保留上次可用结果并标注。
- 对话区的纯文本 feed 是 S02 的可运行占位，不是 S03 的阅读层级；样式、折叠、差异与问卷/审批由 S03 接替。
- 诊断只保留版本、方法、状态、计数、延迟和错误分类；宿主 stderr 只保留有界尾部用于启动失败定位，不写入日志文件，不主动记录用户正文/参数/环境值。
- 凭据只在官方 provider 内解析；Nano 不复制凭据，不改 `$DSH_HOME` 的凭据、全局指令或 `storages/`。官方 profile 支持的凭据读取/迁移行为沿用官方实现，首次使用时应按官方说明核对。
- 强杀、断电和主动逃逸的后代进程不作清理保证；S01 只对本窗口拥有的进程组负责，不扫描进程名。
