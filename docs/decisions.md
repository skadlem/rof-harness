# Decisions

Distilled from the code comments and tests that state them. Anything
here marked chosen-to-be-validated lacks a measured anchor; the code
says so explicitly.

- **Deliverable is the full uncut patch.** `rof` prints
  `snapshot::patch_since_start_full()` on stdout; the 8KiB/200-line
  evidence bound stays internal to the loop. A regression test in
  `main.rs` pins this (the old `patch_since_start` cut stdout).
- **Strict vs Lenient eval grading.** `rof eval` grades with Strict
  patch-apply by default; `--lenient-apply` records Lenient provenance
  in the report. Strict grades are not comparable with earlier
  fuzz-lenient numbers.
- **WAL is on by default, fail-closed.** The durable event log lives at
  `<workdir>/.rof-events.jsonl`; `--log-path` moves it, `--no-log`
  disables it and wins over the override.
- **`--compaction` is opt-in and unvalidated.** Absent the flag, the run
  stays byte-identical to the no-compaction default. The S-1 compaction
  experiment has not run; the flag exists for it only.
- **Fold knobs resolve at the edge.** `THINKING_KEEP` /
  `COLLAPSE_HYSTERESIS` fill in only when the matching flag is absent;
  a present-but-unparsable value is a usage error, never a silent
  default. The loop never reads the environment.
- **Workdir guard is refused-by-default.** `/`, `$HOME`, the harness
  checkout, and repos with uncommitted changes are refused (exit 2)
  unless `--allow-dirty-workdir` is passed; pre-existing dirt then
  appears in the reported patch, so dirt never leaks in silently.
- **Agent-leg eval budgets default to the frozen pilot recipe.** `rof eval
  --agent` fills 300000 tokens / 60 steps / 120 actions when the user
  omits the budget flags (chosen: copied from the frozen pilot recipe in
  NEXT.md; validated when the agent-leg slice runs). Explicit flags win,
  and `cli::budget_for` stays the single resolver.
- **The harbor CLI is the agent-leg grading seam.** `rof eval --agent`
  ports the bash pilot driver's mk_goal/verdict verbatim: job yaml (JSON)
  plus `harbor run -c <job> -y -q`; harbor spawn failure, grading timeout
  without result.json, workdir refusal, or a run with no RunEnd in the
  events dump is InfraFailure — infra is never capability. Live-smoked
  2026-10-08 end-to-end (swd, DeepSeek direct, deepseek-flash): current
  harbor writes `reward_stats.reward` as a `{value: [trials]}` histogram,
  not the pilots' scalar — the Rust reader takes both shapes and fails
  closed on anything else (the old bash drivers' reward lines print the
  dict on current harbor; fix their parse or use `rof eval --agent`
  before resuming them).
- **The workdir guard enforces exactly the four refusals.** Root, `$HOME`,
  the harness checkout, and dirty-without-opt-in; disposability of every
  other directory is documented operator responsibility (threat model),
  not an enforced property.
- **Exit codes.** 0 done; 2 usage/parse/workdir refusal; 3 run failure
  (halted, cancelled, provider error — Provider keeps the historical 3);
  4 missing credentials; 5 log failure; 6 snapshot failure; 7 input
  failure.
- **Bets are feature-flagged.** `--bets` opts in; without it the batch
  commits as before. Bet A (proof gating) needs Bet B's claim field, so
  ablation order is B then A; Bet C (candidate racing) stays flagged and
  unbuilt. Read-only batches carry no hunks and commit.
- **Presets.** UnattendedBatch 20/12 is measured; LongTask 40/25 is
  chosen-to-be-validated. The context-pinned 200k token floor is
  chosen-to-validate (pinned-prefix floor ~4-5k tok/req x 20-30 requests
  plus reasoning completions); explicit flags always win.
- **Explicit budget flags own the whole budget.** Any of
  `--budget-steps` / `--budget-actions` / `--budget-tokens` lifts the
  UnattendedBatch 900s wall to the LongTask 3600s allowance: the wall is
  the measured guard for 20-step default batches, never part of a flagged
  recipe (measured 2026-10-08: v0 sweep cells ran 207–816s and all bound
  on the token cap; one fresh 60-step/300k cell wall-halted at 21 steps,
  24% of its token budget — 300s exec timeouts burn the 900s wall in
  three builds). Flag-less runs keep the preset wall.
- **Dump semantics.** `--dump-events PATH` replaces any previous dump at
  PATH (one run, one dump), truncating at start and appending per event.

## Not inferable from code

- The numeric anchors for the LongTask and context-pinned defaults above
  (no measurements in-tree).
- When Strict-by-default eval grading becomes the permanent baseline.
- Whether Bet C will be built, and on what timeline.
