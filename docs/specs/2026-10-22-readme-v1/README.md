# [2026-10-22-readme-v1] 公开 README 重写

- 组标识：`2026-10-22-readme-v1`；角色：单份 spec（不拆），同时承担总纲与执行规格。
- 状态：草案（待合入）；执行单：待开。
- 上游：`2026-10-20-nano-v1` 的 S01–S12 已实现；0.1.0 发布验收未闭合；视觉对齐第一批已合入。
- 下游：无。本份只改公开文档，不改代码、协议、桥接、官方服务或发布结论。
- 合入前提：spec 合并后开执行 issue；实施只动 `README.md` 与新增 `README.en.md`，不新增 LICENSE / CONTRIBUTING / CI。
- 替代关系：不取代任何已合并 spec；只取代旧 README 的公开叙事。

## 1. 目标与边界

目标：把 `README.md` 从内部工程日志改成公开项目首页，并新增英文镜像 `README.en.md`。首屏必须回答「是什么、长什么样、怎么开始、当前能不能用」；详细状态、结构、测试和边界留在 docs，首页只给摘要和链接。

不做：

- 不改代码、协议、桥接、官方服务、凭据、数据所有权。
- 不新增 `LICENSE`、`CONTRIBUTING`、`SECURITY`、`CODE_OF_CONDUCT`、CI 或 issue 模板（用户本轮明确只改 README）。
- 不宣称 0.1.0 已发布，不改发布结论，不补写未验证的平台或功能。
- 不把设计样稿截图当真实终端证据；只用 `docs/evidence/2026-10-22-visual/batch1/` 的真实 Ghostty 截图。

拥有：`README.md`、`README.en.md`、本 spec。

不碰：`docs/specs/2026-10-20-nano-v1/`、`docs/release/0.1.0/` 的冻结正文、`packaging/`、`crates/`、`bridge/`、`protocol/`。

## 2. 已确认决定

| 决定 | 内容 |
| --- | --- |
| 定位 | Alpha 工具：早期可用、macOS 单平台、从源码构建或运行；不写成练手项目，也不写成正式产品。 |
| 语言 | 中文为主 + 英文镜像；`README.md` 中文，`README.en.md` 英文；如有出入以中文为准。 |
| 范围 | 只改 README；不补许可证、贡献指南、CI。 |
| 结构 | 不拆：1 份 spec + 1 张执行 issue；中文 README 与英文镜像作为一个交付。 |
| 首屏 | Alpha 状态 + 一句话 + 4 个静态徽章 + 真实 Ghostty 截图 + 折叠画廊。 |
| 信息架构 | 目录 → 这是什么 → 功能 → 环境要求 → 快速开始 → 常用键位 → 当前状态 → 已知边界 → 开发与测试 → 架构 → 贡献 → 许可证 → 致谢。 |
| 事实口径 | 只写已核实的平台、版本、功能和边界；未发布就写未发布；不确定的不写。 |

## 3. 现状与问题

以下为本次改写的现状依据；审核时应逐条核对。

- `README.md:3` 只有一句抽象描述；前 8 行没有截图、功能列表或安装路径。
- `README.md:10-26` 是 8 段、17 行的实施流水账，含 S01–S13 编号，夹带 fd 3/4、NDJSON、writer-held 等内部术语；公开读者看不出当前可用性。
- `README.md:28-50` 是 19 行内部模块结构表；`README.md:52-99` 全是开发与测试命令；`README.md:103-121` 是 17 条已知边界。三者都有价值，但不应占据公开首页。
- `Cargo.toml:22` 声明 MIT，但仓库没有 `LICENSE` 文件，GitHub 的 `licenseInfo` 为 null。
- 仓库已有 25 张真实终端截图：`docs/release/0.1.0/evidence/ghostty/` 17 张 + `docs/evidence/2026-10-22-visual/batch1/` 8 张。
- `docs/release/0.1.0/release-conclusion.md:3` 的发布结论是「不发布」；GitHub 没有 Release。
- `crates/nano-app/src/cli.rs:155-193` 是 `nano --help` 的完整键位与参数来源；`packaging/README.md` 是安装与平台前提的来源。

## 4. 读者与首屏

目标读者依次是：

1. 想判断「这个 TUI 值不值得试」的 DeepSeek Harness 用户。
2. 想从源码跑起来、或想贡献的开发者。
3. 想了解项目边界和维护状态的维护者。

首屏约束：

- 标题后必须是一句话定位，不能是内部编号或实现流水账。
- 状态块必须写明：Alpha、未发布、没有官方发布包、唯一验证组合 macOS arm64 + Ghostty 全屏、独立项目且非 DeepSeek 官方产品。
- 徽章只用静态 shields.io：status / platform / dsh / license；不放假 CI、假 release 徽章。
- 主截图用 `docs/evidence/2026-10-22-visual/batch1/03-dark-100-response.png`；折叠画廊用 `02-dark-100-running.png` 与 `08-dark-100-diff-not-applied.png`。
- 首屏不得出现 S01–S13、fd 3/4、NDJSON、writer-held 等内部术语；这些只允许出现在正文后半或链接的 docs 里。

