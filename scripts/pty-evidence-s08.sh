#!/usr/bin/env bash
# Drive the Nano TUI in a real tmux PTY and print readable screen snapshots.
#
# Evidence tooling, not part of the product. It installs a throwaway dsh
# profile and data root, runs manual `!` commands (first-use notice, exit 0,
# stderr + exit 7, a running command), opens the official task panel, and
# quits with Ctrl+C. No model call and no user credential is used.
#
# Run after `cargo build -p nano-app`:
#   scripts/pty-evidence-s08.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${NANO_BIN:-$ROOT/target/debug/nano}"
SESSION="nano-s08-evidence-$$"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/nano-s08-evidence.XXXXXX")"
DSH_HOME="$TMP/dsh"
DATA_ROOT="$TMP/data"

cleanup() {
  tmux kill-session -t "$SESSION" 2>/dev/null || true
  rm -rf "$TMP"
}
trap cleanup EXIT

capture() {
  printf '\n===== %s =====\n' "$1"
  # Trim trailing whitespace, drop trailing blank rows, and squeeze blank runs
  # so a 45-row screen stays readable in the evidence file.
  tmux capture-pane -t "$SESSION" -p \
    | sed -e 's/[[:space:]]*$//' \
    | awk '{ line[NR] = $0; if ($0 != "") last = NR } END { for (i = 1; i <= last; i++) print line[i] }' \
    | cat -s
}

send_line() {
  tmux send-keys -t "$SESSION" -l "$1"
  tmux send-keys -t "$SESSION" Enter
}

wait_ready() {
  local tries=0
  while [ "$tries" -lt 90 ]; do
    if tmux capture-pane -t "$SESSION" -p 2>/dev/null | grep -q "已连接"; then
      return 0
    fi
    tries=$((tries + 1))
    sleep 1
  done
  echo "nano did not become ready" >&2
  tmux capture-pane -t "$SESSION" -p >&2 || true
  return 1
}

tmux new-session -d -s "$SESSION" -x 150 -y 45 \
  "cd '$ROOT' && '$BIN' --dsh-home '$DSH_HOME' --data-root '$DATA_ROOT'; echo EXIT:\$?; sleep 120"
wait_ready
capture "启动：已连接"

send_line "!printf 'hello\\n'"
sleep 1
capture "首次人工命令：用户权限提示（Enter 执行，Esc 取消）"

tmux send-keys -t "$SESSION" Enter
sleep 1.5
capture "执行后：退出码 0，本窗口显示输出，官方 inbox 出现合成上下文"

send_line "!sh -c 'echo out; echo err >&2; exit 7'"
sleep 1.5
capture "第二条命令：不再提示；非零退出与 stderr 分开显示"

send_line "!sleep 3"
sleep 1
capture "运行中的命令：状态为运行中"
sleep 3
capture "命令结束：退出码 0"

tmux send-keys -t "$SESSION" C-t
sleep 1
capture "任务面板：官方 jobs 列表（此会话没有 Agent 后台任务）"

tmux send-keys -t "$SESSION" Escape
sleep 0.3
tmux send-keys -t "$SESSION" C-c
for _ in $(seq 1 20); do
  if tmux capture-pane -t "$SESSION" -p 2>/dev/null | grep -q "EXIT:"; then
    break
  fi
  sleep 1
done
printf '\n===== 已发送 Ctrl+C，退出后的 pane =====\n'
tmux capture-pane -t "$SESSION" -p 2>/dev/null | grep -v '^[[:space:]]*$' | tail -5 || true
