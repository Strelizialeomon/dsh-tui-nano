# S11 证据（2026-10-21）

## 已执行

- `cargo test --workspace`：11 个测试目标全绿；nano-schedule 14 项（表单、CAS、历史标志、折叠分组、租约状态机）、nano-app 27 项、nano-conversation 51+5 项、nano-runtime 18+10+2 项，15 项真实宿主回归仍为 ignored。原始输出见 `rust-tests.txt`。
- `node --test bridge/test/*.test.mjs`：75 项通过。新增 `schedule.test.mjs` 10 项：0600/0700 租约、同 pid 幂等、活锁拒绝且不删锁、死 pid 回收、损坏锁 30 秒门槛、只释放自己的锁、RPC 委托、跨根地址拒绝、官方错误码透传、history 游标。见 `bridge-tests.txt`。
- `NANO_E2E=1 node --test bridge/test/integration/host.test.mjs`：2 项通过，确认新增调度启动没有破坏 S01/S02 的真实宿主、订阅回放和干净关停。见 `host-e2e.txt`。
- `NANO_E2E=1 node --test bridge/test/integration/schedule.test.mjs`：5 项真实宿主用例全部通过，见 `e2e-schedule.txt`：
  1. 六种时间选择器全部经官方 `ctx.schedule.create` 创建；空标题返回 `invalid_prompt`、非法 cron 返回 `invalid_rule`，两者都带官方可读原因；`catalog` 同时返回活跃/已结束，`list` 只返回活跃；`history` 空页的两个裁剪标志分开返回，`limit: 0` 返回 `invalid_rule`；带预期记录的修改成功，第二次用旧记录提交返回 `schedule_conflict` 且未覆盖已提交标题；删除后 `history` 返回 `schedule_not_found`，再次删除返回 `deleted:false`。
  2. 一次性任务真实到期：目录显示 `inactive` + `lastDelivery`（`scheduledAt`/`deliveredAt`/`messageId`），投递历史恰好 1 条且 messageId 与最近回执一致。回执只表示「进入 inbox」。
  3. 冷重启补投：创建 `every_seconds: 60` 后关宿主，真实等待 65 秒再以 `resume` 启动；任务保持 active、只产生 1 条投递记录，证明周期任务只补最近一次。
  4. 同一根会话的 `schedule.lock` 被活进程持有时，启动返回 `occupied`/`holderPid`，锁文件原样保留；宿主没有挂调度。
  5. `routes.schedule` 指回 `json` 时启动返回 `unsupported` 且不生成 `schedule.json`；`routes` 指向不存在的后端时，`ctx.storageDomain` 注入超时后返回 `unsupported`，同样不挂调度；两个根会话的 `schedule.json` 各自只含自己的任务，同一会话重启读到同一张表。
- `NANO_SCHEDULE=1 python3 scripts/pty-evidence.py`：真实 PTY（120×40）启动真实 dsh profile，Ctrl+G 打开调度覆盖层、`n` 新建、Tab 填写标题/指令、进入官方确认后列表出现「巡检提醒」，状态栏显示「任务已由官方确认创建」，Esc 关闭、Ctrl+C 干净退出。屏幕快照见 `pty-schedule.txt`。

## 未执行（不得勾选）

- flush 后任务写失败的重复投递故障注入：官方明确「非原子、可能重复」，界面常驻静态提示；本份没有制造真实写失败，因此「重复投递」只有官方语义与文案，没有故障夹具。
- 投递后的真实模型执行：E2E 宿主没有 provider/model，模型回合以 `has no provider/model` 结束；回执在 turn 开始前已落盘，符合「回执 = 进入 inbox，不是模型已执行」。真实模型费用/权限行为归 S03/S04/S13 联合验收。
- DST 空档与重叠：官方按「缺失本地时间跳过、重叠取较早瞬间」处理；本份未在夏令时切换日实测，只覆盖 `Asia/Shanghai`（无夏令时）与 UTC 目标。
- 归档 API 级行为（有活跃任务时归档被拒、`session-stop` 删除全部任务、取消归档不恢复）：本 profile 未组合 workspace 归档/停止控制器，未执行。
- 历史裁剪（`earlierRecordsUnavailable` / `earlierRecordsPruned` 由真实裁剪产生）：界面分开显示有单元测试；没有制造超过 30 天/200 条的保留窗口。
- Ghostty 全屏下的真实视觉/键位验收：PTY 快照只证明文本布局和交互链路，最终键位由 S13 design-contract 冻结。
- 暂停/恢复：官方无 API，按规格不提供，未做。

## 复现

```sh
cargo build -p nano-app
cargo test --workspace
node --test bridge/test/*.test.mjs
NANO_E2E=1 node --test bridge/test/integration/host.test.mjs
NANO_E2E=1 node --test bridge/test/integration/schedule.test.mjs   # 含 65 秒真实补投等待
NANO_SCHEDULE=1 python3 scripts/pty-evidence.py
```

`scripts/pty-evidence.py` 使用临时 `DSH_HOME` 与数据根；E2E 用例各自建临时目录，不读写用户的 `~/.dsh` 会话。
