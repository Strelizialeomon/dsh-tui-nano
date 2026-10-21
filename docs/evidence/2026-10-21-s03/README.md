# S03 证据（2026-10-21）

## 已执行

> 本目录的真实截图采集于 S03 与 S04-S08 合并后的代码；S11 随后合入，只新增调度面板/能力，S03 transcript 与决策卡路径未改。S11 合入后已复跑全量 Rust/桥接/真实宿主/E2E/PTY 测试，计数为 425 / 198 / 14 / 3 / 8。

- `cargo test`：425 项通过（含 S03 新增 crate 与 S04-S11 既有 crate；S11 合入后复跑）。
- `node --test bridge/test/*.test.mjs`：198 项通过（含决策宿主 15 项、S04-S11 既有桥接测试）。
- `NANO_E2E=1 node --test bridge/test/integration/host.test.mjs`：3 项真实宿主通过。
- `cargo test -p nano-runtime --test integration -- --ignored --test-threads=1 --skip shutdown_budget_is_bounded_when_cleanup_hangs`：14 项真实宿主回归通过，1 项慢测试跳过；能力快照由 `approval/userQuestions/tool-ask-user: unavailable` 更新为 `supported`（S03 已注册唯一 answerer）。
- `python3 scripts/pty-evidence.py`：真实 PTY（120×40）启动真实 dsh profile；空提交、括号粘贴、提交回显、失败回合、干净退出 8 项全过（S11 合入后复跑）。
- 真实模型会话 1（用户授权，deepseek-official / deepseek-chat）：
  - 模型真实调用 `ask_user_question`，问卷卡进入输入位；按 `1` 直选「聚焦回归（推荐）」，官方工具结果返回该答案。
  - 模型随后真实调用 `write` 创建 `proof.txt`；工具卡显示 `+2`、`已应用`、两行新增内容；文件存在。
  - 屏幕：`session-1-question.txt`（实时问卷卡）、`session-1-diff.txt`（实时差异）；最终 UI 用免费 resume 复核：`session-1-resumed-final.txt`。
- 真实模型会话 2（用户授权，同一路由）：
  - 模型真实调用 `write` 并带 `sandbox_permissions: danger-full-access`；官方产生 `approval/asked`，审批卡进入输入位，默认焦点在「拒绝」。
  - 按 `1` 选「允许一次」再 Enter；官方审计落 `approval/decided: allowed-once`，write 成功，越权目标文件存在。
  - 屏幕：`session-2-approval.txt`（实时审批卡）、`session-2-result.txt`（实时结果）；最终 UI resume：`session-2-resumed-final.txt`。
- 免费 resume 复核（无模型调用）：
  - S02 真实 bash 会话：命令卡显示真实命令、输出与「退出码未知」，不把缺失退出码补成 0：`s02-bash-resumed-final.txt`。
  - 另一份 S02 真实中断会话：`sleep 20` 被用户取消，工具卡显示「失败」、`Error: tool call aborted`、「退出码未知」，随后是「回合已取消」：`aborted-tool-resumed.txt`。
  - 已结束问卷只读回看：点击工具行打开「问卷回看 · 只读」，显示问题与已选答案，Esc 关闭；120/60 列：`readonly-120-final.txt`、`readonly-60-final.txt`。
  - 100/60 列布局：`resume-100.txt`、`resume-60.txt`。
  - 鼠标：拖选不触发展开；单击上下文块展开（屏幕内容变化）；滚轮上滚后头部出现「已上滚」，滚回底部后吸底恢复：`mouse-120.txt`。
- 真实会话原始事件摘录（已剔除系统指令、运行时上下文和其他注入消息）：`real-session-events.txt`。
- 真实文件内容：`proof.txt`、`outside.txt`。

## 实测与审核中发现并已修复的展示/状态问题

