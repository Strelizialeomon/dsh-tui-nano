# S06 引用与图片：实施证据

- 日期：2026-10-21
- 规格：`docs/specs/2026-10-20-nano-v1/S06-references-attachments.md`
- 执行单：#9
- 官方基线：`dsh 0.2.1-rc.2`，公开 `ctx.attachments` / `ctx.fileReferences` 服务

## 1. 自动测试

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| `node --test bridge/test/*.test.mjs` | 167 通过 / 0 失败（含 S04/S05/S07/S08 回归） | `bridge-unit.txt` |
| `cargo test` | 全部通过（`nano-attachments` 26、`nano-app` 100；含 S04/S05/S07/S08 回归） | `rust-unit.txt` |
| `NANO_E2E=1 node --test bridge/test/integration/attachments.test.mjs` | 1 通过 | `real-attachments.txt` |
| `NANO_E2E=1 node --test bridge/test/integration/*.test.mjs` | 6 通过（含 S01/S02/S05/S07/S08 回归） | `real-host-all.txt` |
| `cargo test -p nano-runtime --test integration -- --ignored --test-threads=1 --skip shutdown_budget_is_bounded_when_cleanup_hangs` | 14 通过 | `rust-real-host.txt` |
| `python3 scripts/pty-evidence-attachments.py` | 14/14 检查通过，退出码 0 | `pty-attachments.txt` |

## 2. 真实官方链路

`real-attachments.txt` 使用临时 `DSH_HOME`、临时数据根和真实 `dsh` profile：

- `nano.hello` 报告 `attachments`、`fileReferences` 为 `supported`，并带官方图片限额。
- `nano.fileReferences.list` 对 `mai` 返回 `src/main.rs`，对空 query 返回工作区候选，对目录 query `src/` 返回目录项；只返回路径。
- 图片按两个 chunk 暂存 70 字节的 1×1 PNG，`finish` 后 `ready: true`；图片在提交时才由 `ctx.attachments.admitPromptContent()` 规范化并持久化。
- 通用文件 `notes.txt` 经 `saveFileStream()` 原字节保存，返回 `sha256:` 开头的 `attachmentId`（测试断言前缀）、文件名和 11 字节长度；0 字节的 `empty.log` 也完成保存并返回 0 字节引用。
- 提交 `[text, image, image, file, text]` 后，官方 `agent/inbox/spliced` 中消息内容顺序完全一致；图片块只有 `attachmentId/mediaType/width/height`，文件块只有 `attachmentId/name/bytes`。
- PNG、GIF、WebP、JPEG 四种源格式各自完成一次 staging + 提交 + 持久化。测试断言的是「源格式被准入」和「持久媒体类型属于官方支持集合」，不把源扩展名当成 wire 类型；本次实测 GIF 归一化为 `image/webp`，具体类型由官方规范化决定。
- 持久事件序列化结果不含 PNG base64、不含临时工作目录路径。
- 同一 `requestId` 再次提交返回 `duplicate: true` 与同一 `messageId`；已观察退休的图片回执再次引用返回 `receipt-unknown`/`receipt-committed`。
- 正常关停 `clean: true`；generation 2 恢复后重放同一持久事件，仍无 base64 与本地路径。

## 3. 真实 PTY

`pty-attachments.txt` 在 120×40 PTY 中驱动已构建的 `nano`：

1. 输入 `@mai`，补全面板出现 `src/main.rs`；
2. Enter 插入 `@src/main.rs`，补全面板关闭；
3. Alt+A 打开附件路径输入，输入 `pixel.png`，卡片显示 `[图片] pixel.png · 70 B · 1×1 · 已就绪`；
4. 单卡时按 Alt+→，进程不 panic；
5. 再附加 `notes.txt`，按 Alt+← 把它换到图片前；
6. 发送后回显保持重排后的顺序：`@src/main.rs[文件 notes.txt][图片 pixel.png] look`；
7. 没有模型路由时回合明确失败，界面显示失败原因，不伪造回答；
8. Ctrl+C 退出码 0。

## 4. 失败夹具

`bridge/test/attachments.test.mjs` 覆盖：

- chunk 顺序错误 → `nano/attachments/chunk-sequence`（1037）；
- 非 canonical base64 → `jsonrpc/invalid-params`；
- 单 chunk 超过 48 KiB → `nano/frame-too-large`；
- 声明字节数与实际不符 → `nano/attachments/receipt-incomplete`（1036）；
- 官方图片准入失败保留稳定 reason（如 `IMAGE_TYPE_MISMATCH`）→ 1038；
- 模型明确不支持图片 → `MODEL_DOES_NOT_SUPPORT_IMAGES` / 1039；
- 未完成、未知、已 commit 的回执不能再次提交 → 1036/1034/1035；
- 跨会话地址 → 1011；
- 子代理地址通用文件被拒绝（1008），图片可暂存；
- 回执 id/显示名不能夹带本地路径；
- 0 字节通用文件可保存；每消息图片总量超限 → 1040；
- 同一 requestId 并发提交只准入一次、只产生一条消息；
- `bindPrompt` 在 Agent 同步 inbox 事件前生效；durable user message / discarded 也会退休回执；
- 上传失败不自动重发；传输丢失标「结果未知」（Rust app 测试）；
- inbox/durable 观察后回执退休。

`crates/nano-attachments` 覆盖：`@` 在开头/空白后触发、词内邮箱不触发、引号路径、目录下钻、绝对路径格式化、非法字符拒绝、慢响应被 revision 拒绝、光标移动取消补全、多行草稿只替换 token、四种图片格式的魔数与尺寸（含 JPEG `0xFF` 填充）、声明类型不符、字节/像素/边长限额、附件与文本顺序、锚点随编辑移动、排序首尾边界不 panic、未就绪附件阻止提交。

## 5. 实测限额

真实宿主报告并据此校验的官方默认值：单图 20 MiB、每条消息 20 张、总量 200 MiB、6400 万像素、单边 8192 像素，格式 PNG/JPEG/WebP/GIF。界面不写死来源上限；`hello.attachmentLimits` 缺失或不完整时，Rust 不猜测限额并明确拒绝图片入口，bridge 保留 64 MiB 的本地暂存硬上限，官方准入仍是最终判定。`fileAdmission` 对随附 `dsh-attachment-local` 声明 `verbatim`；换用不支持文件保存的 store 时 `finish` 会以 1041 明确失败。

## 6. 未测 / 边界

- 未发起真实模型请求（无付费预算）；图片/文件准入与持久化已由真实官方服务验证，模型侧实际看图不在本份证据内。
- 动画 GIF 按官方规则只保留首帧；本份没有额外的动画测试。
- 子代理输入入口属 S12，当前窗口没有该入口；本份只在协议校验层落实「子代理只收图片、通用文件拒绝」。
- PTY 证据覆盖的是 Ghostty 兼容的 xterm 序列，不是 Ghostty 人工验收；S13 仍负责最终终端矩阵。
- 真实部署限额下的超限/像素/边长拒绝未单独跑：本地检查与 fake store 限额测试覆盖了判定分支，但未用超限真实图片触发官方拒绝。
- `hello.attachmentLimits` 缺失/不完整、`fileAdmission: unsupported` 两种部署形态没有真实宿主；代码按「不猜测、明确拒绝」处理。
- 绝对路径的 `@` 补全只在 Rust 格式化层测过；官方 `file-reference-local` 的发现查询按工作区相对路径工作，绝对 query 未返回候选。
