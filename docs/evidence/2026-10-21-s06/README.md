# S06 引用与图片：实施证据

- 日期：2026-10-21
- 规格：`docs/specs/2026-10-20-nano-v1/S06-references-attachments.md`
- 执行单：#9
- 官方基线：`dsh 0.2.1-rc.2`，公开 `ctx.attachments` / `ctx.fileReferences` 服务

## 1. 自动测试

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| `node --test bridge/test/*.test.mjs` | 82 通过 / 0 失败 | `bridge-unit.txt` |
| `cargo test` | 全部通过（含 23 个 `nano-attachments` 单元测试） | `rust-unit.txt` |
| `NANO_E2E=1 node --test bridge/test/integration/attachments.test.mjs` | 1 通过 | `real-attachments.txt` |
| `NANO_E2E=1 node --test bridge/test/integration/*.test.mjs` | 3 通过（含 S01/S02 回归） | `real-host-all.txt` |
| `cargo test -p nano-runtime --test integration -- --ignored --test-threads=1 --skip shutdown_budget_is_bounded_when_cleanup_hangs` | 14 通过 | `rust-real-host.txt` |
| `python3 scripts/pty-evidence-attachments.py` | 10/10 检查通过，退出码 0 | `pty-attachments.txt` |

## 2. 真实官方链路

`real-attachments.txt` 使用临时 `DSH_HOME`、临时数据根和真实 `dsh` profile：

- `nano.hello` 报告 `attachments`、`fileReferences` 为 `supported`，并带官方图片限额。
- `nano.fileReferences.list` 对 `mai` 返回 `src/main.rs`，对空 query 返回工作区候选；只返回路径。
- 图片按两个 chunk 暂存 70 字节的 1×1 PNG，`finish` 后 `ready: true`；图片在提交时才由 `ctx.attachments.admitPromptContent()` 规范化并持久化。
- 通用文件 `notes.txt` 经 `saveFileStream()` 原字节保存，返回 `sha256:` 开头的 `attachmentId`、文件名和 11 字节长度。
- 提交 `[text, image, image, file, text]` 后，官方 `agent/inbox/spliced` 中消息内容顺序完全一致；图片块只有 `attachmentId/mediaType/width/height`，文件块只有 `attachmentId/name/bytes`。
- PNG、GIF、WebP、JPEG 四种源格式各自完成一次 staging + 提交 + 持久化；GIF 按官方规范化成为 `image/webp`，其余源格式保留对应归一化类型。测试断言的是「源格式被准入」和「持久媒体类型属于官方支持集合」，不把源扩展名当成 wire 类型。
- 持久事件序列化结果不含 PNG base64、不含临时工作目录路径。
- 同一 `requestId` 再次提交返回 `duplicate: true` 与同一 `messageId`；已观察退休的图片回执再次引用返回 `receipt-unknown`/`receipt-committed`。
- 正常关停 `clean: true`；generation 2 恢复后重放同一持久事件，仍无 base64 与本地路径。

## 3. 真实 PTY

`pty-attachments.txt` 在 120×40 PTY 中驱动已构建的 `nano`：

1. 输入 `@mai`，补全面板出现 `src/main.rs`；
2. Enter 插入 `@src/main.rs`，补全面板关闭；
3. Alt+A 打开附件路径输入，输入 `pixel.png`，卡片显示 `[图片] pixel.png · 70 B · 1×1 · 已就绪`；
4. 发送后回显保持顺序：`@src/main.rs[图片 pixel.png] look`；
5. 没有模型路由时回合明确失败，界面显示失败原因，不伪造回答；
6. Ctrl+C 退出码 0。

## 4. 失败夹具

`bridge/test/attachments.test.mjs` 覆盖：

- chunk 顺序错误 → `nano/attachments/chunk-sequence`（1022）；
- 非 canonical base64 → `jsonrpc/invalid-params`；
- 单 chunk 超过 48 KiB → `nano/frame-too-large`；
- 声明字节数与实际不符 → `nano/attachments/receipt-incomplete`（1021）；
- 官方图片准入失败保留稳定 reason（如 `IMAGE_TYPE_MISMATCH`）→ 1023；
- 模型明确不支持图片 → `MODEL_DOES_NOT_SUPPORT_IMAGES` / 1024；
- 未完成、未知、已 commit 的回执不能再次提交 → 1021/1019/1020；
- 跨会话地址 → 1011；
- 子代理地址通用文件被拒绝（1008），图片可暂存；
- 回执 id/显示名不能夹带本地路径；
- 上传失败不自动重发；传输丢失标「结果未知」；
- inbox/durable 观察后回执退休。

`crates/nano-attachments` 覆盖：`@` 在开头/空白后触发、词内邮箱不触发、引号路径、目录下钻、非法字符拒绝、慢响应被 revision 拒绝、光标移动取消补全、四种图片格式的魔数与尺寸、声明类型不符、字节/像素/边长限额、附件与文本顺序、锚点随编辑移动、未就绪附件阻止提交。

## 5. 实测限额

真实宿主报告并据此校验的官方默认值：单图 20 MiB、每条消息 20 张、总量 200 MiB、6400 万像素、单边 8192 像素，格式 PNG/JPEG/WebP/GIF。界面不写死来源上限；`hello.attachmentLimits` 缺失时 Rust 放弃本地限额预检，仍由官方准入最终判定。

## 6. 未测 / 边界

- 未发起真实模型请求（无付费预算）；图片/文件准入与持久化已由真实官方服务验证，模型侧实际看图不在本份证据内。
- 动画 GIF 按官方规则只保留首帧；本份没有额外的动画测试。
- 子代理输入入口属 S12，当前窗口没有该入口；本份只在协议校验层落实「子代理只收图片、通用文件拒绝」。
- PTY 证据覆盖的是 Ghostty 兼容的 xterm 序列，不是 Ghostty 人工验收；S13 仍负责最终终端矩阵。
