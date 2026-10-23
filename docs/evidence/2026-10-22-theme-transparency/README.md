# 2026-10-22 主题配色与透明证据

- 规格：[2026-10-22-nano-theme-transparency](../../specs/2026-10-22-nano-theme-transparency/README.md)
- 执行单：#56
- 默认脚本：`tests/e2e/theme-pty.py`（无头 PTY）
- 按需脚本：`tests/e2e/ghostty-theme-transparency.py`（Ghostty 截图）
- 状态：无头 PTY 全绿；透明 alpha 11 项全绿（由本轮早前 GUI 采集的截图重算）。矩阵/组件截图来自更早构建，标为历史，不参与本次结论；最终修正后未重跑 GUI。

## 怎么跑

默认（无窗口、不抢焦点、不动剪贴板）：

```sh
cargo build -p nano-app
python3 tests/e2e/theme-pty.py
```

按需（需要终端合成/透明证据时；跑前必须由用户确认当前可占用桌面）：

```sh
python3 tests/e2e/ghostty-theme-transparency.py
```

GUI 前置：macOS 辅助功能权限（System Events 发键）与屏幕录制权限（窗口级截图）、Ghostty 1.3.1、`dsh`、`node`、Pillow。受限环境可用 `--capture-only`，但它不能替代完整证据。

## 覆盖（当前结论）

### 无头 PTY（默认）

- 深浅主题 `fg` / `muted` / `accent` / `rule`。
- 深/浅命令面板 `selected_bg`。
- 决策卡 `accent`。
- diff 增删背景与词背景。
- dock `hover_bg`。
- 普通背景 Reset、无 ANSI 命名色 / Indexed、`TERM` 变化不改变 Nano 发出的 RGB。
- `theme-pty.json` 共 42 项；error/warning/success 等状态色由 Rust 单测覆盖，不在 PTY 场景内。

### Ghostty 透明（按需，11 项 alpha）

- 深/浅主题各 5 帧：启动后（initial）、运行时、命令面板打开、面板关闭后、缩放后；空白区 alpha=179，说明窗口透明。
- 不透明回归：`background-opacity=1` 时空白区 alpha=255，棋盘合成后不可见。
- 起始帧是 TUI 启动后、不是「启动前」；没有退出后同一区域截图。因此 spec §6.2「主体透明」保持未勾，需在隔离会话补启动前/退出后证据。

### 历史截图（不参与结论）

- `dark-*.png`、`light-*.png`、`components-*.png`、`fullscreen-*.png`、`local-*.png` 来自更早的二进制。
- 保留用于追溯；不作为当前 verdict，也不声称 8 组矩阵/组件/全屏截图已复现。

## 覆盖对照

| spec §3.4 surface | 可重复验证 | 真实终端 |
| --- | --- | --- |
| 标题、底栏、输入、队列、补全、活动行 | ui 单测 + PTY `fg/muted/accent/rule` | 历史截图（不参与结论） |
| 根/子 transcript、实时预览、Markdown、diff | transcript 单测 + PTY diff 背景 | 历史截图（不参与结论） |
| 会话、技能、规矩、任务、monitor、调度、面板 | ui 单测 + PTY 命令面板 `selected_bg` | 历史截图（不参与结论） |
| 问卷/审批/计划卡 | `option_line`、审批首行 fg 单测 + PTY 决策卡 `accent` | 历史截图（不参与结论） |
| dock、子代理列表 | dock hover/焦点单测 + PTY `hover_bg` | 历史截图（不参与结论） |
| 局部高亮（diff/选择/hover） | diff 背景、选中行、dock hover 单测 + PTY | 历史截图（不参与结论） |
| 交互终端外壳/占位/picker | terminal 默认前景与显式色单测 | 历史截图（不参与结论） |
| 全屏 | 既有全屏布局回归 | 历史截图（不参与结论） |
| 内嵌子程序显式颜色 | `default_foreground_follows_theme...` | 历史截图（不参与结论） |

单测与 PTY 断言精确语义 RGB；历史截图只作追溯。

## 结果（2026-10-23）

- `theme-pty.json`：42 项全绿（7 个场景 + TERM 对比）。
- `theme-transparency.json`：11 项全绿（10 张透明帧 alpha=179，opaque alpha=255、exact_checker=844）。
- `cargo test --workspace`、bridge、cargo e2e、跨功能 e2e 全绿。

## 产物

- `theme-pty.json`：PTY 42 项与 7 个场景的 RGB 集合。
- `theme-transparency.json`：11 项透明 alpha 检查（只列可从截图复现的项）。
- `transparent-*.png`、`opaque-dark.png`：透明/不透明截图。
- 其余 `dark-*.png`、`components-*.png`、`fullscreen-*.png`、`local-*.png`：历史留存，不参与结论。

## 边界

- 局部高亮（diff/选择/hover）是否透明仍由终端决定，不作为主体透明失败。
- macOS 原生全屏会禁用背景透明；全屏交互证据未重跑，spec 对应验收保持未勾。
- 不读取、不修改用户真实 Ghostty 配置；临时配置通过 `--config-file` 实际加载。
- PTY 报告保留 `build_commit`；透明报告只保留 `mode/captured_by/recomputed_from/note/checks`，不声称保留启动参数或截图尺寸。
- GUI 默认不跑；需要时按上面的按需命令，并由用户确认或使用隔离会话。
