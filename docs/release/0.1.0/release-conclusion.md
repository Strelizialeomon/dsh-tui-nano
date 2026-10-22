# 发布结论（0.1.0）

**结论：不发布。**

依据：`docs/specs/2026-10-20-nano-v1/S13-release.md` §6「阻断缺陷定义」与 §8「验收」。以下任一项存在即阻断发布，且不能用「已知问题」降级。

## 1. 阻断项

| # | 阻断项 | 可观察判据 | 证据 |
| --- | --- | --- | --- |
| B1 | 输入框上下文占比线缺失 | 输入框只有普通四边框；没有上下两条按占用比例着色的单单元格线；标题行 `ctx` 的 60/80 阈值也与契约的 80/95 不一致 | `design-contract-compliance.md` §3；`crates/nano-app/src/ui.rs:251`、`:2349` |
| B2 | 主会话约 500ms 忙碌指示缺失 | 标题行只有静态状态文字/颜色，没有周期变化的字符指示 | `design-contract-compliance.md` §11；`crates/nano-app/src/ui.rs:222` |
| B3 | 浅色主题 diff token 未落地 | diff 底色硬编码深色 `Rgb(0,42,0)` / `Rgb(48,0,0)`，浅色终端下没有对应的 light token | `design-contract-compliance.md` §14；`crates/nano-transcript/src/render.rs:670` |
| B4 | 真实系统输入法组合态未验证 | 没有真实 Pinyin/IME 组合、候选选择、组合中 Enter 的证据；S02 与 S13 都明确未测 | `known-limitations.md` §1；S02 issue #5 评论 |
| B5 | 首次启动未显示配置/凭据/全局指令来源 | 应用启动没有来源文案；安装器人类输出打印路径，但 `--json` 验证输出不打印；`Alt+R` 只显示规矩来源 | `crates/nano-app/src/main.rs:28-43`；`packaging/install.sh:199-211`；`evidence/ghostty/02-dark-100-instructions.png` |

B1–B3 属于「键位、布局、主题 token 互相冲突且未按 design-contract 统一」；B4 使「真实终端证据齐全」不成立；B5 属于「必需产物/契约缺失」。五项都不在 S13 内修复：S13 不代其他子件补写功能。

此外，S13 §8 仍有 4 项联合验收未在发布批次内重跑（跨会话全表面隔离、真实对话全链路、`!`/PTY/Monitor、附件/技能/上下文失败路径），只引用子件证据；这些项保持未勾，同样不具备发布条件。

## 2. 已完成的验收

| 验收项 | 结果 | 证据 |
| --- | --- | --- |
| 干净环境安装、首次启动、版本核对、卸载演练 | 安装/卸载通过；成功首启见 Ghostty 证据 | `evidence/install.txt`、`evidence/ghostty/` |
| 已有 `$DSH_HOME`/profile 的重装/升级演练，安装前后哈希比对 | 通过 | `evidence/install.txt` |
| 配置、凭据、全局指令来源显示 | 安装输出显示路径；首次启动未显示（B5） | 安装输出与 `evidence/install.txt` |
| 未知 dsh 版本显式拒绝 | 通过 | `evidence/install.txt`（安装器与 Rust 预检） |
| 版本矩阵覆盖实际构建与 `0.2.1-rc.2` | 通过 | `version-matrix.md` |
| 双窗口同根会话拒绝、双 root 并发、释放后重新认领 | 通过（4/4 e2e：同根拒绝、双 root 并发、隔离、路由） | `evidence/cross-feature.txt` |
| 两个根会话的 transcript、存储分区与调度表互不可见 | 通过 | `evidence/cross-feature.txt` |
| `nano-schedule-json` 路由与错误路由拒绝 | 通过 | `evidence/cross-feature.txt` |
| 自动回归：Rust workspace + 桥接单测 | 通过（Rust 537、桥接 267 通过 + 5 skip、0 失败） | `evidence/cargo-test.txt`、`evidence/bridge-test.txt` |
| Ghostty 深/浅、60/100 列、全屏、CJK 粘贴、选择复制、滚轮、退出、reduced-motion 启动 | 通过（24/24 检查；真实窗口级截图；真实输入法组合态除外） | `evidence/ghostty/` |
| design-contract 逐项核对 | 12/16 符合、1 部分、3 不符合 | `design-contract-compliance.md` |

