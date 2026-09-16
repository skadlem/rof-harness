#!/usr/bin/env bash
# Live eval run against DeepSeek with the env the harness needs, one command.
#   scripts/live-eval.sh [suite] [--limit N] [--jobs N] [any rof eval flags]
#   scripts/live-eval.sh eval/suites/repo-tasks.json --limit 6 --jobs 2
# Token: DEEPSEEK_API_KEY from ~/.hermes/.env, read at run time, never echoed.
# One run per arm proves nothing (see docs/STATUS.md) — run each arm 3 times.
set -euo pipefail
cd "$(dirname "$0")/.."

SUITE="${1:-eval/suites/repo-tasks.json}"
[ $# -gt 0 ] && shift

RUNS_DIR="${ROF_RUNS_DIR:-$HOME/rof-runs}"
mkdir -p "$RUNS_DIR"

# Token: an existing ROF_TOKEN wins (any provider, e.g. `source /tmp/atria-creds.sh`);
# otherwise fall back to DeepSeek from ~/.hermes/.env.
if [ -z "${ROF_TOKEN:-}" ]; then
  export ROF_TOKEN="$(grep -m1 '^DEEPSEEK_API_KEY=' "$HOME/.hermes/.env" | cut -d= -f2- | tr -d '"')"
  [ -n "$ROF_TOKEN" ] || { echo "no ROF_TOKEN set and no DEEPSEEK_API_KEY in ~/.hermes/.env" >&2; exit 1; }
fi
export ROF_CHAT_BASE="${ROF_CHAT_BASE:-https://api.deepseek.com}"
export ROF_CTX_MODEL="${ROF_CTX_MODEL:-deepseek-chat}"
export ROF_EXEC_MODEL="${ROF_EXEC_MODEL:-deepseek-chat}"
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
