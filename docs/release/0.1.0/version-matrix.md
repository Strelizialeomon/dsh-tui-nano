# 版本矩阵（0.1.0）

- 验证日期：2026-10-22
- 结论口径：只列实际执行过的组合；未测组合不宣称可用。

## 1. 产物

| 字段 | 值 |
| --- | --- |
| 版本 | `0.1.0` |
| 构建平台 | macOS 27.0.1（26A434），Apple M5，arm64 |
| Rust | `1.99.1`（`58cf8f9c3`） |
| Node | `v24.18.0` |
| 官方 dsh | `0.2.1-rc.2` |
| 官方源码基线 | `687f282e8625c6b8d11ff58211a90524be2e4317` |
| 终端 | Ghostty `1.3.1` |

官方源码基线是规格调查时固定的一手入口；安装产物只能证明 `dsh --version` 的版本号，不能自证源码提交。矩阵把两者分开记录，不把 commit 当作安装产物的一部分。

## 2. 支持矩阵

| 维度 | 首版承诺 | 不承诺 |
| --- | --- | --- |
| 平台 | macOS arm64 + Ghostty，全屏/窗口 | 其他终端、Windows、Linux、inline 渲染 |
| 官方 Harness | 精确 `0.2.1-rc.2` | 其他版本、未知版本 |
| 架构 | arm64（Apple M5 实测） | x86_64 或其他未构建架构 |
| 语言/输入 | 英文、中文粘贴与显示 | 真实系统输入法组合态（未测） |
| 主题 | Ghostty 深/浅两套映射（见 design-contract） | 任意主题或任意宽度 |
| 宽度 | 100 列与 60 列实测 | 其他宽度、鼠标窗口缩放后的任意列数 |

## 3. 构建与产物哈希

| 文件 | SHA-256 |
| --- | --- |
| `bin/nano` | `8eecddd54a9bf36baf11d0201afb8e86f8543653a37e0ac8d6c668f5e3a1af97` |
| `dsh-tui-nano-0.1.0-arm64.tar.gz` | `b7c0579bb132772fd38cde8e346dba98c34b0bdc448d4c39aa1b93eb5811577c` |
| `SHA256SUMS` | 见安装凭据 `payloadSumsSha256`（证据目录 `install.json`） |

产物哈希在最终构建后重新生成；若脚本或源码变更，必须重新构建并更新本表，不能沿用旧哈希。

## 4. 验证覆盖

| 项目 | 方式 | 证据 |
| --- | --- | --- |
| 安装/重装/卸载 | 合成 HOME 安装演练 | `evidence/install.txt` |
| 版本拒绝 | 假 dsh `0.2.1-rc.1` | `evidence/install.txt` |
| 双窗口认领与释放 | 真实宿主 | `evidence/cross-feature.txt` |
| 跨根会话隔离 | 真实宿主 + 唯一标记 | `evidence/cross-feature.txt` |
| 调度路由与隔离 | 真实宿主 | `evidence/cross-feature.txt` |
| Ghostty 视觉/交互 | 真实 Ghostty + System Events | `evidence/ghostty/` |
| 自动回归 | `cargo test --workspace`、桥接单测 | `evidence/cargo-test.txt`、`evidence/bridge-test.txt` |

未覆盖项见 `known-limitations.md`。
