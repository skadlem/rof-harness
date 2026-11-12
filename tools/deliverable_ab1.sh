#!/bin/sh
# Named-deliverable forcing arm: dna-assembly + dna-insert, steer off vs on.
# PAID: 4 rows at the frozen recipe (300k/60/120, exec-policy any), roughly
# $0.10-0.12 each at PEAK price-book rates (about half that off-peak).
# Usage: DEEPSEEK_API_KEY=... tools/deliverable_ab1.sh
# Flags mirror the banked slice-a-any rows except --deliverable*.
set -eu
: "${DEEPSEEK_API_KEY:?set DEEPSEEK_API_KEY (and re-read the live balance first)}"
ROF=${ROF:-"$(dirname "$0")/../target/release/rof"}
TASKS=${TASKS:-"$HOME/.local/share/rof-tb/datasets/tb21-dataset/terminal-bench-2-1"}
OUT=${OUT:-"$HOME/.local/share/rof-tb/work/deliverable-ab1"}
mkdir -p "$OUT/reports"
for task in dna-assembly dna-insert; do
  for steer in off on; do
    row="$task-steer-$steer"
    # skip-guard: a finished row is never re-run (and never re-billed)
    [ -e "$OUT/reports/$row.report.json" ] && { echo "skip $row"; continue; }
    "$ROF" eval --tasks-dir "$TASKS" --ids "$task" --agent --in-container \
      --model deepseek-flash --endpoint https://api.deepseek.com \
      --api-key-env DEEPSEEK_API_KEY \
      --budget-tokens 300000 --budget-steps 60 --budget-actions 120 \
      --exec-policy any --steer off --streak-steer off \
      --deliverable primers.fasta --deliverable-steer "$steer" \
      --out-dir "$OUT/$row" --report "$OUT/reports/$row.report.json" \
      || echo "row $row exited $?"
    echo "$row shadow hits: $(grep -rl deliverable-missing "$OUT/$row" 2>/dev/null | wc -l)"
  done
done
grep -o '"verdict": *"[A-Za-z]*"\|"halt_reason": *"[a-z]*"' "$OUT"/reports/*.report.json
