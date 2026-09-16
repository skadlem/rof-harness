# Status

Last verified: 2026-09-14, on this working tree (`git log` has the v1 commit; this file is the
running handoff).

## Verified state

| Check | Result |
|---|---|
| `cargo test` | 40 passed, 0 failed |
| `cargo clippy --all-targets` | 0 warnings |
| `cargo fmt --check` | clean |
| `cargo build --release` | green, `target/release/rof` ≈ 5.9 MB |
| Live baseline (DeepSeek `deepseek-chat`, `repo-tasks` first 6, `--jobs 2`) | **matched 3/6 (50%)**, wall 69 s, est $0.011, tool accuracy 95%, cache-hit input 50%, 87k in / 6.4k out |
| Live end-to-end | passes, with real file writes + `cargo check`/`cargo test` evidence in the trace |
| Measured cost | ~$0.002–0.005 per passing task |

Baseline artifact: `docs/baselines/2026-09-14-deepseek-chat-6.json` (the `--report` dump of the
run; trace in `rof-runs/`, not committed). Two independent runs of the same 6 tasks produced
the same match set, so the harness is stable at this sample size.

Per task, and both failure modes are model-side, each with the evidence in its MISMATCH line:

| Task | Result | Why |
|---|---|---|
| tool-count | pass, 1 round | additive method, `cargo check` green |
| procrun-doc | pass, 1 round | doc replacement, `cargo check` green |
| trace-doc | pass, 1 round | doc insertion, `cargo check` green |
| doc-comment | fail, 2 rounds | patch search string guessed wrong → `fs.patch` refused it, 0 writes |
| add-retriever-test | fail, 2 rounds | guessed the `#[cfg(test)]` module text; search string not found |
| metrics-method | fail, 2 rounds | rewrote the file instead of adding a method, clobbered `EvalReport` → `cargo test` exit 101 |

The pattern worth acting on next: all three failures are one behaviour — writing a patch without
reading the file it edits. The harness caught each honestly (refused patch, or compile evidence);
it did not prevent the waste.

Disk: 6 task copies ≈ 3.0 GB (a `cargo test` task owns ~1 GB of `target/`, a `cargo check`
task ~250 MB). Point `ROF_TASK_ROOT` at disk; `ROF_CLEAN_TASKS=yes` for long suites.

## v1 scope: done

- Engine: session, supervisor loop (plan -> implement -> review, bounded rounds), model router
  (Context vs Executor + fallback).
- Agents: planner, implementer, reviewer behind one trait; each pluggable.
- Context: long/mid/short layers, per-layer budgets, head+tail truncation, cheap-model
  compaction on overflow, keyword retrieval with budgets.
- Tools behind a single deny-by-default gate: `fs.list`, `fs.read`, `fs.write`, `fs.patch`,
  `proc.run` (exact allowlist, no shell), `http.get` (host allowlist, no redirects).
- Verification: checks run before review and their output is injected with an explicit
  `STATUS: PASSED/FAILED (exit N)` line; the harness rejects a "pass" on empty work when the
  task expected writes; retries carry previous check output.
- Meta-harness: JSON suites, per-task matched results, append-only JSONL traces, metrics for
  success / tool accuracy / tokens / cache-hit rate / cost (estimated + provider-reported) /
  latency / budget aborts / retries / model errors, `utility = success − λ·cost`.
- **Fan-out**: `max_parallel_tasks` config + `rof eval --jobs N` + `--limit K`. Isolation by
  copy — one scratch copy per task under `task_root` (`target/`, `.git/` excluded), so parallel
  writers never share a tree and the source tree is never a target. Results keep suite order;
  a failed copy fails only its own task.
- Config as an artifact: JSON config file, `rof config` canonical dump, round-trip tested.
- Version control: `git init` + the v1 commit.
- Docs: this file + README + three example configs + a 20-task suite.

## v1 scope: remaining

None. Everything the v1 scope named is built, tested and exercised live. The items below are
the next round, not gaps in this one.

## Known warts (all deliberate, each has a reason)

- The reviewer runs on the executor model — same model as the implementer. Review is
  evidence-gated self-review, not independent verification.
- Plan task count varies run to run for an identical goal, which dominates cost variance.
  A/B comparisons must pin the plan or skip the planner (`ROF_PLANNER=skip`), and a 6-task
  sample is a baseline, not a measurement of a change.
- `expect_writes` is a policy, not a truth: an analysis task must declare
  `expect_writes: false` or the harness will correctly refuse its pass.
- Retrieval is keyword-based; no embeddings (dropped deliberately for zero deps).
- Suites are JSON, not YAML (same reason).
- Fan-out multiplies disk and CPU by the job count: each task builds its own `target/`.
  `--jobs` above ~4 mostly buys contention on a 12-core box with cargo checks in the loop.

## Good next steps (ranked)

1. Spend the baseline on the model, not the harness: run the full 20-task suite once at
   `--jobs 4` and record the matched rate per task class (single-file edit, multi-file,
   read-only analysis). The 3/6 above is the first data point.
2. Independent review: route the reviewer to a second model (the `RoutingConfig` seam exists)
   and measure whether the verdicts change — self-review is the biggest known weakness.
3. Pin the plan for A/B runs: cache the planner's `tasks[]` per goal and replay it, so config
   comparisons hold task count constant.
4. The deferred token-efficiency items that still have no work behind them:
   circuit breaker on identical tool failures, symbol-outline/search read tools,
   retry context decay (context only, never output), read-result cache keyed on (path, mtime, size).
