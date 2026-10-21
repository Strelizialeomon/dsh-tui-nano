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
NANO_E2E=1 cargo test --test integration -- --test-threads=1

# 10s + 3s 关停预算的慢测试
NANO_E2E=1 NANO_E2E_SLOW=1 cargo test --test integration shutdown_budget_is_bounded_when_cleanup_hangs
```

前提：本机 `node`、`dsh`（精确 `0.2.1-rc.2`）与 Rust `stable` 可用。集成测试使用临时 `DSH_HOME` 和临时数据根，不读写用户的 `~/.dsh` 会话。

## 已知边界

- 首版只宣称 macOS + Ghostty 全屏；其他环境不宣称可用。
- S01 不打开 HTTP/WebSocket 监听端口，不接入浏览器或 Web 页面。
- 调度（S11）、Monitor（S10）、PTY（S09）、审批/问卷 UI（S03）等能力在 hello 里明确报 `unsupported`，不是静默缺省。
- 官方升级不在验证矩阵内时启动直接拒绝；不猜字段、不降级。
