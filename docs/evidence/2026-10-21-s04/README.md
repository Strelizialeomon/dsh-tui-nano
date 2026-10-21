# S04 证据（2026-10-21）

## 已执行

- `cargo test --workspace`：nano-controls 23 项、nano-app 39 项、nano-conversation 52 项 + 契约 5 项、nano-runtime 18 项 + 契约 10 项 + preflight 2 项，全绿；15 项真实宿主集成测试保持 `--ignored`。
- `node --test bridge/test/*.test.mjs`：83 项通过，含控制服务 18 项与协议契约。
- 真实 PTY（120×40，macOS）：`python3 scripts/pty-controls-evidence.py` 21 项全通过。使用用户真实 `~/.dsh` 凭据（未复制），数据根与 cwd 是临时目录。用户在会话中授权了一次极小的真实请求（提示「只回复 ok」）。快照与官方日志摘录见 `controls-session.txt`。
- 真实官方事实：
  - 模型目录按 provider 分组（`DeepSeek`），列出 `DeepSeek-V4.1-Flash` 与 `DeepSeek-V4-Pro`；前者声明 `Off / Low / High / Max`，默认 `High`。
  - 在强度面板选择 `Low`，官方日志写入 `model/selection {provider: deepseek-official, model: deepseek-v4-pro, reasoningEffort: low}`；切回 Flash 后写入 `model/selection {model: deepseek-flash, reasoningEffort: high}`（官方解析出的默认强度）。
  - 权限面板显示 `read-only`、`workspace-write`、`danger-full-access`；选择 `danger-full-access` 后官方日志依次出现 `command/run {name: permission, args: " danger-full-access"}`、`permission/preset`、`sandbox/mode: danger-full-access`、`approval/policy: never`、`command/done {kind: success}`；切回 `workspace-write` 同样走 `/permission` 命令。
  - 两次切换产生的 `The approval policy changed from "ask" to "never" (changed by the user).` 与反向通知出现在官方 inbox（屏幕快照 `permission-switched` / `permission-restored`），证明走的是注入 changed-by-user 的官方命令路径，而不是 `permissionPresets.set()`。
  - 底部常驻行显示 `权限 workspace-write` / `权限 danger-full-access`；模型与强度只在 `/model` 面板内显示。
  - 在 `deepseek-v4-pro + low` 状态下发送「只回复 ok」：官方 `request/header` 的 `config` 为 `{provider: deepseek-official, model: deepseek-v4-pro, reasoningEffort: low}`，证明下一请求使用切换后的路由；`user/message`（`source.kind: user-approval`）携带 `The approval policy changed from "ask" to "never" (changed by the user).` 进入该回合，助手回复 `ok`，`turn/end` 为 `completed`。

## 未执行（不得勾选）

- 「运行中的步骤保持原路由」：真实请求只验证了空闲后的下一请求；没有在回合运行中切换模型再验步骤内路由。官方语义是步骤开始时组装并保留该路由，桥接钩子只改下一次组装。
- 模型变更提示（`[model changed: ...]`）：本次真实会话的首个请求没有更早的请求头，官方钩子按设计不注入提示；提示逻辑由桥接单测覆盖（provider/model 变化才注入，纯强度变化不注入），未在第二个真实请求上复验。
- 第二个窗口写同一根会话：真实互斥由 S01 的会话挂载账本承担；本份用桥接单测覆盖 `session/writer-held` 映射与「不接管」文案，未再跑双窗口。
- 第三方 provider 的真实目录/凭据退出：本份只验证 `deepseek-official`；provider 失败在真实面板中按官方 `failures` 显示为不可选原因，单位测试覆盖。
- 默认值保存失败：官方 `saveSelection` 只写后台告警，界面回执明确只确认会话内选择；未注入真实写盘失败。
- 无 reasoning 元数据的真实模型：单位测试覆盖「没有强度行、不能输入任意值」；本次两个真实模型都带强度元数据。
- Ghostty 全屏下的最终键位：`Ctrl+P`、`/model`、`/permission` 由本份暂定，S13 的 design-contract 冻结。

## 边界数字

`MAX_COMMAND_LINE_BYTES` 4096 是命令面板/输入路由的单行保护，不是官方限制；官方命令本身按会话日志与帧上限处理。模型目录在 Rust 侧按 provider 64 组、每组 256 个模型截断，权限选项 64 个、命令 256 条，均只为显示上限，超出部分不影响官方事实。

## 复现

```sh
cargo build -p nano-app
# 安装/更新 Nano profile（真实 dsh home）
node bridge/profile/install.mjs --dsh-home "$HOME/.dsh" \
  --data-root /tmp/nano-s04-install --bridge-package "$PWD/bridge" \
  --dsh-bin "$(which dsh)" --json
python3 scripts/pty-controls-evidence.py
```

脚本使用临时数据根与 cwd，不读写用户的会话；模型与权限选择本身不发请求，只有「只回复 ok」那一步会产生一次极小的真实模型调用。并行工作树共用 `~/.dsh/profiles/dsh-tui-nano`，跑证据前需从本工作区重新安装 profile。
