# 证据索引（0.1.0）

所有命令均在合成 HOME / workdir 下运行；不读取或写入用户的真实凭据、会话和桌面。

| 文件 | 来源 | 证明 |
| --- | --- | --- |
| `install.txt` | `packaging/verify-install.sh --payload packaging/dist/dsh-tui-nano-0.1.0-arm64` | 干净安装、重装、版本拒绝、启动拒绝、卸载与数据保留 |
| `cross-feature.txt` | `NANO_BRIDGE_PACKAGE=<payload>/lib/bridge NANO_E2E=1 node --test tests/e2e/cross-feature.test.mjs` | 双窗口认领/释放、跨根会话 transcript 与调度隔离、错误路由拒绝 |
| `cargo-test.txt` | `cargo test --workspace` | Rust 单元与契约回归 |
| `bridge-test.txt` | `npm run test:bridge` | 桥接单元回归 |
| `ghostty/ghostty-acceptance.txt` | `python3 tests/e2e/ghostty-acceptance.py --payload …` | 真实 Ghostty 的窗口级检查与截图生成记录 |
| `ghostty/*.png` | 同上 | 深/浅主题、100/60 列、面板、CJK、模型响应、滚轮前后、全屏、reduced-motion。`06` 是第一次滚轮方向（无变化），`06b` 是反向滚轮后的变化视图；检查取后者 |
| `ghostty/install.json` | 同上 | Ghostty 验收所用安装凭据（产物哈希、官方版本/来源、路径） |
| `artifact.txt` | 汇总命令 | 发布产物、官方版本/来源与版本矩阵的绑定 |

截图只截取本次运行拥有的窗口（`screencapture -l <windowid>`），不采集用户桌面或其他 Ghostty 窗口。窗口标题、工作目录和会话内容均为合成本次运行数据。
