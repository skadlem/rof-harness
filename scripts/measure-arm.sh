#!/usr/bin/env bash
# One arm of a measured comparison: N labelled runs of the same suite.
#   scripts/measure-arm.sh <tag> [runs] [suite] [extra rof eval flags...]
#   scripts/measure-arm.sh before 3 eval/suites/repo-tasks.json --limit 6 --jobs 2
# Writes $ROF_RUNS_DIR/run-<tag>-<i>.json (report) and trace-<tag>-<i>.jsonl
# per run, under ~/rof-runs-<tag> by default. One run per arm proves nothing
# (docs/STATUS.md: a 6-task run swings +-2 tasks) — 3+ runs, same flags.
set -euo pipefail
cd "$(dirname "$0")/.."

TAG="${1:?usage: measure-arm.sh <tag> [runs] [suite] [flags...]}"
RUNS="${2:-3}"
SUITE="${3:-eval/suites/repo-tasks.json}"
[ $# -ge 3 ] && shift 3 || shift $#

DIR="${ROF_ARM_DIR:-$HOME/rof-runs-$TAG}"
mkdir -p "$DIR"
export ROF_RUNS_DIR="$DIR" ROF_TASK_ROOT="$DIR"

for i in $(seq 1 "$RUNS"); do
  export ROF_TRACE="$DIR/trace-$TAG-$i.jsonl"
  echo "=== $TAG run $i/$RUNS -> $DIR"
  scripts/live-eval.sh "$SUITE" "$@"
  # live-eval names the report by minute; copy it to the run's own name so
  # two runs in one minute cannot overwrite each other.
  cp "$(ls -t "$DIR"/report-*.json | head -1)" "$DIR/run-$TAG-$i.json"
  rm -rf "$DIR"/rof-task-*   # one copy per task per run: ~3 GB each
done
echo "reports: $DIR/run-$TAG-*.json"
