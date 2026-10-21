# S03 证据（2026-10-22）

## 已执行

- `cargo test`：193 项通过（nano-app 32、nano-conversation 51 + 契约 5、nano-decisions 30、nano-runtime 18 + 契约 10 + preflight 2、nano-transcript 37 + 契约 8）。
- `node --test bridge/test/*.test.mjs`：80 项通过，含新增决策宿主 12 项、协议夹具与能力快照。
- `NANO_E2E=1 node --test bridge/test/integration/host.test.mjs`：2 项真实宿主通过。
- `cargo test -p nano-runtime --test integration -- --ignored --test-threads=1 --skip shutdown_budget_is_bounded_when_cleanup_hangs`：14 项真实宿主回归通过，1 项慢测试跳过；能力快照由 `approval/userQuestions/tool-ask-user: unavailable` 更新为 `supported`（S03 已注册唯一 answerer）。
- `python3 scripts/pty-evidence.py`：真实 PTY（120×40）启动真实 dsh profile；空提交、括号粘贴、提交回显、失败回合、干净退出 8 项全过。
- 真实模型会话 1（用户授权，deepseek-official / deepseek-chat）：
  - 模型真实调用 `ask_user_question`，问卷卡出现在输入位；按 `1` 直选「聚焦回归（推荐）」后，官方工具结果返回该答案。
  - 模型随后真实调用 `write` 创建 `proof.txt`；工具卡显示 `+2`、`已应用`、两行新增内容；文件存在。
  - 屏幕：`session-1-question.txt`（实时问卷卡）、`session-1-diff.txt`（实时差异）；最终 UI 用免费 resume 复核：`session-1-resumed-final.txt`。
- 真实模型会话 2（用户授权，同一路由）：
  - 模型真实调用 `write` 并带 `sandbox_permissions: danger-full-access`；官方产生 `approval/asked`，审批卡出现在输入位，默认焦点在「拒绝」。
  - 按 `1` 选「允许一次」再 Enter；官方审计落 `approval/decided: allowed-once`，write 成功，越权目标文件存在。
  - 屏幕：`session-2-approval.txt`（实时审批卡）、`session-2-result.txt`（实时结果）；最终 UI resume：`session-2-resumed-final.txt`。
- 免费 resume 复核（无模型调用）：
  - S02 真实 bash 会话：命令卡显示真实命令、输出与「退出码未知」，不把缺失退出码补成 0：`s02-bash-resumed-final.txt`。
  - 已结束问卷只读回看：点击工具行打开「问卷回看 · 只读」，显示问题与已选答案，Esc 关闭；120/60 列：`readonly-120-final.txt`、`readonly-60-final.txt`。
  - 100/60 列布局：`resume-100.txt`、`resume-60.txt`。
  - 鼠标：拖选不触发展开；单击上下文块展开（屏幕内容变化）；滚轮上滚后头部出现「已上滚」，滚回底部后吸底恢复：`mouse-120.txt`。
- 真实会话原始事件摘录（已剔除系统指令、运行时上下文和其他注入消息）：`real-session-events.txt`。
- 真实文件内容：`proof.txt`、`outside.txt`。

## 实测中发现并已修复的展示问题

- 审批 pending 时工具行曾显示「结果未知」；现在区分「审批中 / 结果未知 / 已结算」，单测覆盖。
- 助手消息里的 `tool-call` 内容块曾显示成 `[tool-call]`；现在不渲染该占位，工具卡承载工具身份。
- 注入上下文曾整段铺开；现在默认一行摘要 + 隐藏行数，点击展开，原文不变。
- 只有工具调用的助手消息曾留下空的「助手」标签；现在不渲染空块。

## 未执行（不得勾选）

- 限时问卷的真实模型路径：profile 默认 `tool-ask-user` 为 blocking；`mode: timed` 需在 active preset 配置。timed 的 claim、`ASK_TIMED_OUT`、continued `userQuestions/answer`、投影恢复由单测/桥接测试覆盖，未跑真实模型终端。
- Ghostty 原生滚轮曲线、IME 组合态、原生选择复制：PTY 证据是 xterm 类终端；滚轮曲线与滚动状态有单测，真实 Ghostty 需人工验收。
- 真实 `edit` 工具的逐行坐标：官方 `FileDiff` 没有 patch 坐标，本份按规范留空 4ch 行号槽；真实会话只覆盖 `write` create（meta 的 `diffs` 为空，按调用参数本地计算 +N），`edit` 解析由单测覆盖。
- 真实审批的拒绝/取消/不可用终端操作：桥接测试覆盖四种 outcome 与撤回/过期；真实会话只操作了「允许一次」。
- S12 子代理挂载与跨地址导航：D04 只读/可交互接口已导出并有契约测试；真实子对话归 S12。
- 深浅主题的终端配色对比：UI 正文使用终端默认色，强调色固定；生产主题 token 由 S13 design-contract 冻结。PTY 快照不包含颜色。

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

真实模型会话需要用户授权与官方凭据；本次使用临时数据根与临时 cwd，profile 安装到
`~/.dsh/profiles/dsh-tui-nano`，未复制凭据。会话 ID：

- session-1：`session-117c94b4-7506-4527-b65c-5f73671ec5b2`
- session-2：`session-697c58b0-72c1-4ec5-bdfd-1e38f93fdae7`
- S02 bash 复核：`session-a49b2891-f1a2-4d24-8ec2-5066f115061f`

## 边界数字

差异 8 行、命令 1 行、输出 2 行、思考 1 行、失败摘要 3 行、overscan 80 行、挂载上限 300 行等是 design-contract / Claude Code 参照值，不是本机 Ghostty 的耗时实测；滚动与挂载有单测和真实 PTY 复核，未做帧耗时测量。
