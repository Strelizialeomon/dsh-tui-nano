# 证据索引（0.1.0）

所有命令均在合成 HOME / DSH_HOME / workdir 下运行；不读取或写入用户的真实凭据、会话和桌面。前置：`zstd` CLI（会话日志为多帧 zstd）、Pillow（截图差分）、Ghostty 与 macOS 辅助功能/屏幕录制权限。

| 文件 | 来源 | 证明 |
| --- | --- | --- |
| `install.txt` | `packaging/verify-install.sh --payload packaging/dist/dsh-tui-nano-0.1.0-arm64` | 干净安装、重装、版本拒绝、启动拒绝、卸载与数据保留 |
| `cross-feature.txt` | `NANO_BRIDGE_PACKAGE=<payload>/lib/bridge NANO_E2E=1 node --test tests/e2e/cross-feature.test.mjs` | 双窗口认领/释放、双 root 并发、跨根会话 transcript 与调度隔离、错误路由拒绝 |
| `cargo-test.txt` | `cargo test --workspace` | Rust 单元与契约回归 |
| `host-integration.txt` | `cargo test -p nano-runtime --test integration -- --ignored --test-threads=1 --skip shutdown_budget_is_bounded_when_cleanup_hangs` | 真实宿主的退出/EOF/信号/崩溃恢复等 ignored 套件 |
| `shutdown-budget.txt` | `NANO_E2E_SLOW=1 cargo test -p nano-runtime --test integration -- --ignored --exact shutdown_budget_is_bounded_when_cleanup_hangs` | 10s + 3s 关停预算慢测试 |
| `bridge-test.txt` | `npm run test:bridge` | 桥接单元回归 |
| `ghostty/ghostty-acceptance.txt` | `python3 tests/e2e/ghostty-acceptance.py --payload …` | 真实 Ghostty 的 24 项窗口级检查与截图生成记录 |
| `ghostty/*.png` | 同上 | 深/浅主题、100/60 列（窗口与全屏）、面板、CJK、模型响应、滚轮前后、全屏、reduced-motion。`06` 是第一次滚轮方向（无变化），`06b` 是反向滚轮后的变化视图；面板与滚轮检查用像素差分；全屏 100/60 用字号 27/45 逼近目标列数 |
| `ghostty/install.json` | 同上 | Ghostty 验收所用安装凭据（产物哈希、官方版本/来源、路径） |
| `artifact.txt` | 汇总命令 | 发布产物、官方版本/来源与版本矩阵的绑定 |

截图只截取本次运行拥有的窗口（`screencapture -l <windowid>`），不采集用户桌面或其他 Ghostty 窗口。窗口标题、工作目录和会话内容均为合成本次运行数据。
