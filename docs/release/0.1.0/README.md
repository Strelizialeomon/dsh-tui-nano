# dsh-tui-nano 0.1.0 发布验收

- 规格：`docs/specs/2026-10-20-nano-v1/S13-release.md`；执行单：#16。
- 结论：**不发布**。存在 3 项 design-contract 阻断偏差，且人工输入法组合态未验证。详见 `release-conclusion.md`。
- 冻结产物：`packaging/dist/dsh-tui-nano-0.1.0-arm64.tar.gz`（SHA-256 见 `version-matrix.md`）。

## 目录

| 文件 | 内容 |
| --- | --- |
| `version-matrix.md` | 构建号、官方版本/源码基线、终端/系统/Node/CPU、产物哈希 |
| `known-limitations.md` | 未测项、已知边界和阻断项 |
| `release-conclusion.md` | 发布结论、阻断项、验收勾选 |
| `design-contract-compliance.md` | 对 design-contract 的逐项核对 |
| `evidence/` | 安装演练、跨功能 e2e、自动回归、Ghostty 截图与检查 |

## 复核方式

```sh
./packaging/build-release.sh
./packaging/verify-install.sh --payload packaging/dist/dsh-tui-nano-0.1.0-arm64
NANO_E2E=1 node --test tests/e2e/cross-feature.test.mjs
python3 tests/e2e/ghostty-acceptance.py --payload packaging/dist/dsh-tui-nano-0.1.0-arm64
cargo test --workspace
npm run test:bridge
```

Ghostty 验收需要 macOS 辅助功能权限（System Events）与屏幕录制权限（仅窗口级截图）。所有截图来自合成 HOME/workdir；不采集用户真实桌面。
