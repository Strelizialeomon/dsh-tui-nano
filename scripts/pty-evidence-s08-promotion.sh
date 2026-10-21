#!/usr/bin/env bash
# One authorized real-model TUI session for S08 promotion + job panel evidence.
#
# Evidence tooling, not a product. It installs a throwaway dsh profile in a
# temporary DSH_HOME, symlinks (does not copy) the real credentials file for
# the run, asks the model to run a foreground command with a short tool
# timeout so tool-bash promotes it to a background job, then opens the real
# task panel, follows its output, stops it, and captures the official terminal
# state. The user's existing profiles are not touched.
#
# Usage (user authorization required; uses the official credentials and costs
# model tokens):
#   NANO_EVIDENCE_MODEL=deepseek-flash scripts/pty-evidence-s08-promotion.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${NANO_BIN:-$ROOT/target/debug/nano}"
REAL_DSH_HOME="${DSH_HOME:-$HOME/.dsh}"
SESSION="nano-s08-promo-$$"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/nano-s08-promo.XXXXXX")"
DSH_HOME_DIR="$TMP/dsh"
DATA_ROOT="$TMP/data"
WORK="$TMP/work"
MODEL="${NANO_EVIDENCE_MODEL:-deepseek-flash}"
PROVIDER="${NANO_EVIDENCE_PROVIDER:-deepseek-official}"
mkdir -p "$WORK" "$DSH_HOME_DIR"

cleanup() {
  tmux kill-session -t "$SESSION" 2>/dev/null || true
  rm -rf "$TMP"
}
trap cleanup EXIT

capture() {
  printf '\n===== %s =====\n' "$1"
  tmux capture-pane -t "$SESSION" -p \
    | sed -e 's/[[:space:]]*$//' \
    | awk '{ line[NR] = $0; if ($0 != "") last = NR } END { for (i = 1; i <= last; i++) print line[i] }' \
    | cat -s
}

pane_contains() {
  tmux capture-pane -t "$SESSION" -p 2>/dev/null | grep -q "$1"
}

wait_for_text() {
  local text="$1" tries="$2" index=0
  while [ "$index" -lt "$tries" ]; do
    if pane_contains "$text"; then return 0; fi
    index=$((index + 1))
    sleep 1
  done
  return 1
}

credentials_before="$(shasum "$REAL_DSH_HOME/.credentials.yaml" | awk '{print $1}')"
ln -s "$REAL_DSH_HOME/.credentials.yaml" "$DSH_HOME_DIR/.credentials.yaml"

node "$ROOT/bridge/profile/install.mjs" \
  --dsh-home "$DSH_HOME_DIR" \
  --data-root "$DATA_ROOT" \
  --bridge-package "$ROOT/bridge" \
  --json >/dev/null
printf 'PROFILE {"model":"%s/%s","dshHome":"temporary"}\n' "$PROVIDER" "$MODEL"

read -r -d '' PROMPT <<'EOF' || true
只做一件事：用 bash 工具前台运行命令 `sh -c 'echo s08-promo; sleep 30'`，并把 timeoutMs 设为 1000；不要设置 run_in_background，不要运行其他命令，不要读写文件。然后简短说明结果。
EOF

tmux new-session -d -s "$SESSION" -x 150 -y 45 \
  "cd '$WORK' && '$BIN' --dsh-home '$DSH_HOME_DIR' --data-root '$DATA_ROOT' --cwd '$WORK' --provider '$PROVIDER' --model '$MODEL'; echo EXIT:\$?; sleep 120"
if ! wait_for_text "已连接" 90; then
  echo "nano did not connect" >&2
  capture "启动失败"
  exit 1
fi
capture "启动：已连接"

tmux send-keys -t "$SESSION" -l "$PROMPT"
tmux send-keys -t "$SESSION" Enter

if ! wait_for_text "任务 [1-9]" 150; then
  echo "no job appeared in the header" >&2
  capture "未观察到任务"
  exit 1
fi
capture "promotion：前台命令已转成后台 job（header 显示任务数）"

tmux send-keys -t "$SESSION" C-t
sleep 1
capture "任务面板：真实 job 列表"

tmux send-keys -t "$SESSION" Enter
sleep 1.5
capture "任务详情：非消费跟随到 s08-promo 输出"

tmux send-keys -t "$SESSION" k
sleep 1
capture "停止请求：已请求停止，等待官方终态"

if wait_for_text "被终止" 20; then
  capture "官方终态：被终止"
else
  capture "未在 20s 内看到终态"
fi

tmux send-keys -t "$SESSION" Escape
sleep 0.3
tmux send-keys -t "$SESSION" C-c
for _ in $(seq 1 30); do
  if pane_contains "EXIT:"; then break; fi
  sleep 1
done
printf '\n===== 退出 =====\n'
tmux capture-pane -t "$SESSION" -p 2>/dev/null | grep -E "EXIT:|nano:" | tail -3 || true

if [ -n "${NANO_EVIDENCE_LOG:-}" ]; then
  mkdir -p "$(dirname "$NANO_EVIDENCE_LOG")"
  : > "$NANO_EVIDENCE_LOG"
  while IFS= read -r file; do
    case "$file" in
      *.zstd) zstd -dc "$file" >> "$NANO_EVIDENCE_LOG" 2>/dev/null || true ;;
      *) cat "$file" >> "$NANO_EVIDENCE_LOG" ;;
    esac
  done < <(find "$DATA_ROOT/sessions" -type f \( -name '*.jsonl' -o -name '*.jsonl.zstd' \) | sort)
  printf 'SESSION_LOG %s (%s lines)\n' "$NANO_EVIDENCE_LOG" "$(wc -l < "$NANO_EVIDENCE_LOG" | tr -d ' ')"
fi

credentials_after="$(shasum "$REAL_DSH_HOME/.credentials.yaml" | awk '{print $1}')"
printf 'CREDENTIALS_UNCHANGED %s\n' "$([ "$credentials_before" = "$credentials_after" ] && echo true || echo false)"
