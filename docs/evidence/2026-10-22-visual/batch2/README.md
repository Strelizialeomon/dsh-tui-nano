# 视觉对齐第二批 · 真实终端证据

- 日期：2026-10-22
- 分支：`issue-38`（第二批）
- 终端：Ghostty 1.3.1，macOS，深/浅两套调色板（深色 `#17191d` / `#e8e6e0` / accent `#7da1de`；浅色 `#f6f3ed` / `#343945` / accent `#365f9d`）
- 列宽：100 列 × 35 行、60 列 × 30 行
- 路径：从本分支构建 `target/release/nano`；合成 `HOME` / `DSH_HOME` / `NANO_DATA_ROOT`，官方 `dsh 0.2.1-rc.2` 真实宿主
- 模型：本机 `127.0.0.1` Messages SSE mock，不产生模型费用；截图为 `screencapture -l` 捕获的真实 Ghostty 窗口
- 输入：AppleScript 粘贴/按键注入；窗口标题里的数字是本次抓图的 run id

## 场景与步骤

| 图 | 操作步骤 | 覆盖判据 |
| --- | --- | --- |
| `dark-100-question.png` | 100 列深色启动，发送「显示一个问卷卡片，确认部署目标。」，mock 返回 `ask_user_question` | 决策卡顶部 accent 线、无外框；题目加粗；单选不显示 `[ ]`，编号 + `>` focus 指针；描述另起一行缩进 5ch、只渲染一次；底部只留键位提示；活动行「等待你的选择」在卡片与底栏之间 |
| `dark-60-question.png` | 同上，60 列 | 60 列下卡片不横向溢出；页脚、选项与活动行完整 |
| `light-60-question.png` | 同上，60 列浅色 | 浅色主题下卡片、focus、活动行仍可读 |
| `dark-100-diff.png` | 100 列深色，mock 返回 `edit`（文件未先读，官方拒绝应用） | `▸ Edit path` + 右侧 `+1 -1`（绿/红）；状态行 muted「未应用」；4ch 行号空槽；增删整行底色与行内强调；深色 token |
| `light-60-diff.png` | 同上，60 列浅色 | 浅色 diff token：新增底 `#dcebdd`、删除底 `#f1dcdf`、新增词底 `#b6d6bc`、删除词底 `#e9bac3`；浅色终端下不出现深色块 |
| `dark-100-jump.png` | 100 列深色，mock 返回 40 行长文本，PageUp 上滚后截图 | transcript 右下出现「回到最新」按钮；按钮只在离开底部时出现；`Ctrl+End` 仍可用（单测覆盖） |
| `dark-60-completion.png` | 60 列深色，空输入键入 `@` | 补全弹层沿用现有槽位；2ch 边距；标题与候选行完整；现有选中色（黑底青字）不变 |

## 未覆盖 / 延期项

- **审批卡**：合成 profile 的沙箱/审批策略会自动放行工具调用（工作区外写入、网络访问都直接执行），未产生官方 `approval/request`，因此没有真实审批卡截图。渲染由 `ui.rs` 单测 `an_approval_card_uses_the_bash_question_and_shows_the_full_command` 覆盖。
- **计划审阅卡**：官方 `ask_user_question` 工具 schema 会剥掉 `intent`，mock 传入的 `plan-review` 被降级成普通问卷，未取得计划审阅截图；计划审阅结构沿用 S03，低严重度。
- **hover**：未实现真实 `MouseEventKind::Moved` 悬停底色；保留现有聚焦/选中底色。spec §4.3 允许不做，但需在此标注。
- **队列**：沿用现有槽位，未单独截图；低严重度。
- **子代理 dock / 详情**：第一批证据已覆盖，本批未改动，未重复抓图。
- **真实输入法、选择复制等**：仍属 S13 发布验收缺口，不在本批范围。
