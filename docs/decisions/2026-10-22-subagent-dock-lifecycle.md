# ADR-20261022-subagent-dock-lifecycle：dock 无标题，官方终态 30 秒后统一清理

- 状态：生效中
- 日期：2026-10-22
- 决策人：用户
- 适用范围：子代理 dock 的标题、行内容、状态词与完成行生命周期
- 取代：`2026-10-20-nano-v1` S12 的「可续聊子代理普通回合完成不清理」「dock 保留 N local agents/活动/用时」条款
- 被取代：无
- 决策来源：`docs/specs/2026-10-22-nano-visual-followup/README.md` §3.2；用户 2026-10-22 选择「完全按设计稿」「30 秒后消失」

## 决定

- 顶边框只保留一条线，不嵌 `N local agents` 或其他标题。
- 行内容：焦点指针（未聚焦空格、聚焦 `›`）、状态灯、名字（弹性）、右侧一个状态词；不显示用时、不显示 mode。
- dock 只显示本地直接子代理；external 行不进 dock，只在「全部子代理」列表显示。
- 状态词：`执行中` / `完成` / `终止` / `错误` / `未运行`；unknown 终态并入 `未运行`。
- 所有官方 `subagent/end`（含可续聊）都保留 30 秒后从 dock 移除；再次 `subagent/start` 清除 hidden。
- 计数移到「全部子代理」列表标题：`全部子代理 · N · ...`，N 只数本地直接子代理，不含 hidden/external。

## 依据

- 设计稿的 dock 没有标题和用时；用户要求完全按设计稿。
- Claude Code 的 `PANEL_GRACE_MS = 30_000`：完成行在面板保留 30 秒后淘汰。
- 用户反馈「subagent 完成后没消失」；原 S12 对 continuable 的永久保留被取代。

## 否决了什么

- 保留 `N local agents` 标题与用时（旧 S12 验收项）。
- 1.5 秒清理（设计稿样稿值；用户选择对齐 Claude Code）。
- continuable 完成行永久保留。
- 多显示「一次性/不可进入」mode 后缀；mode 只在全部列表显示。

## 接受的代价

- dock 上不再直接看到 agent 总数与用时；需要打开「全部子代理」。
- 完成行会占 30 秒；之后需从全部列表或父对话找回记录。

## 允许重开的条件

- 设计稿改版，重新引入 dock 标题或计时。
- 用户明确要求恢复 continuable 永久入口。
- 全部列表无法稳定打开，需要把计数放回 dock。

## 关联

- spec：`docs/specs/2026-10-22-nano-visual-followup/README.md`
- issue：#45
- 相关：`ADR-20261022-input-history`
