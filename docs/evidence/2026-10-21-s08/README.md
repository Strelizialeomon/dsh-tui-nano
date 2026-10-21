# S08 证据（2026-10-21）

## 已执行

- `cargo test --workspace`：nano-app 77 项（含 S08 的面板/错误/快照/follow 测试）、nano-conversation 52 + 契约 5、nano-controls 27 + 契约 2、nano-jobs 契约 13、nano-runtime 18 + 契约 10 + preflight 2、nano-skills 20 + 契约 5，全绿；nano-runtime 的 15 项真实宿主回归默认 ignored。
- `npm run test:bridge`：133 项通过（合并 S04/S05 后），含 shell 服务 13 项、jobs 服务 11 项与协议契约夹具。
- `NANO_E2E=1 node --test bridge/test/integration/host.test.mjs`：3 项通过。真实宿主验证 `!` 的 per-call `danger-full-access` 策略、`<bash-input>/<bash-stdout>/<bash-stderr>` 注入、`bash-mode` source、不唤醒驱动（状态保持 idle）、不分配 job ID、每命令新 shell 且 cwd 固定；`nano.jobs.*` 的空列表、非消费 follow 与 not-found 路径。输出见 `host-e2e.txt`。
- `scripts/pty-evidence-s08.sh`：真实 PTY（tmux 150×45）启动真实 dsh profile。首次 `!` 显示「用户权限、不是 Agent 授权」并要求二次 Enter；退出码 0/7、stderr 分节、运行中状态、任务面板（官方 jobs 空列表）与 Ctrl+C 干净退出可见。快照见 `manual-shell-terminal.txt`。
- jobs 观察层用官方投影夹具覆盖：`job/registered` 的所属调用关联、`jobs.event` 的 registered/progress/stopping/settled/removed、`readAt` 非消费 offset、`lossy`/`gapBefore` 缺口标记、ring 裁剪、`requested`/`already-finished` 与 not-found 区分、人类停止原因固定为 `cancelled by the user`。
- **真实模型会话（用户授权，deepseek-official / deepseek-flash）**：模型用 bash 工具在后台启动 `sh -c 'echo s08-start; sleep 40; echo s08-end'`；S08 通过官方投影看到 `bash-1e69…` 运行中，`readAt(0)` 非消费读到 `s08-start`，`nano.jobs.kill` 返回 `requested`，官方终态 `killed` + `signal: SIGTERM; cancelled by the user`，`cause: kill`、`awaited: false`；随后完成通知在 2 个观测帧（splice + inbox 投影）里出现，但 **message id 只有一个**，且带人工停止原因。输出见 `real-model-jobs.txt`。该会话使用用户真实 `~/.dsh` 凭据（未复制），profile 安装到 `~/.dsh/profiles/nano-s08-evidence-*` 并在退出后删除；数据根与 cwd 是临时目录。证据运行把 `tool-jobs` 的 `completionDelivery` 设为 `quiet`，避免再开一个 wake 模型回合；通知渠道本身仍是官方 `dsh-tool-jobs`。
- **真实 TUI + 真实模型（用户再次授权）**：在真实 `nano` 里让模型前台运行 `sh -c 'echo s08-promo; sleep 30'` 且 `timeoutMs: 1000`；官方工具结果出现 `still running after 1000ms; moved to background job bash-f6ff…`，同一 job id 随即出现在 Ctrl+T 任务面板（运行中，带所属调用），Enter 详情非消费读到 `s08-promo`，k 停止后显示官方终态 `被终止 · signal: SIGTERM; cancelled by the user`。这把 promotion 成功路径和 TUI 任务面板/详情/停止一起覆盖。输出见 `real-tui-promotion.txt`，官方会话日志摘录见 `promotion-session.txt`（同一 notice id `1468…` 同时出现在 `agent/inbox/spliced` 与 `user/message`，是同一通知的两种观测，不是两次投递）。临时 `DSH_HOME` 只符号链接真实凭据文件（未复制），运行前后 sha 校验一致；运行使用临时 profile，结束后删除。

## 未执行（不得勾选）

- **promotion 的三条失败路径**：`promoteOnTimeout: false`、缺 registry、容量拒绝按官方 `dsh-tool-bash` 行为退回 deadline kill。成功路径已由真实 TUI 覆盖；这三条需要额外配置的真实回合，尚未分别构造，S08 只观察官方投影。
- **自然完成 / already-finished 的真实终态**：真实会话用人工停止构造了 `killed`；`completed` 与「停止一个已结束任务返回 already-finished」由投影夹具覆盖，未再花真实回合。
- **真实多任务并排与进度行**：真实会话只有单个任务；多任务排序、progress 行与 200 条上限只有夹具覆盖。
- **D08 负向闸**：`!` 输出不能作为审批授权。S08 侧夹具确认 source 是 `bash-mode` 而非 `user`，且不触碰 approval 服务；真实审批响应与权限切换由 S03/S04 的负向夹具在发布闸验证。
- **逃逸后代进程**：macOS 上 SIGKILL 后仍可能留下逃逸后代，S01 已按官方 `subprocess-local` 的限制标注，S08 不承诺追到所有后代。
- Ghostty 全屏下的人工键位验收归 S13 的 design-contract；当前键位（`!`、Ctrl+T）是 S08 的实现默认值。

## 边界数字

- `!` 注入前把合成消息的 JSON 量化到 200,000 字节以下（低于 S01 的 256 KiB 事件帧上限），必要时按流截断并标 `injected context truncated` + spill 路径；模型侧只拿到有界上下文。本窗口显示尾部最多 4,000 字符，超出时显式标「显示已截断」，官方截断/spill 提示在尾部所以不会被丢掉。
- 任务列表最多显示 200 条官方投影；列表滚动跟随选中项；输出详情只保留一个 job 的 256 KiB 尾部，渲染取尾部最多 200,000 字符并显式标「显示已截断」。官方 ring 的 `retainBytes` 262144 / `settledRetainBytes` 16384 / `pumpPollMs` 150 是唯一输出预算来源。
- `nano.shell.exec` 在请求上显式设 `timeoutMs: 60000`（与当前 profile 的 executor deadline 一致），Rust 等待上限 75s 留出准备/终止余量；不是新的执行超时。

## 复现

```sh
cargo build -p nano-app
scripts/pty-evidence-s08.sh
NANO_E2E=1 node --test bridge/test/integration/host.test.mjs
# 真实模型证据（会使用用户凭据并产生费用；需用户授权）
NANO_EVIDENCE_MODEL=deepseek-flash node scripts/model-evidence-s08.mjs
NANO_EVIDENCE_MODEL=deepseek-flash NANO_EVIDENCE_LOG=/tmp/s08-promotion.jsonl \
  scripts/pty-evidence-s08-promotion.sh
# 原始 JSONL 含系统指令与运行时上下文，不入库；只提取 tool/call、job/*、notice 事件
```

普通 PTY 脚本使用临时 `DSH_HOME` 与数据根，不读写用户的 `~/.dsh` 会话或凭据。host 模型证据脚本在真实 `DSH_HOME` 下安装一个临时 profile、用后删除，不复制凭据，也不改动已有 profile。TUI 模型证据脚本使用临时 `DSH_HOME`，只符号链接真实凭据文件（未复制；运行前后 sha 一致），安装临时 profile 并在退出后删除。
