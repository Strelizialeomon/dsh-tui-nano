# 视觉与输入反馈收尾 · 证据

- 日期：2026-10-22
- 分支：`docs/followup-evidence`（实施分支 `issue-45` 已合并为 `77ba43d`）
- 二进制：本分支 `target/debug/nano`
- 宿主：官方 `dsh 0.2.1-rc.2`；合成 `HOME` / `DSH_HOME` / `NANO_DATA_ROOT`
- 模型：本地 Messages mock（`127.0.0.1`），不产生模型费用
- 关联：spec `docs/specs/2026-10-22-nano-visual-followup/README.md`；issue #45

## 输入历史（真实 PTY）

- 命令：`python3 scripts/pty-evidence-history.py`
- 原始输出：`history-pty.txt`
- 覆盖：连续提交两条 prompt；上键回看第二条、再回看第一条；下键回到第二条、再回到空草稿；`history.jsonl` 存在、0600、两条记录、`project` 为 canonical cwd。
- 真实键盘事件（PTY 写入），不是测试替身。

## 子代理 dock（真实 PTY 运行态）

- 命令：临时 PTY driver，复用 `scripts/ghostty-evidence-followup.py` 的同一 mock 协议
- 原始输出：`dock-running-pty.txt`
- 覆盖：mock 子代理启动后，dock 行显示 `● followup-tui … 执行中`；无标题、无用时。

## 自动化

| 文件 | 命令 | 结果 |
| --- | --- | --- |
| `cargo-test.txt` | `cargo test` | 全部通过 |
| `bridge-test.txt` | `node --test bridge/test/*.test.mjs` | 272 通过、0 失败 |

新增单测覆盖：dock 无标题/状态词/焦点指针/列表计数、30 秒清理（29 秒保留、30 秒后隐藏、start 清除 hidden）、标题下 1 行留白、预览缺口/超长文案与 60 列换行、历史文件 0600/项目过滤/当前会话优先/100 条上限、上键回看与下键恢复草稿、子代理历史只取 `MessageKind::User`、`NANO_THEME` 深浅 hover 色。

## Ghostty 截图

| 文件 | 覆盖 |
| --- | --- |
| `01-main-100-dark.png` | 100 列空态：标题 `nano` + 路径，随后 1 行留白 |
| `02-dock-completed-100-dark.png` | 100 列，完成后 30 秒内，dock 状态词 `完成`，无标题/无用时 |
| `04-dock-cleaned-100-dark.png` | 100 列，30 秒后 dock 行已消失 |
| `05-main-60-dark.png` | 60 列空态：标题与留白不溢出 |
| `06-dock-60-dark.png` | 60 列，dock 槽位与状态词 |
| `07-dock-focus-100-light.png` | `NANO_THEME=light`，dock 焦点行 hover `#e8e8e5` |

- `01` / `05` 由无键盘的 Ghostty 启动脚本捕获：安装合成 profile、启动 `nano`、12 秒后 `screencapture -l`，不发送任何按键。
- `02` / `04` / `06` / `07` 取自 18:37 的 run；该 run 通过 System Events 驱动真实键盘。之后按轻审修订了历史扫描、粘贴/队列编辑与 dock 边框色；dock 行文案、状态词与 30 秒清理行为与最终代码一致，边框色以最终代码（DarkGray）为准。
- 截图只捕获本次运行拥有的 Ghostty 窗口；剪贴板已恢复。
- 本机当前 `osascript` 辅助功能权限被拒，`scripts/ghostty-evidence-followup.py` 的键盘驱动段无法重跑；脚本保留完整流程，权限恢复后可重新生成 `01`–`07`。

## 未覆盖 / 注意

- **100 列运行中 dock（Ghostty）**：由 `dock-running-pty.txt` 的真实 PTY 屏幕覆盖；Ghostty 100 列保留的是完成态与清理后。
- **60 列运行/完成拆分**：只保留一张 dock 截图（`06`），未分别截运行与完成。
- **历史跨进程回看**：由单测与真实 PTY 文件检查覆盖；未做第二次启动后的 Ghostty 截图。
- **子代理输入历史**：由 `child_history_uses_only_user_messages_from_the_child_projection` 单测覆盖；未做真实 PTY 子代理输入回看。
- 浅色证据只覆盖 dock 焦点行；第二批的 diff 浅色证据在 `docs/evidence/2026-10-22-visual/batch2/`。
