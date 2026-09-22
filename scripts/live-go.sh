#!/usr/bin/env bash
# Live eval run against the OpenCode Go subscription (DeepSeek V4.1 Flash).
#   scripts/live-go.sh [suite] [--limit N] [--jobs N] [any rof eval flags]
#   scripts/live-go.sh eval/suites/repo-tasks.json --limit 6 --jobs 2
#
# Key (never stored in the repo — pick one):
#   export ROF_TOKEN="your-go-key"          # session env (preferred), or
#   echo "your-go-key" > ~/.rof/go.key      # chmod 600 file the script reads
# Get the key via opencode `/connect` -> OpenCode Go -> opencode.ai/auth.
# Model IDs: https://opencode.ai/docs/go/ (V4.1 Flash = deepseek-flash).
#
# Single-model default: ctx and exec both run deepseek-flash (see STATUS
# 2026-09-22 entry — the cheap tier never earned its keep in any arm).
# Two-tier A/B: ROF_CTX_MODEL=deepseek-v4-flash scripts/live-go.sh ...
set -euo pipefail
cd "$(dirname "$0")/.."

SUITE="${1:-eval/suites/repo-tasks.json}"
[ $# -gt 0 ] && shift

RUNS_DIR="${ROF_RUNS_DIR:-$HOME/rof-runs}"
mkdir -p "$RUNS_DIR"

# Token: an existing ROF_TOKEN wins; otherwise a local key file outside the repo.
if [ -z "${ROF_TOKEN:-}" ]; then
  if [ -f "$HOME/.rof/go.key" ]; then
    ROF_TOKEN="$(tr -d ' \t\n' < "$HOME/.rof/go.key")"
  fi
  [ -n "${ROF_TOKEN:-}" ] || {
    echo "no ROF_TOKEN set and no key in ~/.rof/go.key" >&2
    echo "paste your Go key: export ROF_TOKEN=<key>  (or write it to ~/.rof/go.key)" >&2
    exit 1
  }
fi
export ROF_TOKEN
# rof appends /chat/completions itself — base stops at /v1.
export ROF_CHAT_BASE="${ROF_CHAT_BASE:-https://opencode.ai/zen/go/v1}"
export ROF_CTX_MODEL="${ROF_CTX_MODEL:-deepseek-flash}"
export ROF_EXEC_MODEL="${ROF_EXEC_MODEL:-deepseek-flash}"
export ROF_WORKDIR="${ROF_WORKDIR:-$PWD}"
export ROF_TASK_ROOT="${ROF_TASK_ROOT:-$RUNS_DIR}"
export ROF_ALLOW_CMDS="${ROF_ALLOW_CMDS:-cargo check,cargo test}"

TAG="$(date +%m%d-%H%M)"
export ROF_TRACE="${ROF_TRACE:-$RUNS_DIR/trace-$TAG.jsonl}"

cargo build --release
./target/release/rof eval "$SUITE" --report "$RUNS_DIR/report-$TAG.json" "$@"

echo "trace:  $ROF_TRACE"
echo "report: $RUNS_DIR/report-$TAG.json"
echo "copies: rm -rf $ROF_TASK_ROOT/rof-task-*"
