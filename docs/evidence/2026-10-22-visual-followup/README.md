# 视觉与输入反馈收尾 · 证据

- 日期：2026-10-22
- 分支：`docs/followup-evidence2`（实施分支 `issue-45` 已合并为 `77ba43d`）
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

## 子代理输入历史（真实 PTY）

- 命令：`python3 scripts/pty-evidence-child-history.py`
- 原始输出：`child-history-pty.txt`
- 覆盖：进入子代理视图（gate `可输入`）→ 发送 `child history prompt` → 上键回看，composer 行恢复该提示；任务结束干净退出。

## 自动化

| 文件 | 命令 | 结果 |
| --- | --- | --- |
| `cargo-test.txt` | `cargo test` | 全部通过 |
| `bridge-test.txt` | `node --test bridge/test/*.test.mjs` | 272 通过、0 失败 |

新增单测覆盖：dock 无标题/状态词/焦点指针/列表计数、30 秒清理（29 秒保留、30 秒后隐藏、start 清除 hidden）、标题下 1 行留白、预览缺口/超长文案与 60 列换行、历史文件 0600/项目过滤/当前会话优先/100 条上限、上键回看与下键恢复草稿、子代理历史只取 `MessageKind::User`、`NANO_THEME` 深浅 hover 色。

## Ghostty 截图

命令：`python3 scripts/ghostty-evidence-followup.py`；运行检查见 `ghostty-evidence.txt`（10 项全 ok）。

| 文件 | 覆盖 |
| --- | --- |
| `01-main-100-dark.png` | 100 列空态：标题 `nano` + 路径，随后 1 行留白 |
| `02-dock-running-100-dark.png` | 100 列运行中：dock `● followup-tui … 执行中`，无标题/无用时 |
| `03-dock-completed-100-dark.png` | 100 列完成后 30 秒内：状态词 `完成` |
| `04-dock-cleaned-100-dark.png` | 100 列，30 秒后 dock 行已消失 |
| `05-main-60-dark.png` | 60 列空态：标题与留白不溢出 |
| `06-dock-60-dark.png` | 60 列运行中：dock 状态词 `执行中` |
| `07-dock-focus-100-light.png` | `NANO_THEME=light`，dock 焦点行 hover `#e8e8e5` |

- 截图只捕获本次运行拥有的 Ghostty 窗口；跑完已关闭，无遗留进程。
- 截图期间有并发键入，`01` / `02` 的输入行出现一个「和」；标题、留白、dock 行与状态词不受影响。

## 未覆盖 / 注意

- **历史跨进程回看**：由单测与真实 PTY 文件检查覆盖；未做第二次启动后的 Ghostty 截图。
- 浅色证据只覆盖 dock 焦点行；第二批的 diff 浅色证据在 `docs/evidence/2026-10-22-visual/batch2/`。
