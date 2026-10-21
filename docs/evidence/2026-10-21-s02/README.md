# S02 证据（2026-10-21）

## 已执行

- `node --test bridge/test/*.test.mjs`：61 项通过，含对话服务 14 项与对话契约夹具。
- `cargo test`：工作区单元/契约测试通过（nano-conversation 41 项、nano-app 14 项、nano-runtime 单元/契约）。
- `cargo test -p nano-runtime --test integration -- --ignored --test-threads=1 --skip shutdown_budget_is_bounded_when_cleanup_hangs`：14 项真实宿主回归通过。
- `NANO_E2E=1 node --test bridge/test/integration/host.test.mjs`：2 项通过；第二项在真实 dsh 宿主上验证 submit、requestId 去重、inbox 投影、resume 后从官方日志重放。
- `python3 scripts/pty-evidence.py`：真实 PTY（120×40）启动真实 dsh profile；空提交被拒、括号粘贴多行、提交后回显、输入清空、回合失败可见、Ctrl+C 干净退出。快照见 `pty-default.txt`。

`pty-default.txt` 的会话没有配置 provider/model，因此回合在官方侧以 `no provider/model` 失败；这正是「失败可见、不伪造成功」的证据。配置 `--provider deepseek-official --model <id>` 且提供凭据时走同一路径。

## 未执行（不得勾选）

- 真实模型往返（流式正文、真实工具调用、正式 `user/message`）：需要用户授权模型预算；本轮未调用模型。
- Ghostty 全屏下的中文输入法组合态、选择复制、滚轮：需要人工在目标终端验收；输入模型层已有组合态 Enter 不发送的单元测试。
- 运行中队列/插话/停止的真实模型时序：桥接单元测试覆盖队列变更与 keepInbox 取消；真实宿主测试覆盖 inbox 事件；模型运行态时序待有预算的真实会话。
- 子代理地址与草稿：S02 只定义地址形状，S12 实施。

## 复现

```sh
cargo build -p nano-app
python3 scripts/pty-evidence.py
```

脚本使用临时 `DSH_HOME` 与数据根，不读写用户的 `~/.dsh` 会话；它会安装一次性 profile。