- 审批 pending 时工具行曾显示「结果未知」；现在区分「审批中 / 结果未知 / 已结算」。
- 助手消息里的 `tool-call` 内容块曾显示成 `[tool-call]`；现在不渲染该占位，工具卡承载工具身份。
- 注入上下文曾整段铺开；现在默认一行摘要 + 隐藏行数，点击展开，原文不变。
- 只有工具调用的助手消息曾留下空的「助手」标签；现在不渲染空块。
- 思考行折叠态曾误显示「收起」；现在折叠显示「展开」、展开显示「收起」。
- 差异行曾只有前景色；现在有增删底色，配对的删/增行有行内强调。
- 审批/问卷卡片的点击表曾把底边框映射到被裁掉的第一行；现在点击命中随文本裁剪。
- Other 行曾无法输入空格和数字；现在 Other 聚焦时空格/数字都作为文本。
- continued 答案提交后曾无法重新武装；现在 `answer()===false`、`not-continued`、被丢弃的队列答复都会把卡片放回输入位。
- 审批信号中止/退出曾自动回答 cancelled；现在退出不回答未决请求，turn 中止只发 `withdrawn`，不伪造决定。
- 流式预览被正式消息替换时阅读锚点曾丢失；现在按 turn/step 建立别名，锚点跟随新消息身份。
- 未知工具结果曾显示成「已应用」差异；现在显示「差异未知」，不声称文件状态。
- 空的 `write` 曾显示 `+1` 空行；现在 `+0`。

## 未执行（不得勾选）

- 限时问卷的真实模型路径：profile 默认 `tool-ask-user` 为 blocking；`mode: timed` 需在 active preset 配置。timed 的 claim、`ASK_TIMED_OUT`、continued `userQuestions/answer`、投影恢复由单测/桥接测试覆盖，未跑真实模型终端。
- Ghostty 原生滚轮曲线、IME 组合态、原生选择复制：PTY 证据是 xterm 类终端；滚轮曲线与滚动状态有单测，真实 Ghostty 需人工验收。
- 真实 `edit` 工具的逐行坐标：官方 `FileDiff` 没有 patch 坐标，本份按规范留空 4ch 行号槽；真实会话只覆盖 `write` create（meta 的 `diffs` 为空，按调用参数本地计算 +N），`edit` 解析与行内强调由单测覆盖。
- 真实审批的拒绝/取消/不可用终端操作：桥接测试覆盖四种 outcome、撤回、过期与退出不自动答；真实会话只操作了「允许一次」。
- 真实计划审阅（`plan-review` 意图）：卡片与两种动作有单测；profile 未装载 plan-mode 真实请求。
- S12 子代理挂载与跨地址导航：D04 只读/可交互接口已导出并有契约测试；真实子对话归 S12。
- 深浅主题的终端配色对比：UI 正文使用终端默认色，强调色固定；生产主题 token 由 S13 design-contract 冻结。PTY 快照不包含颜色。
- 同一消息内多个过程块的「共享预算」：当前实现为每块独立预算；该 spec 规则未实现，已在 spec 审核修订记录中标注。

## 证据卫生说明

- `results.json` 是首轮两次会话的合并输出：session-1 全绿，session-2 首次因另一个并发会话覆盖了共享 dsh profile 而未连接；重跑结果在 `results-2.json`，审批结果失败的原因与修复见该文件及 `real-session-events.txt`。
- `session-1-question.txt`、`session-1-diff.txt`、`session-2-approval.txt`、`session-2-result.txt` 是修复前的实时 PTY 文本快照，含少量 ANSI/光标残留；保留它们是为了证明真实请求/答复，最终 UI 以 `*-resumed-final.txt` 为准（pending 标签、空助手占位、上下文降噪和差异底色均由最终代码/单测覆盖）。
- 真实会话使用临时数据根与临时 cwd；profile 安装到 `~/.dsh/profiles/dsh-tui-nano`，未复制凭据。会话 ID：
  - session-1：`session-117c94b4-7506-4527-b65c-5f73671ec5b2`
  - session-2：`session-697c58b0-72c1-4ec5-bdfd-1e38f93fdae7`
  - S02 bash 复核：`session-a49b2891-f1a2-4d24-8ec2-5066f115061f`
  - S02 中断复核：`session-39777d81-1264-41df-b521-c3671acf04b8`

## 复现

```sh
cargo test
node --test bridge/test/*.test.mjs
NANO_E2E=1 node --test bridge/test/integration/host.test.mjs
cargo test -p nano-runtime --test integration -- --ignored --test-threads=1 \
  --skip shutdown_budget_is_bounded_when_cleanup_hangs
cargo build -p nano-app
python3 scripts/pty-evidence.py
```

真实模型会话需要用户授权与官方凭据；本次使用临时数据根与临时 cwd。

## 边界数字

差异 8 行、命令 1 行、输出 2 行、思考 1 行、失败摘要 3 行、上下文 1 行、overscan 80 行、挂载上限 300 行等是 design-contract / Claude Code 参照值，不是本机 Ghostty 的耗时实测；滚动与挂载有单测和真实 PTY 复核，未做帧耗时测量。
