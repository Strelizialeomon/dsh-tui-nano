# S11 证据（2026-10-21）

## 已执行

- `cargo test --workspace`：12 个测试目标全绿；nano-schedule 25 项（表单、CAS、无改动 update、历史标志、折叠分组、失败夹具）、nano-app 44 项（含调度覆盖层与失败路径）、nano-conversation 52+5 项、nano-runtime 18+10+2 项、nano-skills 20+5 项，15 项真实宿主回归仍为 ignored。原始输出见 `rust-tests.txt`。
- `node --test bridge/test/*.test.mjs`：98 项通过（合并 origin/main 的 S05 后全量重跑）。调度 13 项见 `bridge/test/schedule.test.mjs`：0600/0700 租约、同 pid 幂等、活锁拒绝且不删锁、死 pid 回收、损坏锁 30 秒门槛、只释放自己的锁、原子发布无临时文件、**两进程并发回收同一死锁只有一个 owner**、RPC 委托、跨根地址拒绝、官方错误码透传、history 游标、写失败夹具。见 `bridge-tests.txt`。
- `NANO_E2E=1 node --test bridge/test/integration/host.test.mjs`：2 项通过，确认新增调度启动没有破坏 S01/S02 的真实宿主、订阅回放和干净关停。见 `host-e2e.txt`。
- `NANO_E2E=1 node --test bridge/test/integration/schedule.test.mjs`：6 项真实宿主用例全部通过，见 `e2e-schedule.txt`：
  1. 六种选择器经官方 `ctx.schedule.create` 创建；空标题、空指令返回 `invalid_prompt`，非法 IANA 时区返回 `invalid_time_zone`，非法 cron 返回 `invalid_rule`，都带官方可读原因；`catalog` 同时返回活跃/已结束，`list` 在一次性任务结束后不再包含它；`history` 空页的两个裁剪标志分开返回，`limit: 0` 返回 `invalid_rule`；带预期记录的修改成功，第二次用旧记录提交返回 `schedule_conflict` 且未覆盖已提交标题；删除后 `history` 返回 `schedule_not_found`，再次删除返回 `deleted:false`。
  2. 一次性任务真实到期：目录显示 `inactive` + `lastDelivery`（`scheduledAt`/`deliveredAt`/`messageId`），投递历史恰好 1 条且 messageId 与最近回执一致。回执只表示「进入 inbox」。
  3. 冷重启补投：创建 `every_seconds: 60` 后关宿主，真实等待 **130 秒（> 2 个周期）** 再以 `resume` 启动；任务保持 active、只产生 1 条投递记录，证明只补最近一次而不是逐次补跑。同一轮里另建一个 `at +4s` 的一次性任务，停机越过目标后重启仍被投递并留下 1 条回执。
  4. 同一根会话的 `schedule.lock` 被活进程持有时，启动返回 `occupied`/`holderPid`，锁文件原样保留；宿主没有挂调度。
  5. `routes.schedule` 指回 `json` 时启动返回 `unsupported` 且不生成 `schedule.json`；`routes` 指向没有 service key 的后端时，`ctx.storageDomain` 注入超时后返回 `unsupported`，同样不挂调度；损坏的 `nano_probe.json` 会被删除并重试，启动继续且探针文件被清理。
  6. 两个根会话的 `schedule.json` 各自只含自己的任务，同一会话重启读到同一张表。
- `NANO_SCHEDULE=1 python3 scripts/pty-evidence.py`：真实 PTY（120×40）启动真实 dsh profile，Ctrl+G 打开调度覆盖层、`n` 新建、Tab 填写标题/指令、官方确认后列表出现「巡检提醒」；再新建一条并用 ← 把时间类型切到「每日」，列表显示「每日 09:00:00.000」；列表常驻「可能再次投递」和「归档会删除全部活跃任务」提示；Esc 关闭、Ctrl+C 干净退出。屏幕快照见 `pty-schedule.txt`。
- 失败夹具：`bridge/test/fixtures/schedule-post-flush-write-failure.json` 表示「flush 已确认、任务行写入失败」后的官方状态：任务仍 active、没有新回执、history 为空。Rust 模型测试和桥接测试都读取它，断言界面不显示成功、保留「可能重复投递」并显示「最近回执：暂无」。这是**界面级**夹具，不声称注入了官方内部的写失败。

## 未执行（不得勾选）

- flush 后任务写失败的**官方内部**故障注入：官方明确「非原子、可能重复」；上面的夹具只覆盖界面如何呈现这个事实，没有制造真实写失败。
- 投递后的真实模型执行：E2E 宿主没有 provider/model，模型回合以 `has no provider/model` 结束；回执在官方 flush 后提交。E2E 只断言 `lastDelivery`/history，没有断言回执与 turn 的先后时序。
- DST 空档与重叠：官方按「缺失本地时间跳过、重叠取较早瞬间」处理；本份未在夏令时切换日实测，只覆盖 `Asia/Shanghai`（无夏令时）与 UTC 目标。
- 归档 API 级行为（有活跃任务时归档被拒、`session-stop` 删除全部任务、取消归档不恢复）：本 profile 未组合 workspace 归档/停止控制器，未执行；界面只显示静态后果文案。
- history 的 newest-first 与多记录 `before` 游标：官方契约承担，本份只有单条记录的真实用例；界面分页 append 有单元测试。
- 历史裁剪（`earlierRecordsUnavailable` / `earlierRecordsPruned` 由真实裁剪产生）：界面分开显示有单元测试；没有制造超过 30 天/200 条的保留窗口。
- workspace/projection cache 与共享 `sessions/` 的隔离：本份只验证调度表按根会话隔离；那两个 domain 属 S01/S07。
- Ghostty 全屏下的真实视觉/键位验收：PTY 快照只证明文本布局和交互链路，最终键位由 S13 design-contract 冻结。
- 暂停/恢复：官方无 API，按规格不提供，未做。

## 复现

```sh
cargo build -p nano-app
cargo test --workspace
node --test bridge/test/*.test.mjs
NANO_E2E=1 node --test bridge/test/integration/host.test.mjs
NANO_E2E=1 node --test bridge/test/integration/schedule.test.mjs   # 含 130 秒真实补投等待
NANO_SCHEDULE=1 python3 scripts/pty-evidence.py
```

`scripts/pty-evidence.py` 使用临时 `DSH_HOME` 与数据根；E2E 用例各自建临时目录，不读写用户的 `~/.dsh` 会话。
