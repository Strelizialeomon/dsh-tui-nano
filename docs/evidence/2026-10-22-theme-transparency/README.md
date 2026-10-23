# 2026-10-22 主题配色与透明证据

- 规格：[2026-10-22-nano-theme-transparency](../../specs/2026-10-22-nano-theme-transparency/README.md)
- 执行单：#56
- 脚本：`tests/e2e/ghostty-theme-transparency.py`
- 状态：待运行（实现完成后统一执行，结果写入本目录）

## 怎么跑

```sh
cargo build -p nano-app
python3 tests/e2e/ghostty-theme-transparency.py
```

前置：macOS 辅助功能权限（System Events 发键）、屏幕录制权限（窗口级截图）、Ghostty 1.3.1、`dsh`、`node`、Pillow。

受限环境可先跑 `--capture-only`：跳过键盘输入，只抓首帧并按 PID 关闭窗口，用来验证截图链路与调色板隔离；它不能替代完整证据。

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
| 根/子 transcript、实时预览、Markdown、diff | `nano-transcript` 单测（主题缓存隔离、diff 背景/强调） | 8 组矩阵 |
| 会话、技能、规矩、任务、monitor、调度、面板 | ui 渲染单测 + 既有验收截图 | `components-command.png`、`components-schedule.png` |
| 问卷/审批/计划卡 | `option_line`、审批首行 fg 单测 | `components-decision.png` |
| dock、子代理列表 | dock hover/焦点单测 | 8 组矩阵截图 |
| 交互终端外壳/占位/picker | terminal 默认前景与显式色保留单测 | `components-terminal.png` |
| 内嵌子程序显式颜色 | `default_foreground_follows_theme...` | `components-terminal.png` |

单测断言精确语义 RGB；截图只证明真实终端下的颜色独立性与背景边界，精确值以 `nano-theme` 色表单测为准。

## 产物

- `theme-transparency.json`：机器检查、8 组实测色值计数与终端/系统版本。
- `<theme>-<palette>-<width>.png`：8 组矩阵截图。
- `transparent-*.png`、`opaque-dark.png`、`components-*.png`、`fullscreen-dark.png`。

截图经过 macOS 色彩管理，像素值与 spec 的语义 RGB 有轻微偏移；脚本用容差识别主色，并在同一主题下比较两套终端调色板的实测颜色是否一致。精确色值由 `nano-theme` 单元测试断言，截图只证明真实终端下的独立性。

## 边界

- 局部高亮（diff/选择/hover）是否透明仍由终端决定，不作为主体透明失败。
- macOS 原生全屏会禁用背景透明；全屏证据只做布局回归。
- 不读取、不修改用户真实 Ghostty 配置；临时配置前后哈希必须一致。
