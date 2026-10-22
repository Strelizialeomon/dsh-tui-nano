# ADR-20261022-nano-theme-scope：`NANO_THEME` 同时管 diff token 与 dock 焦点底色

- 状态：已被取代
- 日期：2026-10-22
- 决策人：用户
- 适用范围：`NANO_THEME=dark|light` 影响的视觉 token
- 取代：`2026-10-22-nano-visual-v1` §9「`NANO_THEME` 只影响 diff 底色与行内强调，不影响其他 token」
- 被取代：[ADR-20261022-independent-tui-theme](2026-10-22-independent-tui-theme.md)
- 决策来源：`docs/specs/2026-10-22-nano-visual-followup/README.md` §3.2；用户 2026-10-22 选择「完全按设计稿」

## 决定

- `NANO_THEME` 继续选择 diff 的深/浅 token。
- 同一变量也决定子代理 dock 焦点行的 hover 底色：`dark` 用 `#242b35`，`light` 用 `#e8e8e5`。
- 其他 token 仍由终端主题决定；本决定不扩大 `NANO_THEME` 到正文、边框、状态灯等角色。

## 依据

- 设计稿的聚焦行使用 `--hover`：深色 `#242b35`、浅色 `#e8e8e5`。
- 视觉第二批已在代码中读取 `NANO_THEME` 选择 diff token；dock 复用同一读取，不新增第二个环境变量。

## 否决了什么

- dock 焦点行继续用反显或固定高对比选中色。
- 为 dock 新增独立主题变量。
- 让 `NANO_THEME` 接管所有 token。

## 接受的代价

- `NANO_THEME` 的适用范围比旧 spec 宽一项；若终端主题与 `NANO_THEME` 不一致，dock 焦点行可能与终端整体观感不一致。
- 浅色 hover 在深色终端上会显得突兀；这是手动选择主题的预期代价。

## 允许重开的条件

- 终端自动主题探测落地，手动 `NANO_THEME` 被替代。
- 设计稿更换 hover token。
- 发现 `NANO_THEME` 与终端主题混用时产生不可读的焦点行。

## 关联

- spec：`docs/specs/2026-10-22-nano-visual-followup/README.md`
- issue：#45
- 相关：`ADR-20261022-subagent-dock-lifecycle`
