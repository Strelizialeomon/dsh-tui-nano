# design-contract 符合性核对（0.1.0）

对照 `docs/release/design-contract.md` 逐项核对。判据只认代码、测试和真实终端证据；未测项不勾。

| # | 契约项 | 判据 | 结果 |
| --- | --- | --- | --- |
| 1 | 布局槽位顺序：标题 1 / 主视图 Min 3 / 队列 / 补全 / dock / 输入 / 底部 1 | `crates/nano-app/src/ui.rs` 主布局 | 符合 |
| 2 | 60/100 列不横向溢出、槽位顺序不变 | Ghostty 60/100 截图 | 符合：`07-light-60-main.png`、`01-dark-100-main.png`；`--window-save-state=never` 保证真实列宽 |
| 3 | 输入框上下单单元格上下文线，80% warning / 95% error | `ui.rs` 输入框绘制 + Ghostty 截图 | **不符合**：没有占比线；标题行 `ctx` 使用 60/80 阈值（`01`/`07`/`09` 截图可见只有普通四边框） |
| 4 | 底部 cwd + 权限 + 瞬态 | `app.status_line()` | 符合 |
| 5 | 4ch 行号槽 + 2ch 标记槽 | `nano-transcript/src/render.rs` | 符合 |
| 6 | 8 行差异预算 | `DIFF_BUDGET = 8` + 单测 | 符合 |
| 7 | 命令标题 1 行、运行尾 2 行 | `COMMAND_BUDGET = 1`、渲染预算 | 符合 |
| 8 | 附件 3 张 + 溢出提示 | `attachment_lines()` | 符合 |
| 9 | 子代理 dock 4 + 1 溢出 | `nano-subagents` dock + 单测 | 符合 |
| 10 | 子代理灯 800ms、completed 绿 / aborted 黄 / error 红 / inactive muted | `lifecycle.rs` + 单测 | 符合 |
| 11 | 主会话约 500ms 忙碌指示 | `ui.rs` 标题行 + Ghostty 截图 | **不符合**：只有静态状态文字，无周期指示 |
| 12 | reduced-motion：`NANO_REDUCED_MOTION=1|true`，灯与忙碌指示静止 | `reduced_motion_env()` + `ui.rs:600` | 部分符合：灯静止；忙碌指示不存在，无从静止 |
| 13 | 装饰性 emoji 0 个 | UI 符号清单 | 符合 |
| 14 | 深/浅两套主题 token | diff 底色 | **不符合**：diff 底色硬编码深色；浅色终端无对应 token |
| 15 | 状态灯/错误/警告只用角色色 | `ui.rs` | 符合（ANSI 角色映射） |
| 16 | 键位无冲突、覆盖层不泄漏 | 键位表 + 单测 + Ghostty 按键 | 符合：会话/技能/规矩/任务/调度/命令面板均用真实按键打开，`06b` 滚轮改变视图，Ctrl+C 干净退出 |

结论：16 项中 12 项符合、1 项部分符合、3 项不符合。三项不符合均为发布阻断项，不在 S13 内修复。
