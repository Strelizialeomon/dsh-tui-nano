# S09 交互终端实施证据（2026-10-21）

对应规格：`docs/specs/2026-10-20-nano-v1/S09-terminal.md`；执行单 #12。
环境与冻结版本见 `versions.txt`：官方 dsh 0.2.1-rc.2、Node v24.18.0、vt100 0.15.2、tui-term 0.2.0。

## 真实 TUI 会话

用 `tmux` 驱动真实 `nano` 二进制、真实官方宿主和真实 `zsh -i`，命令与截屏：

| 文件 | 证明 |
| --- | --- |
| `01-conversation.txt` | 宿主握手完成，对话场景可用 |
| `02-terminal-attached.txt` | F12 打开终端，官方 `create`/`retain`/`attach` 成功，状态为 `connected`、`可输入`，显示真实 zsh 提示符 |
| `03-terminal-output.txt` | 真实输入与输出；宽字符 `世界`、组合字符 `é`、ANSI 颜色都能经过 PTY 和 VT 解析器；配色在 tmux 文本截屏中被剥掉，颜色语义由 `parser-compat.txt` 的 SGR 用例核验 |
| `04-terminal-scrollback-source.txt` | 连续命令输出进入有界屏幕，未被会话或输入区污染 |
| `04b-terminal-reattached.txt` | F10 重附成功，状态回到 `connected`；验证 attach 的 `sessionId` 路径 |
| `05-terminal-closed.txt` | F11 关闭后状态为 `closed`，占位页明确说明终端已关闭 |
| `05b-shell-picker.txt` / `05c-shell-picker-cancelled.txt` | 关闭后 F9 能正常渲染 shell 列表，Esc 取消 |
| `06-conversation-return.txt` | F12 返回对话，终端不残留为对话内容 |
| `07-exit.txt` / `exit.txt` | Ctrl+C 正常退出，宿主返回 `EXIT:0` |
| 按键 | F12 进入/返回、F11 关闭、F10 重附、F9 选 shell、F8 复制；滚动曲线按 `TERM_PROGRAM` 选择 Ghostty / xterm.js / 默认档 |
| `install.json` / `install.err` | 真实 profile 安装成功且无错误 |

截屏是 tmux 文本捕获，不是浏览器截图，也不是伪造数据。证据只包含临时目录路径，不含用户会话正文。

## 自动化证据

| 文件 | 命令 | 结果 |
| --- | --- | --- |
| `cargo-test.txt` | `cargo test --workspace` | 全部通过；含 S01/S02 回归、S09 状态机与输入编码 |
| `bridge-test.txt` | `npm run test:bridge` | 全部通过；含终端服务、序列、retain/release、关闭与负向工具夹具 |
| `parser-compat.txt` | `cargo test -p nano-terminal --test parser_compat` | 9 个冻结解析器兼容样本通过（SGR、宽/组合字符、alt-screen、滚动区、鼠标模式、输入模式） |
| `terminal-e2e.txt` | `cargo test --test integration terminal -- --ignored --test-threads=1` | 7 个真实宿主测试通过 |
| `full-e2e.txt` | `cargo test --test integration -- --ignored --test-threads=1 --skip shutdown_budget_is_bounded_when_cleanup_hangs` | 21 个 S01/S09 真实宿主回归全部通过 |

真实宿主测试覆盖：

1. `terminal_create_input_resize_reattach_close`：创建、幂等 create、retain、attach 快照、输入、ack、resize 后的 state、detach 不杀进程、重新附着拿到一致屏幕、`vim` 进入/离开 alt-screen、close、list 为空；并检查 `sessions/` 下没有终端输出。
2. `terminal_retain_protects_the_process_until_release`：缩短 `unattendedTimeoutMs`/`activityPollIntervalMs`，有 hold 时超过超时仍保留，release 后按官方超时回收。
3. `terminal_process_exit_disables_input_then_drains`：`exit` 后先收到 `writable=false` 的 `exited` state，附件保持可读；close 排空并结束附件。
4. `terminal_without_a_retain_hold_is_reclaimed`：只 attach 不 retain 的终端被官方无人值守回收，附件结束；证明 `retain()` 是必要持有关系。
5. `terminals_are_scoped_to_their_root_session_host`：两个不同根会话各自启动宿主；A 创建的终端在 A 的 list 可见、在 B 的 list 为空，关闭 A 不影响 B。
6. `nano_profile_has_no_user_terminal_agent_tool`：真实 `--dump-config` 证明 profile 挂了 `terminal-controller`，但没有 `dsh-tool-terminal` 或 Monitor 行；Agent/Monitor 没有用户终端入口。
7. `host_shutdown_closes_a_live_terminal`：有活动附件和 hold 时宿主正常关停，`agents` 步骤在预算内关闭终端，关停报告干净。

## 未测 / 未保证

- `less`、`tmux` 嵌套会话没有单独自动化；它们与 `vim` 使用同一 PTY、alt-screen 和 VT 解析路径。`vim` 的 alt-screen 已实测。
- 模型请求内容未检查：本证据没有发起模型请求，也不使用凭据；「终端输出不进入模型上下文」由结构保证（终端帧只走 `nano.terminal.*` 实时通知，不写 `session/event`），并用 `sessions/` 文件不含标记文本做了直接检查。
- 不同根会话的宿主互不可见已实测；同一个 `nano` 进程内切换根会话的旧终端清理依赖 S07（尚未实施）。当前每个 `nano` 进程只驱动一个根会话，传输断开后终端进入 `disconnected` 并清空附件。
- macOS 强杀宿主后的逃逸后代范围沿用 S01 已记录的范围，不在此重复宣称。
- 未支持的 shell 活动观察不会被误回收；本轮只用受支持的 `zsh -i`/`bash -i` 验证，未知 shell 行为按官方限制记录。


## 重审处置（2026-10-21）

两路重审结论见 PR #25 评论。已处置：

- attach 对齐 S01：请求带 `sessionId`、可选 `terminalId`，结果字段为 `snapshot`。
- 未知 create 的 pending id 不再被 list 结果误删；重开用同一 id 重试，关闭时按 pending id 发 `nano.terminal.close`。epoch 守卫丢弃旧请求的迟到响应。
- F9 选择器在创建/连接/持有/pending 时拒绝打开；无 parser 时也渲染列表；窄窗口按区域裁剪，不再越界 panic；Shift 鼠标事件绕过程序鼠标上报做本地选择。
- 鼠标释放后清选择，滚轮恢复；X10/UTF-8 释放编码改为 button code 3；选区列号按 parser 宽度 clamp。
- 传输断开后迟到响应不再覆盖 `disconnected`；关闭成功后清旧错误；输出排空在状态行单独标注。
- 桥接输入队列按 `maxBufferedBytes` 计费、ack 释放，失败时清空排队命令；`slow-consumer` 结束原因可达；未知 `info.state` 显式失败；`--resume` 的非安全 session id 在落盘前拒绝。
- 鼠标捕获移入 `TerminalGuard`，panic/错误退出也会恢复外层终端；宿主关停对终端 close 做有限重试。
