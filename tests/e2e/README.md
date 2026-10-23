# S13 跨功能 e2e

这些测试只属于发布验收，不替代各子件的单元/契约测试。全部使用合成 HOME / DSH_HOME / 数据根；不读写用户真实凭据或会话。

前置：`zstd` CLI（官方会话日志是多帧 zstd，Node 内置 zstd 只解第一帧）、Pillow（Ghostty 截图差分）。

## 真实宿主跨功能

```sh
NANO_E2E=1 node --test tests/e2e/cross-feature.test.mjs
```

覆盖：

- 同一根会话的第二个窗口在挂载账本处被拒绝，干净退出后释放并可重新认领。
- 两个根会话的 transcript（真实 submit + 本地 mock 模型）、存储分区和官方调度表互不可见。
- `nano-schedule-json` 路由可用；错误路由显式拒绝，不回落。

前置：`dsh` 精确 `0.2.1-rc.2`、Node、可用的 profile 安装（测试内临时安装）。`NANO_BRIDGE_PACKAGE` 指向安装产物的 `lib/bridge` 时，测试的是发布包而不是仓库源码。

## 真实 Ghostty

```sh
python3 tests/e2e/ghostty-acceptance.py \
  --payload packaging/dist/dsh-tui-nano-0.1.0-arm64
```

前置：macOS 辅助功能权限（System Events 发键）与屏幕录制权限（窗口级截图）、Ghostty 1.3.1、已构建的 payload。

脚本只截取本次运行拥有的窗口（`screencapture -l`），截图前把窗口尺寸固定为 100/60 列；模型响应来自本地 mock，不产生费用。截图写入 `docs/release/<version>/evidence/ghostty/`。运行前会保存用户剪贴板，结束时恢复；若无法把目标窗口置前，脚本直接中止，不会把按键发给前台应用。

## 主题配色与透明

```sh
python3 tests/e2e/ghostty-theme-transparency.py
```

覆盖 2 种 TUI 主题 × 2 套故意错配的终端调色板 × 60/100 列的 8 组颜色独立性，以及主体透明、不透明回归、组件/退出与临时配置哈希。结果写入 `docs/evidence/2026-10-22-theme-transparency/`。

受限环境可加 `--capture-only`：跳过键盘输入，只抓首帧并按 PID 关闭窗口，不能替代完整证据。

## 边界

- `--test-name-pattern` 可单跑某个跨功能用例。
- `NANO_KEEP=1` 保留失败现场（合成临时目录）。
- Ghostty 的 Alt 快捷键依赖 `--macos-option-as-alt=true`；窗口尺寸依赖 `--window-save-state=never`。两者都是测试夹具设置，不是产品要求。
