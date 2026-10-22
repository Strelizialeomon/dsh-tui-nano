#!/usr/bin/env bash
# Drive the Nano TUI in a real tmux PTY for S10 Monitor evidence.
#
# Evidence tooling, not part of the product. It uses a temporary DSH_HOME with
# a copy of the user's credentials (so the real profile is not modified), starts
# a real model session, asks the model to run a background job, attaches a quiet
# monitor from the retained beginning with a literal filter, then opens one user
# turn and shows the durable receipt.
#
# Run after `cargo build -p nano-app`:
#   scripts/pty-evidence-s10.sh > docs/evidence/2026-10-22-s10/pty-evidence.txt
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${NANO_BIN:-$ROOT/target/debug/nano}"
SESSION="nano-s10-evidence-$$"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/nano-s10-pty.XXXXXX")"
DSH_HOME="$TMP/dsh"
DATA_ROOT="$TMP/data"
MODEL="${NANO_EVIDENCE_MODEL:-deepseek-flash}"
PROVIDER="${NANO_EVIDENCE_PROVIDER:-deepseek-official}"

cleanup() {
  tmux kill-session -t "$SESSION" 2>/dev/null || true
  rm -rf "$TMP"
}
trap cleanup EXIT

mkdir -p "$DSH_HOME"
cp "$HOME/.dsh/.credentials.yaml" "$DSH_HOME/.credentials.yaml" 2>/dev/null || true
cp "$HOME/.dsh/.anonymous-user-id" "$DSH_HOME/.anonymous-user-id" 2>/dev/null || true
chmod 600 "$DSH_HOME/.credentials.yaml" 2>/dev/null || true

capture() {
  printf '\n===== %s =====\n' "$1"
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
  while [ "$tries" -lt 180 ]; do
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

tmux new-session -d -s "$SESSION" -x 160 -y 48 \
  "cd '$ROOT' && '$BIN' --install --dsh-home '$DSH_HOME' --data-root '$DATA_ROOT' --provider '$PROVIDER' --model '$MODEL'; echo EXIT:\$?; sleep 120"
wait_ready
capture "启动：已连接（临时 DSH_HOME，真实凭据副本）"

send_line "只做一件事：用 bash 工具运行命令 \`sh -c 'sleep 4; echo monitor-line-1; echo noise-ignore-me; sleep 1; echo monitor-line-2; sleep 1; echo monitor-line-3'\`，并把 run_in_background 设为 true。不要运行其他命令，不要读写文件。启动后用一句很短的话回复 job id，然后结束这一轮。"
sleep 18
capture "模型已启动后台任务并回复 job id"

tmux send-keys -t "$SESSION" C-t
sleep 1
capture "任务面板：官方 jobs 列表"

tmux send-keys -t "$SESSION" Down Down m
sleep 1
capture "按 m：附着监听表单，显示已有 N 字节未读与范围/模式选择"

tmux send-keys -t "$SESSION" a t f
sleep 0.3
tmux send-keys -t "$SESSION" -l "monitor-line"
tmux send-keys -t "$SESSION" Enter
sleep 0.3
capture "选择从开头、quiet 模式、字面过滤 monitor-line"

tmux send-keys -t "$SESSION" Enter
sleep 2
capture "创建监听：quiet 模式、官方回执状态与预算；终态只进 UI"

tmux send-keys -t "$SESSION" Escape
sleep 0.3
tmux send-keys -t "$SESSION" Escape
sleep 0.3
capture "返回对话；监听消息已入队但 quiet 未唤醒"

send_line "刚才的监听通知里出现了哪些 monitor-line 行？只列出数字，不要调用工具。"
sleep 15
capture "普通用户回合唤醒后：模型看到注入内容并回答数字"

tmux send-keys -t "$SESSION" C-t
sleep 1
tmux send-keys -t "$SESSION" Down Down m
sleep 1
capture "重新打开监听：durable user/message 回执 accepted；role=user / source=monitor"

tmux send-keys -t "$SESSION" s
sleep 1
capture "按 s 停止监听：任务未受影响，监听记录保留"

tmux send-keys -t "$SESSION" Escape
sleep 0.3
tmux send-keys -t "$SESSION" C-c
for _ in $(seq 1 30); do
  if tmux capture-pane -t "$SESSION" -p 2>/dev/null | grep -q "EXIT:"; then
    break
  fi
  sleep 1
done
printf '\n===== 已发送 Ctrl+C，退出后的 pane =====\n'
tmux capture-pane -t "$SESSION" -p 2>/dev/null | grep -v '^[[:space:]]*$' | tail -5 || true
