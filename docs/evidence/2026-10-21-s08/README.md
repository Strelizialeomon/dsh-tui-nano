# S08 证据（2026-10-21）

## 已执行

- `cargo test --workspace`：nano-app 34 项、nano-conversation 51 + 契约 5 项、nano-jobs 契约 8 项、nano-runtime 单元 18 + 契约 10 + preflight 2 项全绿；nano-runtime 的 15 项真实宿主回归默认 ignored。
- `npm run test:bridge`：87 项通过，含 shell 服务 12 项、jobs 服务 9 项与协议契约夹具。
- `NANO_E2E=1 node --test bridge/test/integration/host.test.mjs`：3 项通过。真实宿主验证 `!` 的 per-call `danger-full-access` 策略、`<bash-input>/<bash-stdout>/<bash-stderr>` 注入、`bash-mode` source、不唤醒驱动（状态保持 idle）、不分配 job ID、每命令新 shell 且 cwd 固定；`nano.jobs.*` 的空列表、非消费 follow 与 not-found 路径。输出见 `host-e2e.txt`。
- `scripts/pty-evidence-s08.sh`：真实 PTY（tmux 150×45）启动真实 dsh profile。首次 `!` 显示「用户权限、不是 Agent 授权」并要求二次 Enter；退出码 0/7、stderr 分节、运行中状态、任务面板（官方 jobs 空列表）与 Ctrl+C 干净退出可见。快照见 `manual-shell-terminal.txt`。
- jobs 观察层用官方投影夹具覆盖：`job/registered` 的所属调用关联、`jobs.event` 的 registered/progress/stopping/settled/removed、`readAt` 非消费 offset、`lossy`/`gapBefore` 缺口标记、ring 裁剪、`requested`/`already-finished` 与 not-found 区分、人类停止原因固定为 `cancelled by the user`。
- **真实模型会话（用户授权，deepseek-official / deepseek-flash）**：模型用 bash 工具在后台启动 `sh -c 'echo s08-start; sleep 40; echo s08-end'`；S08 通过官方投影看到 `bash-9a4d…` 运行中，`readAt(0)` 非消费读到 `s08-start`，`nano.jobs.kill` 返回 `requested`，官方终态 `killed` + `signal: SIGTERM; cancelled by the user`，`cause: kill`、`awaited: false`；随后**恰好一条**模型侧完成通知，带人工停止原因。输出见 `real-model-jobs.txt`。该会话使用用户真实 `~/.dsh` 凭据（未复制），profile 安装到 `~/.dsh/profiles/nano-s08-evidence-*` 并在退出后删除；数据根与 cwd 是临时目录。证据运行把 `tool-jobs` 的 `completionDelivery` 设为 `quiet`，避免再开一个 wake 模型回合；通知渠道本身仍是官方 `dsh-tool-jobs`。

## 未执行（不得勾选）

- **前台命令超时转后台（promotion）**：真实会话只验证了模型显式 `run_in_background` 的后台任务；「前台命令超时后仍是同一 job ID、同一份 ring」与 `promoteOnTimeout: false`、缺 registry、容量拒绝三条退回 deadline kill 的路径没有用真实模型分别构造。S08 只观察官方投影，这三条属于 `dsh-tool-bash` 的行为。
- **自然完成 / already-finished 的真实终态**：真实会话用人工停止构造了 `killed`；`completed` 与「停止一个已结束任务返回 already-finished」由投影夹具覆盖，未再花一次真实回合。
- **真实任务列表的有输出多任务并排**：真实会话只有一个后台任务；多任务排序、进度行与 200 条上限只有夹具覆盖。
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
# 真实模型证据（会使用用户凭据并产生费用；需用户授权）
NANO_EVIDENCE_MODEL=deepseek-flash node scripts/model-evidence-s08.mjs
```

PTY 脚本使用临时 `DSH_HOME` 与数据根，不读写用户的 `~/.dsh` 会话或凭据。模型证据脚本在真实 `DSH_HOME` 下安装一个临时 profile、用后删除，不复制凭据，也不改动已有 profile。
