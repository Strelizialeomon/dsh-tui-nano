# @dsh-tui-nano/bridge

薄桥接：在官方 dsh profile 进程内装载，独占 `protocol/` 的 `nano.*` 契约、会话挂载账本和关停顺序。

- `runtime/`：Cordis 插件入口、帧解析、fd 3/4 传输、账本、订阅、关停。
- `controls/`：控制宿主。命令目录来自官方 `commands` 服务；模型目录调用官方导出的 `buildModelCatalog`（在注入 `llm` 的子上下文里），切换安装官方 `installModelSelection` 并写官方 `model/selection`；权限读取来自官方投影/预设目录，切换固定走官方 `/permission <preset>` 命令（不直接 `permissionPresets.set()`）。实时事件为 `controls.model` 与 `controls.permission`。
- `skills/`：技能与规矩宿主。`nano.skills.list` 只读官方 `ctx.skills.snapshot()` 并附官方命令名；`nano.skills.preview` 按官方 `path` 有界只读；`nano.instructions.snapshot` 折合官方 `agent-instructions` 持久消息的来源、时间与预算提示。只订阅 `skills/change`/`commands/change` 发刷新信号，不自建 watcher，也不解析指令文件。
- `conversation/`：对话宿主。`submit` 按官方 `user-rpc` 源写入 `requestId`，`editQueued`/`deleteQueued`/`steerQueued` 修改官方 inbox，`cancel` 固定 `keepInbox: true`，`snapshot` 返回绝对状态；实时事件为 `conversation.status`、`conversation.inbox` 和 `conversation.stream`。inbox 投影在超过事件帧上限时降级为有界预览并标 `truncated`；单次提交上限 32 KiB。
- `attachments/`：S06 附件宿主。`nano.attachment.begin|chunk|finish|discard` 以 48 KiB 原始 chunk 接收图片和通用文件；图片只暂存，提交时经官方 `ctx.attachments.admitPromptContent()` 校验、规范化和持久化；通用文件在 `finish` 时经官方 `saveFileStream()` 按原字节保存并生成 `FileAttachmentRef`，提交时校验归属并转交。回执在 inbox/durable 观察后退休，已 commit 未退休的回执不能再次提交。`nano.fileReferences.list` 调用官方 `ctx.fileReferences`，只给路径候选。
- `cordis.patch.yml`：bundle 的固定行集（bridge、`tool-ask-user`、关闭 HMR），并额外挂载 `@deepseek-ai/dsh-file-reference-local`，因为最小 `dsh-base` 组合不包含 `@file` 发现 provider。
- `profile/install.mjs`：初始化 `$DSH_HOME/profiles/dsh-tui-nano` 并安装本包（`dsh plugin --profile ... install`）。

运行前提：由 Rust `nano-runtime` 通过 `dsh --profile dsh-tui-nano --patch <runtime-patch>` 启动；`--patch` 提供每会话 storage 根和 `nano-bridge` 配置。手动直接运行该 profile 会因缺配置报 `nano.startupError: startup-failed`，这是预期行为。

诊断只写 stderr；协议帧只在 fd 3/4（或带 nonce 的 stdin/stdout 回退）上。
