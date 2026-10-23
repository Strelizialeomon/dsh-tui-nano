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

## 覆盖

- **颜色独立性**：2 种 TUI 主题 × 2 套故意错配的终端调色板 × 60/100 列，共 8 组。正常启动不注入 Nano 色表；错配调色板只作对照夹具。
- **主体透明**：`background-opacity=0.7`、`background-opacity-cells=false`，窗口背景用稳定棋盘图；深浅主题各一次，普通区域不得被实色填满。
- **不透明回归**：`background-opacity=1` 下棋盘图仍可见，证明普通背景是终端默认背景而不是 Nano 实色。
- **组件与退出**：命令面板、调度、交互终端、问卷决策卡、全屏；退出后临时 Ghostty 配置哈希不变。
- **隔离**：临时 `HOME`、临时数据根、`--config-default-files=false` + 独立 `--config-file`，不读写用户配置。

## 覆盖对照

| spec §3.4 surface | 可重复验证 | 真实终端 |
| --- | --- | --- |
| 标题、底栏、输入、队列、补全、活动行 | `nano-app` ui 单测（默认前景/边界、上下文线、`self_drawn_colors_are_explicit_rgb...`） | 8 组矩阵 + `components-main.png` |
| 根/子 transcript、实时预览、Markdown、diff | `nano-transcript` 单测 + PTY diff 背景断言 | 历史截图（不参与结论） |
| 会话、技能、规矩、任务、monitor、调度、面板 | ui 渲染单测 + 既有验收截图 | `components-command.png`、`components-schedule.png` |
| 问卷/审批/计划卡 | `option_line`、审批首行 fg 单测 | `components-decision.png` |
| dock、子代理列表 | dock hover/焦点单测 + PTY hover_bg 断言 | 历史截图（不参与结论） |
| 局部高亮（diff/选择/hover） | diff 背景、选中行、dock hover 单测 | `local-cells-true.png`、`local-diff-cells-true.png` |
| 交互终端外壳/占位/picker | terminal 默认前景与显式色保留单测 | `components-terminal.png` |
| 全屏 | 既有全屏布局回归 | 历史截图（不参与结论） |
| 内嵌子程序显式颜色 | `default_foreground_follows_theme...` | `components-terminal.png` |

单测断言精确语义 RGB；截图只证明真实终端下的颜色独立性与背景边界，精确值以 `nano-theme` 色表单测为准。

## 结果（2026-10-23）

### 无头 PTY（默认）

- `theme-pty.json`：37 项全绿；覆盖深浅主题 `fg/muted/accent/rule`、命令面板 `selected_bg`、决策卡 `accent`、diff 增删背景、dock `hover_bg`、普通背景 Reset、无 ANSI 命名色/Indexed、`TERM` 不改变 Nano RGB。
- 截图不参与默认回归；精确色值以 PTY 的 SGR 真彩色断言为准。

### Ghostty 透明（按需，历史截图）

- `theme-transparency.json` 只保留能从提交截图复现的 **11 项透明 alpha 检查**：深浅主题的初始、运行、面板打开、面板关闭后、缩放后各 5 项（alpha=179），加不透明回归（alpha=255，棋盘不可见）。
- 矩阵/组件/全屏截图来自更早的二进制（`dark-*.png`、`components-*.png` 等），仅作历史留存，**不参与本次结论**；颜色角色由 `theme-pty.json` 的 37 项覆盖。
- 没有在最终二进制上重新启动 GUI；需要重跑时按上面的「按需」命令，并由用户确认当前可占用桌面。

## 产物

- `theme-transparency.json`：机器检查、8 组实测色值计数与终端/系统版本。
- `<theme>-<palette>-<width>.png`：8 组矩阵截图。
- `transparent-*.png`、`opaque-dark.png`、`components-*.png`、`fullscreen-dark.png`。

截图经过 macOS 色彩管理，像素值与 spec 的语义 RGB 有轻微偏移；脚本用容差识别主色，并在同一主题下比较两套终端调色板的实测颜色是否一致。精确色值由 `nano-theme` 单元测试断言，截图只证明真实终端下的独立性。

## 边界

- 局部高亮（diff/选择/hover）是否透明仍由终端决定，不作为主体透明失败。
- macOS 原生全屏会禁用背景透明；全屏证据只做布局回归。全屏窗口在独立 Space，脚本不置前发键，交互回归沿用既有 acceptance 脚本。
- 不读取、不修改用户真实 Ghostty 配置；临时配置通过 `--config-file` 实际加载，正常退出与启动失败退出后哈希都必须一致。
- 报告保留构建 commit、每次启动参数、每张截图窗口尺寸与主题/透明夹具。
