# dsh-tui-nano

通过 DeepSeek Harness 的公开能力接入，重新构建一个极简终端界面。

- [项目目的与边界](https://github.com/Strelizialeomon/dsh-tui-nano/issues/1)
- [规格总纲](docs/specs/2026-10-20-nano-v1/README.md)
- [设计资料索引](docs/design/README.md)
- [交互原型](docs/design/2026-10-17-dsh-tui-ux-research/preview/index.html)

## 当前状态

S01「宿主与通信」已实施（执行单 [#4](https://github.com/Strelizialeomon/dsh-tui-nano/issues/4)）：Rust 运行时能启动一份真实的 `dsh --profile dsh-tui-nano` 宿主，通过 fd 3/4 交换版本化 NDJSON，完成握手、会话创建/恢复、订阅、关停预算和会话挂载互斥。功能 UI（S02-S13）尚未实现；S01 不提供界面，只提供可启动、可关停的适配边界与共享协议。

## 结构

| 路径 | 内容 |
| --- | --- |
| `protocol/` | `nano-protocol.json` 机器可校验契约；Rust 与 TypeScript 测试都读取它 |
| `bridge/` | Cordis 插件、fd 3/4 传输、会话挂载账本、订阅、关停；profile 安装器 |
| `crates/nano-runtime/` | Rust 侧：preflight、启动、握手、请求/事件、生命周期、关停四事实 |
| `docs/specs/2026-10-20-nano-v1/` | 已合并的规格 |

## 命令

```sh
# 契约与桥接单元测试（无需凭证）
node --test bridge/test/*.test.mjs

# Rust 单元与契约测试
cargo test

# 真实宿主集成测试（会运行 pnpm 安装 profile、启动真实 dsh）
cargo test --test integration -- --ignored --test-threads=1 --skip shutdown_budget_is_bounded_when_cleanup_hangs

# 10s + 3s 关停预算的慢测试
NANO_E2E_SLOW=1 cargo test --test integration -- --ignored --exact shutdown_budget_is_bounded_when_cleanup_hangs

# 桥接侧真实宿主测试（错误 JSON 后连接仍可用）
NANO_E2E=1 node --test bridge/test/integration/host.test.mjs
```

前提：本机 `node`、`dsh`（精确 `0.2.1-rc.2`）与 Rust `stable` 可用。集成测试使用临时 `DSH_HOME` 和临时数据根，不读写用户的 `~/.dsh` 会话。

## 已知边界

- 首版只宣称 macOS + Ghostty 全屏；其他环境不宣称可用。
- S01 不打开 HTTP/WebSocket 监听端口，不接入浏览器或 Web 页面。
- 调度（S11）、Monitor（S10）、PTY（S09）在 hello 里报 `unsupported`；审批与问卷的官方 seam 已组合，但 S01 不注册回答者，因此报 `unavailable`（回答者属 S03）。不是静默缺省。
- 官方升级不在验证矩阵内时启动直接拒绝；不猜字段、不降级。
- 诊断只保留版本、方法、状态、计数、延迟和错误分类；宿主 stderr 只保留有界尾部用于启动失败定位，不写入日志文件，不主动记录用户正文/参数/环境值。
- 凭据只在官方 provider 内解析；Nano 不复制凭据，不改 `$DSH_HOME` 的凭据、全局指令或 `storages/`。官方 profile 支持的凭据读取/迁移行为沿用官方实现，首次使用时应按官方说明核对。
- 强杀、断电和主动逃逸的后代进程不作清理保证；S01 只对本窗口拥有的进程组负责，不扫描进程名。
