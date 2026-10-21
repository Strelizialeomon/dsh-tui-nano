# S08 证据（2026-10-21）

## 已执行

- `cargo test --workspace`：nano-app 34 项、nano-conversation 51 + 契约 5 项、nano-jobs 契约 8 项、nano-runtime 单元 18 + 契约 10 + preflight 2 项全绿；nano-runtime 的 15 项真实宿主回归默认 ignored。
- `npm run test:bridge`：87 项通过，含 shell 服务 12 项、jobs 服务 9 项与协议契约夹具。
- `NANO_E2E=1 node --test bridge/test/integration/host.test.mjs`：3 项通过。真实宿主验证 `!` 的 per-call `danger-full-access` 策略、`<bash-input>/<bash-stdout>/<bash-stderr>` 注入、`bash-mode` source、不唤醒驱动（状态保持 idle）、不分配 job ID、每命令新 shell 且 cwd 固定；`nano.jobs.*` 的空列表、非消费 follow 与 not-found 路径。输出见 `host-e2e.txt`。
- `scripts/pty-evidence-s08.sh`：真实 PTY（tmux 150×45）启动真实 dsh profile。首次 `!` 显示「用户权限、不是 Agent 授权」并要求二次 Enter；退出码 0/7、stderr 分节、运行中状态、任务面板（官方 jobs 空列表）与 Ctrl+C 干净退出可见。快照见 `manual-shell-terminal.txt`。
- jobs 观察层用官方投影夹具覆盖：`job/registered` 的所属调用关联、`jobs.event` 的 registered/progress/stopping/settled/removed、`readAt` 非消费 offset、`lossy`/`gapBefore` 缺口标记、ring 裁剪、`requested`/`already-finished` 与 not-found 区分、人类停止原因固定为 `cancelled by the user`。

## 未执行（不得勾选）

- **真实后台任务与人工停止**：需要一个真实模型回合让 `bash` 工具跑后台命令（或让前台命令超时转后台），再在任务面板观察并停止。这会使用用户凭据并产生费用，**等用户授权后补测**；当前只有官方投影夹具与真实空列表证据。因此验收中的「前台命令超时仍是同一 job ID」「任务列表/进度与官方投影一致（有任务时）」「停止后终态与完成通知唯一渠道」尚未取得真实模型证据。
- **promotion 失败路径**：`promoteOnTimeout: false`、缺 registry、容量拒绝三条按官方 `dsh-tool-bash` 行为退回 deadline kill；S08 只观察投影，未用真实模型分别构造。
- **D08 负向闸**：`!` 输出不能作为审批授权。S08 侧夹具确认 source 是 `bash-mode` 而非 `user`，且不触碰 approval 服务；真实审批响应与权限切换由 S03/S04 的负向夹具在发布闸验证。
- **逃逸后代进程**：macOS 上 SIGKILL 后仍可能留下逃逸后代，S01 已按官方 `subprocess-local` 的限制标注，S08 不承诺追到所有后代。
- Ghostty 全屏下的人工键位验收归 S13 的 design-contract；当前键位（`!`、Ctrl+T）是 S08 的实现默认值。

## 边界数字

- `!` 注入前把合成消息的 JSON 量化到 200,000 字节以下（低于 S01 的 256 KiB 事件帧上限），必要时按流截断并标 `injected context truncated` + spill 路径；完整结果仍显示在本窗口，模型侧只拿到有界上下文。
- 任务列表最多显示 200 条官方投影；输出详情只保留一个 job 的 256 KiB 尾部；官方 ring 的 `retainBytes` 262144 / `settledRetainBytes` 16384 / `pumpPollMs` 150 是唯一输出预算来源。
- `nano.shell.exec` 的 Rust 等待上限 75s，对应官方 executor 的 60s deadline 加准备/终止余量；不是新的执行超时。

## 复现

```sh
cargo build -p nano-app
scripts/pty-evidence-s08.sh
NANO_E2E=1 node --test bridge/test/integration/host.test.mjs
```

PTY 脚本使用临时 `DSH_HOME` 与数据根，不读写用户的 `~/.dsh` 会话或凭据。真实模型/后台任务证据需要用户授权与模型路由（会使用官方凭据并产生费用）。
