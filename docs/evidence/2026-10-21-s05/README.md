# S05 证据（2026-10-21）

## 已执行

- `cargo test`：nano-app 40 项、nano-conversation 52 项 + 契约 5 项、nano-runtime 18 项 + 契约 10 项、nano-skills 20 项 + 契约 5 项全绿；见 `tests.txt`。
- `node --test bridge/test/*.test.mjs`：84 项通过，含 S05 桥接 18 项与协议夹具。
- `NANO_E2E=1 node --test bridge/test/integration/*.test.mjs`：3 项通过。新增的 S05 真实宿主测试用真实 dsh profile、真实技能目录、真实 `AGENTS.md`/`CLAUDE.md`/`AGENTS.local.md` 与真实 `agent-instructions` 消息验证发现、预览、watcher 刷新、删除、`/hello` 官方注入和规矩来源；见 `tests.txt`。
- `cargo test -p nano-runtime --test integration -- --ignored --test-threads=1 --skip shutdown_budget_is_bounded_when_cleanup_hangs`：14 项 S01 真实宿主回归通过；见 `s01-regressions.txt`。
- `python3 scripts/pty-evidence-s05.py`：真实 PTY（120×40）驱动真实 `nano` 二进制；见 `pty.txt`。全部 23 项检查通过，覆盖：
  - 技能面板列出官方目录（`hello`、`user-only`，仅用户可调用标记、描述、官方路径）。
  - 发送前预览 `SKILL.md`，正文可见。
  - 外部新增 `added-later` 技能后，官方 watcher 触发并重拉目录，下一次打开面板可见。
  - `/hello` 按字面量发送，官方 pre-step 注入 `<skill_content name="hello">`，界面显示「技能正文已注入：/hello」。
  - 未知 `/does-not-exist` 按普通 prose 通过，回合结束后显示「未注入」。
  - `/compact` 命中官方命令，命令优先，草稿保留。
  - 项目规矩面板按官方顺序显示 `$DSH_AGENTS_HOME/AGENTS.md` → `AGENTS.md` → `AGENTS.local.md`；内容相同的 `CLAUDE.md` 只出现一次；显示「最近一次加载」相对时间。
  - 第二个真实会话用超预算的本地覆盖文件，面板显示官方预算提示 `Workspace instruction budget 65536 bytes: omitted …; truncated AGENTS.local.md …`。
  - 两次会话都 Ctrl+C 干净退出（exit 0）。

技能与规矩夹具都由脚本在临时目录生成，不读写用户 `~/.dsh` 会话：

- `work/.git`：项目根标记。
- `work/.agents/skills/hello/SKILL.md`：普通技能，正文含 `S05_SKILL_BODY_MARKER`。
- `work/.agents/skills/user-only/SKILL.md`：`disable-model-invocation: true`。
- `work/.agents/skills/added-later/SKILL.md`：会话运行中由脚本新增。
- `work/AGENTS.md` 与内容相同的 `work/CLAUDE.md`：去重夹具。
- `work/AGENTS.local.md`：本地覆盖；预算场景下放大到超过 65536 字节。
- `$DSH_AGENTS_HOME/AGENTS.md`：共享全局链夹具。

PTY 与真实宿主测试都把官方 DeepSeek 路由指向本机 Messages 兼容 mock（固定 SSE `ok`），因此真实 pre-step、技能注入、指令加载都实际执行，但没有真实模型费用，也没有使用用户凭据。

## 重审后的修订（2026-10-21）

用户点档「重审」后，两路只读审核的结论原样贴在 PR #21。代码侧已修：

- 不完整的官方目录不再覆盖上次可用目录；只有完整观察才替换，首次不完整观察仍展示部分候选。
- S05 面板不再被无关宿主事件弹栈关闭；空 inbox 只关闭队列焦点。
- 断线重建订阅补上 `skills.changed`/`instructions.changed`，初始与重建共用 `SUBSCRIPTION_KINDS`。
- 预览 RPC/解析错误保留预览块并显示错误；迟到/异地址预览被拒绝；目录刷新后失效的旧预览被清除。
- 技能列表窗口按实际换行高度计算，长路径不再把选中项或预览挤出屏幕。
- `skills.snapshot` 与模型侧一致传 `scope: agent`，cwd 走官方 `workingDirectory.ensure(agent)`，并加 20s 边界。
- 预算提示不再被 UI 静默截到 600 字符；只接受官方渲染的首行预算提示，规则正文伪造的同名前缀被忽略；新基线重置投影截断标记。
- 规矩面板滚动有上界；`//compact` 不再被当成 `compact` 命令。
- PTY 证据加强：预览检查预览标题；watcher 在面板保持打开时验证目录刷新；不再因重开面板而假绿。

规格正文已按用户点的「改旧的」同步：§4 明确超上限显示截断提示、§9 补 512 来源/8192 提示/4000 显示上限、§10 改为实施状态并指向证据；增量顺序、`watch:false`、加载失败三项改写为已知限制。

仍作为已知限制记录：

- 增量新增的浅层规矩来源按到达顺序追加，不是完整官方链顺序；面板与规格都如实标注“基线按官方加载顺序；增量按官方消息到达顺序追加”。
- `watch:false` 没有公开 API 可读 provider 配置，S05 不提供自动标注。
- 权限/I/O/根标记失败的官方错误没有公开事件通道进入面板；官方回合失败本身仍显示在对话记录。

## 未执行（不得勾选）

- **真实远程技能 provider**：本份只用官方 filesystem provider。远程 provider 的发现失败、重试与 `complete=false` 只由桥接单元测试覆盖，未接真实远程服务。
- **读写文件后新增嵌套规矩**：需要真实模型发出 `read`/`write`/`edit` 工具调用后由官方 loader 发现；本份没有跑真实工具调用，只由桥接单元测试覆盖 durable 消息折叠与基线替换。
- **会话恢复后的规矩基线校正**：桥接单元测试覆盖「构造时折叠已有 durable `agent-instructions` 消息」，没有单独录一次真实 `--resume` 的规矩面板。
- **工作目录变化后的重载**：切换会话/工作目录属于 S07；S05 只由单元测试覆盖「新的完整基线替换旧来源集合」，没有真实窗口级切换证据。
- **权限 / I/O / 根标记失败**：由官方 loader 抛错；本份验证了「没有消息就不伪造来源」，没有注入这三类真实失败。
- **真实模型消费与注入成本**：mock 只验证官方注入确实发生；未跑带真实凭据的模型回合。

## 边界数字

`maxSkillPreviewBytes` 65536、来源投影上限 512 条、预算提示上限 8192 字符是 S05 自定细节；64 KiB 预览上限与事件帧上限（262144 bytes）无关，来源投影上限用于保证 `instructions.changed` 不逼近事件帧上限。官方的技能描述长度、目录缓存与 watcher 防抖（默认 200ms/100ms，microtask 合并）不在 S05 重算。

## 复现

```sh
cargo build -p nano-app
cargo test
node --test bridge/test/*.test.mjs
NANO_E2E=1 node --test bridge/test/integration/*.test.mjs
cargo test -p nano-runtime --test integration -- --ignored --test-threads=1 --skip shutdown_budget_is_bounded_when_cleanup_hangs
python3 scripts/pty-evidence-s05.py
```

PTY 脚本使用临时 `DSH_HOME`、`DSH_AGENTS_HOME` 和数据根；mock 模型监听 `127.0.0.1` 随机端口。
