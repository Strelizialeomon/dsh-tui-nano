# 打包与安装

本目录产出并安装 `dsh-tui-nano` 的首版 macOS 包。包内只有程序与版本声明，不含凭据、会话或用户配置。

## 产物

```text
dsh-tui-nano-0.1.0-arm64/
  bin/nano             Rust 终端程序
  lib/bridge/          薄桥接插件（官方 profile bundle）
  lib/protocol/        版本化协议契约
  install.sh           安装/升级
  uninstall.sh         卸载
  manifest.json        版本矩阵与官方依赖声明
  SHA256SUMS           产物内每个文件的哈希
```

## 构建

前提：macOS arm64、Rust 工具链、Node、`shasum`、`rsync`。

```sh
packaging/build-release.sh
```

输出：`packaging/dist/dsh-tui-nano-0.1.0-arm64/` 与同名 `.tar.gz`；脚本打印压缩包 SHA-256。构建只读仓库源码，不写入用户目录。

## 安装

前提：`dsh` 精确 `0.2.1-rc.2`、Node 主版本 ≥ 22（已验证 v24.18.0）。

```sh
./install.sh
```

默认路径：

| 对象 | 默认 | 说明 |
| --- | --- | --- |
| 入口 | `~/.local/bin/nano` | 安装器生成的包装脚本，设置 `NANO_BRIDGE_PACKAGE` 后执行真实二进制 |
| 安装目录 | `~/.local/share/dsh-tui-nano/<version>` | 版本化；`current` 指向当前版本；`install-receipt.json` 记录产物哈希 |
| 数据根 | `~/.dsh-tui-nano` | 权限 0700；官方会话在 `sessions/`，运行数据按根会话分区 |
| 官方 profile | `${DSH_HOME:-~/.dsh}/profiles/dsh-tui-nano` | 由官方 `dsh plugin` 路径初始化；不覆盖其他 profile |

安装输出会同时打印配置、凭据和全局指令的实际来源：

- 配置：`profiles/dsh-tui-nano/package.json` 与官方 bundle 组成结果。
- 凭据：沿用 `${DSH_HOME:-~/.dsh}` 的官方凭据，Nano 不复制。
- 全局指令：由官方 `agent-instructions` 加载；应用内 `Alt+R` 查看实际来源。

可用参数：`--dsh-home`、`--data-root`、`--install-root`、`--bin-dir`、`--dsh-bin`、`--no-profile-install`、`--json`。

安装器在写入前核对 Node 和 dsh 版本；不匹配时报出当前值与所需值并以非零退出，不静默降级。

## 升级 / 重装

重复运行 `install.sh` 即可：

- 同版本且校验通过：不复制，刷新 `current` 与入口。
- 新版本：复制到新的版本目录，再切换 `current`；不执行 `--from-default-profile`，不删除依赖或 user patch。
- profile 安装是幂等的；已有 `cordis.yml` / `cordis.patch.yml` 只在缺失时创建。

## 卸载

```sh
./uninstall.sh
```

默认只删除入口和安装目录；**保留**数据根、官方 dsh home、profile 与其他 profile。以下删除必须显式选择：

- `--remove-profile`：只删除确属 `@dsh-tui-nano/bridge` 的 profile。
- `--purge-data`：删除 Nano 数据根，包括会话与运行数据。

## 首版边界

- 只构建和验证 macOS arm64；其他架构需自行构建，不在支持矩阵内。
- 只接受官方 dsh `0.2.1-rc.2`；未知版本由 Rust 预检拒绝，入口包装核对 Node 与桥接包版本。
- 安装器不修改 `DSH_HOME` 下的凭据、全局指令、其他 profile 或 `storages/`。
- 包装脚本把安装时的 `--dsh-home` / `--data-root` / `--dsh-bin` 固化为 `DSH_HOME` / `NANO_DATA_ROOT` / `NANO_DSH`，每次启动核对桥接包版本；缺失或不符时给出可操作错误。