## 5. 章节内容契约

| 章节 | 必须包含 | 禁止 |
| --- | --- | --- |
| 这是什么 | DSH 负责什么；Rust / TypeScript 薄桥接分工；三条设计原则；独立重写声明 | 重写 Agent、伪造会话事实 |
| 功能 | 对话、阅读/决策、输入增强、工作流、会话/控制、模型/权限六组；每组一句 | 把未实现或未验证功能写成可用 |
| 环境要求 | macOS arm64 + Ghostty；dsh 精确 `0.2.1-rc.2`；Node ≥22；Rust stable / 1.82；凭据由官方解析 | 宣称 Windows/Linux/其他终端 |
| 快速开始 | 源码 4 条命令；首次启动装 profile；指定模型的费用提醒；本地安装包路径 | 假 curl 安装、假 GitHub Release |
| 常用键位 | 10–13 条高频键；`nano --help` 给完整表；`!` 权限提醒 | 复制整份 HELP |
| 当前状态 | S01–S12 已实现；视觉第一批已合入、第二批待做；0.1.0 未发布；平台限制 | 把发布阻断说成已解决 |
| 已知边界 | 7 条以内用户可感知边界 + 完整清单链接 | 复制 20 条内部边界 |
| 开发与测试 | 4 条最常用命令 + 真实宿主测试费用提醒 + e2e/证据链接 | 复制完整测试矩阵 |
| 架构 | 一张短图 + 7 行顶层目录表 + 详细链接 | 15 行内部模块表 |
| 贡献 | 个人主导、先开 issue 对齐、提交前测试、无 CI | 承诺响应时间或 SLA |
| 许可证 | MIT、Cargo.toml 引用、独立 LICENSE 文件待补 | 宣称已有 LICENSE 文件 |
| 致谢 | DSH、Ratatui/Crossterm、设计/证据贡献者 | 无 |

## 6. 中英文镜像

- `README.en.md` 与中文版同章节顺序、同事实口径、同图片和链接。
- 英文版可以是完整镜像，不要求逐句直译；但状态、版本、限制、命令不得漂移。
- 英文版顶部写「English mirror；如有出入以中文为准」。
- 中文版顶部放 `[English](README.en.md)`；英文版放 `[中文](README.md)`。

## 7. 验收

- 两份 README 的相对链接和图片路径全部存在；外部链接可解析。
- 4 个徽章 URL 返回 200；不出现 CI / release 徽章。
- 两文件的 H2 集合一致；TOC 覆盖所有 H2，锚点可跳转。
- 首屏无内部编号 / 实现术语；状态块与发布结论一致。
- 快速开始命令与 `nano --help`、`packaging/README.md`、`package.json` 实际脚本一致。
- 英文版与中文版的状态、版本、限制、命令一致；英文版有中文优先声明。
- `git diff --check` 无空白错误；Markdown 表格列数一致。
- 不新增 LICENSE / CONTRIBUTING / CI 等文件；不改代码与冻结文档。

## 8. 完成标准

- 两份 README 按上述契约落地，验收项全部通过。
- 没有未处理的已知阻断缺陷；未验证的平台 / 功能如实标注。
- 不把 0.1.0 发布阻断项说成已解决。
- 交付证据：链接检查输出、徽章 HTTP 200、`git diff --check`、最终 diff。

## 9. 机制清单

无新增机制。本份只重排公开文档，不引入新概念、新抽象、新流程或新脚本。

## 10. 自定细节

- 首屏主截图选 `03-dark-100-response.png`；折叠画廊选 `02-dark-100-running.png` 与 `08-dark-100-diff-not-applied.png`，不选空白的 `01-dark-100-main.png`。
- 徽章用 shields.io 静态徽章，不接 CI；版本写 `0.2.1-rc.2`，平台写 `macOS arm64`。
- 章节顺序按 §2；`目录` 放在截图画廊之后、正文之前。
- 英文版写成完整镜像而非短摘要；顶部声明中文版优先。
- 许可证段写「MIT，文本在 Cargo.toml，独立 LICENSE 文件待补」，不新增 LICENSE 文件。
- 贡献段写「个人主导、先开 issue 对齐、无 CI、人工评审」，不承诺 SLA。
- 当前状态用表格，不用 S01–S13 流水账；正文只在状态表里保留 S01–S12 编号。
- 旧的详细状态、结构表、完整命令、20 条边界从首页移除，靠 git 历史和 docs 链接回溯；不另建归档文件。

## 11. 审核修订记录

| # | 严重度 | 发现 | 处置 |
| --- | --- | --- | --- |
| - | - | - | - |
