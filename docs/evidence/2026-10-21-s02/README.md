# S02 证据（2026-10-21）

## 已执行

- `cargo test`：nano-conversation 49 项 + 契约 5 项、nano-app 24 项、nano-runtime 单元/契约全绿。
- `node --test bridge/test/*.test.mjs`：65 项通过，含对话服务 18 项与契约夹具。
- `cargo test -p nano-runtime --test integration -- --ignored --test-threads=1 --skip shutdown_budget_is_bounded_when_cleanup_hangs`：14 项真实宿主回归通过。
- `NANO_E2E=1 node --test bridge/test/integration/host.test.mjs`：2 项通过；真实宿主验证 submit、requestId 去重、inbox 投影、resume 后从官方日志重放。
- `python3 scripts/pty-evidence.py`：真实 PTY（120×40）启动真实 dsh profile；空提交被拒、括号粘贴多行、提交回显、输入清空、回合失败可见、Ctrl+C 干净退出。快照见 `pty-default.txt`。
- 真实模型会话 1（用户授权，deepseek-official / deepseek-chat）：文本发送、流式接收、一次真实 bash 工具调用与结果、正式 `user/message` + `assistant/message`、turn completed。官方日志摘录见 `real-session.txt`。
- 真实模型会话 2（用户授权，同一路由）：运行中 queue、Alt+S 插话（next-turn → next-step）、Alt+D 删除、Esc 停止保留队列、Alt+X 逐项清空。屏幕统计与官方 inbox splice 摘录见 `real-queue-session.txt`。

两次真实会话使用用户真实 `~/.dsh` 凭据（未复制），并把 profile 安装到 `~/.dsh/profiles/dsh-tui-nano`；会话数据根与 cwd 都是临时目录。摘录剔除了系统指令、运行时上下文和其他注入消息。

## 未执行（不得勾选）

- Ghostty 全屏下的中文输入法组合态、选择复制、滚轮：需要人工在目标终端验收。输入模型层有组合态 Enter 不发送的单元测试；crossterm 不暴露 IME 组合事件，真实组合态由终端自己吞掉 Enter。`Ctrl+Y` 使用 OSC 52 复制原始草稿，未在 Ghostty 人工验证。
- 运行中「清空并取消」的取消段：Alt+X 在空闲时只逐项删除；运行中的停止保留队列已由会话 2 验证，但「删除后立即 cancel」的竞态没有单独的真实回合证据。
- 切换会话/进入子对话返回后的真实导航：S02 只提供地址、草稿、场景/焦点接口；真实子对话导航归 S12。
- 真实失败注入（持久化/清理失败）归 S01；真实审批/问卷归 S03。

## 边界数字

`MAX_PREVIEW_BYTES` 256 KiB、`MAX_TOTAL_PREVIEW_BYTES` 1 MiB、`MAX_HISTORY_ITEMS` 2000、回显 128 条、单次提交 32 KiB 是按 S01 的 256 KiB 事件帧上限与 32 MiB 请求帧上限推导的有界值，不是终端耗时实测；实时预览用 dense-index 与字节上限兜底，超出显式标记截断/缺口。

## 复现

```sh
cargo build -p nano-app
python3 scripts/pty-evidence.py
```

脚本使用临时 `DSH_HOME` 与数据根，不读写用户的 `~/.dsh` 会话。真实模型会话需要用户授权与 `--provider deepseek-official --model <id>`（会使用官方凭据并产生费用）。
