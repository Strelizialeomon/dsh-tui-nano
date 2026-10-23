# S13 跨功能 e2e

这些测试只属于发布验收，不替代各子件的单元/契约测试。全部使用合成 HOME / DSH_HOME / 数据根；不读写用户真实凭据或会话。

## 默认：无头 PTY（TUI 输入/配色）

```sh
cargo build -p nano-app
python3 tests/e2e/theme-pty.py
```

- 用 `pty.openpty` 启动真实 `nano`，设置行列，直接向 PTY 写按键；解析输出的 SGR `38;2` / `48;2` 真彩色。
- **不开窗口、不抢焦点、不改剪贴板**；可以在用户正常工作时并行跑。
- 覆盖：深浅主题的 `fg` / `muted` / `accent` / `rule`，命令面板 `selected_bg`，决策卡 `accent`，diff 增删背景，dock `hover_bg`，普通背景 Reset，无 ANSI 命名色 / Indexed，`TERM` 变化不改变 Nano 发出的 RGB。
- 局限：PTY 没有窗口合成，测不了透明、壁纸、字体渲染和原生全屏；这些只由 Ghostty 截图或隔离会话补。

## 真实宿主跨功能

```sh
NANO_E2E=1 node --test tests/e2e/cross-feature.test.mjs
```

覆盖：

- 同一根会话的第二个窗口在挂载账本处被拒绝，干净退出后释放并可重新认领。
- 两个根会话的 transcript（真实 submit + 本地 mock 模型）、存储分区和官方调度表互不可见。
- `nano-schedule-json` 路由可用；错误路由显式拒绝，不回落。

前置：`dsh` 精确 `0.2.1-rc.2`、Node、可用的 profile 安装（测试内临时安装）。`NANO_BRIDGE_PACKAGE` 指向安装产物的 `lib/bridge` 时，测试的是发布包而不是仓库源码。

## 真实 Ghostty（按需，非默认）

GUI 脚本会启动窗口、抢前台/键盘（`open -g` 也挡不住新 Ghostty 实例激活），并可能使用剪贴板。**默认不跑**；只在以下情况跑：

- 需要终端合成、透明、字体等 PTY 覆盖不到的证据；
- 在独立 macOS 用户会话或 VM 中；
- 或用户明确说当前空闲、可以占用桌面。

跑前必须让用户确认，禁止后台自动启动。脚本只截取本次运行拥有的窗口（`screencapture -l`）；模型响应来自本地 mock，不产生费用。

### 发布验收

```sh
python3 tests/e2e/ghostty-acceptance.py \
  --payload packaging/dist/dsh-tui-nano-0.1.0-arm64
```

前置：macOS 辅助功能权限（System Events 发键）与屏幕录制权限（窗口级截图）、Ghostty 1.3.1、已构建的 payload。截图写入 `docs/release/<version>/evidence/ghostty/`。

### 主题透明

```sh
python3 tests/e2e/ghostty-theme-transparency.py
```

用于终端合成/透明/8 组调色板矩阵；结果写入 `docs/evidence/2026-10-22-theme-transparency/`。受限环境可加 `--capture-only`，只抓首帧并按 PID 关闭窗口。

## 边界

- `--test-name-pattern` 可单跑某个跨功能用例。
- `NANO_KEEP=1` 保留失败现场（合成临时目录）。
- Ghostty 的 Alt 快捷键依赖 `--macos-option-as-alt=true`；窗口尺寸依赖 `--window-save-state=never`。两者都是测试夹具设置，不是产品要求。
- 无头 PTY 是默认回归；Ghostty 截图是补充证据，不与 PTY 结果互相替代。