## 3. 未勾选的验收（S13 §8）

- [x] 安装包在干净环境完成安装、首次启动、版本核对和卸载演练；已有 DSH_HOME/profile 的重装/升级演练有哈希比对。
- [ ] 首次启动显示配置、凭据和全局指令来源；版本不符给出可操作错误；卸载保留用户数据。— 版本拒绝/卸载通过；首次启动来源显示缺失（B5）。
- [x] 版本矩阵覆盖实际构建与官方 `0.2.1-rc.2`；未知版本被拒绝。
- [x] 双窗口、会话认领、会话切换（S07 证据 + S13 复核）、`nano-schedule-json` 路由、补投组合通过；两个根会话的调度表互不可见。
- [ ] 跨会话内容隔离端到端通过：transcript、草稿、队列、附件、审批、问卷、终端、Monitor、调度都不跨根会话或父子地址。— S13 只验证 transcript、存储与调度；其余面依赖子件证据，未做同批联合复验。
- [ ] 真实对话、工具、差异、问卷、审批、压缩、子代理链路端到端通过。— 子件已有真实后端证据；S13 未在发布批次内重跑全部组合。
- [ ] `!`、PTY、任务停止、Monitor 收件和预算耗尽通过。— 子件证据；S13 未重跑。
- [ ] 附件、技能/规矩、上下文展示的边界与失败路径通过。— 子件证据；S13 未重跑。
- [x] 正常退出、EOF、SIGTERM、崩溃、启动取消的清理结果逐项记录；不误杀独立 dsh。— `evidence/host-integration.txt`（ignored 真实宿主套件）+ `evidence/shutdown-budget.txt` + S13 Ghostty 正常退出。
- [x] Ghostty 全屏下 60/100 列、深浅主题、CJK、选择复制、滚轮、退出通过人工验收。— `evidence/ghostty/` 24/24 检查通过；CJK 为粘贴/显示，真实输入法组合态另列 B4；B1–B3 是对照契约发现的阻断偏差，不能用本项勾选抵消。
- [x] 证据绑定安装产物哈希 + 官方包完整版本/来源；脱敏检查通过。— 不含用户真实路径、用户名、主机名；合成 `/tmp`/`/var/folders` 路径按 S13 §5 允许保留。
- [ ] 已知限制、未测平台和阻断项清单与实际一致；没有未处理的阻断缺陷。— 清单一致；但存在 B1–B5，后半句不成立，故发布阻断。
- [x] 发布证据冻结在 `docs/release/0.1.0/`，可被人独立复核。
- [x] 本份只汇总其他子件证据，不为缺失功能补写实现或演示数据。

## 4. 解除阻断所需动作

1. 输入框上下文占比线与 80/95 阈值：由 S02 的 owner（输入框绘制）按 design-contract 实现并补真实终端证据；上下文数值可联 S07。
2. 主会话 500ms 忙碌指示：由 S02/S12 的 owner 实现并补证据。
3. 浅色 diff token：由 S03 的 owner 按 design-contract 的 light token 落地并补浅色终端截图。
4. 真实输入法组合态：由 S02 的 owner 在 Ghostty + 系统输入法下补录屏/截图；未测前不宣称中文输入法可用。
5. 首次启动来源显示：由 S01/S02 的 owner 在首启界面显示配置、凭据和全局指令的实际来源（安装输出不能替代首启）。

以上完成后重新跑 `packaging/verify-install.sh`、`tests/e2e/cross-feature.test.mjs`、`tests/e2e/ghostty-acceptance.py` 与本目录的符合性核对，再决定是否发布。
