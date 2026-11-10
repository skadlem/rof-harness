# Research and implementation handoff (2026-11-11)

## Objective and current state

Build competitive coding-agent capability, judged on pinned external tasks and
same-model comparisons. Keep total tokens per solved task at or below 1.3x the
matched reference as a guardrail; cost is not the objective. Current external
reference is Terminal-Bench CPU 4.1.0 (63 tasks, tag `v4.1.0`, commit
`4070d6cef56a7d20cb5dc98d5e10ada88a6b9440`, verified 2026-11-10). Verify the
official release again before any future spend.

The current official-account campaign is stopped. Eight paid rows cover three
tasks: three Session Window failures, three WAL rows (0, 0, 1), and React control
plus interrupted retention treatment. Only the WAL think-retention row is a
clean ROF pass; React treatment has native artifact reward 1 but incomplete
provider usage, so it is not a completed agent result. This is task-level
evidence, not a suite score. The carried upper bound is **$2.158150338**, which
includes the **$0.324172800** unknown-request reservation, against the existing
**$2.82** ceiling. Do not make paid calls or reset the ledger. See
[`current-benchmark-pilot.md`](current-benchmark-pilot.md).

ROF's pilot internal deadline is 3,600s; the task-agent allowance is 28,800s.
Verifier budgets are separate and task-specific (for example, WAL 1,800s and
React 900s). Do not conflate these clocks. Archive-handle and Harbor adapter
implementation is verified: root exercised collapse plus `output_read` end to
end; 166 Rust tests, 4 adapter tests, Clippy and formatting passed (reported
2026-11-11). Benchmark capability remains unmeasured. Defaults stay unchanged.

## Prepared, not run: next experiment protocol

1. Before inference, freeze a fresh development slice and untouched holdouts
   from the then-current official CPU release. Record release/task hashes,
   runtime hash, Harbor overlay hashes, full config, oracle result and budget
   ledger. Require a passing native oracle and release/hash preflight.
2. Freeze the pilot control from `current-benchmark-pilot.md`: 1,200,000 gross
   tokens, 120 steps, 240 actions, observation/thinking keep 5/2, legacy
   workflow, failed-check preservation off, and Harbor `max_tokens=8000`.
   Keep model, task, endpoint, exec policy and all other fields fixed. Each
   treatment changes exactly one: retention value; `--workflow minimal`;
   `--preserve-failed-checks on`; `--archive-output on`; `--shell on`;
   or the starting response cap. Use
   `--exec-policy any` for the shell arm. Current defaults remain unchanged.
3. Preserve failed-check state, final artifacts, native reward, completion,
   per-request usage, retries and wall time separately. Charge every attempt.
   A zero-solve control has no defined tokens-per-solved-task ratio.
4. Confirm a surviving treatment on untouched holdouts. No hidden-verifier
   output may choose runtime actions, procedures, or candidates. Resample the
   control only when testing extra inference (review, reflection, or candidate
   generation) so that added model calls have a token-matched comparison.

Harbor receives adapter options, rather than raw CLI arguments. These are the
single-field treatment changes to the fixed control options:

| Experiment | Harbor option change |
|---|---|
| Observation retention | `observation_keep: 32` |
| Reasoning retention | `thinking_keep: 256` |
| Minimal workflow | `workflow: "minimal"` |
| Preserve executed failing checks | `preserve_failed_checks: true` |
| Archived output retrieval | `archive_output: true` |
| Shell composition | `shell: true` |
| Response allocation, if traces show truncation | `max_tokens: 16000` |

These values are chosen experimental settings; paired native task outcomes
will determine whether they help. Select an experiment within verified remaining headroom; this table
does not schedule all arms. Freeze a newly built runtime for both arms and
record its hash. The historical pilot binary and results remain unchanged.

These are prepared recipes, not benchmark runs or claimed results. The archive
cap is 64 MiB per command, per `crates/tools-std/src/runner.rs`. For the archive
arm, verify retrieval past the 120-character preview through the returned
run-owned handle. For failed-check preservation,
check that an executed failing check remains failed and the edits persist; do
not count a nonexecuted check as proof. Harbor `max_tokens` changes the first
request cap (default 8,000); provider retry escalation is unchanged. No hidden
verifier inputs, paid run, or new result is authorized by these recipes.

## Proposed task slice and remaining readiness work

The [machine-readable task record](next-benchmark-slice.json) proposes development
tasks `freight-dispatch-shift` and `cargo-flight-dispatch`, with
`coq-block-bound` and `vf2-speedup-networkx` reserved prospectively as holdouts.
Selection used public metadata and instructions for coding relevance and resource
fit. It is a convenience slice; historical exposure auditing is incomplete.
Do not call these tasks historically unseen or representative of the suite.

All eight public metadata/instruction hashes were checked against the pinned
source manifest on 2026-11-11. Complete task packages, images, and native oracle
results remain unvalidated. A read-only host snapshot showed 12 CPUs, about
15.9 GB total Docker memory, and 3.6 GB free disk. The selected task metadata
requests 2 CPUs and 4–8 GiB RAM, with a 10 GiB storage ceiling per task. That
ceiling is not a minimum disk requirement; measure actual image/build footprint
and working headroom before building. No image pruning or benchmark runs were
performed for this preparation.

## Deferred work register

All proposals stay out of the default runtime. Existing opt-in code is marked
implemented/off; other proposals are not built. Revisit only at the stated gate.

| Idea and status | Why deferred; evidence and transfer limit | Prerequisite and revisit criterion |
|---|---|---|
| Automatic harness-search optimizer — **not built** | Search reports gains on a disjoint split, but harness choice studies find model choice dominates; harness changes more reliably move cost than accuracy. No ROF selection result exists. [2611.02079](https://arxiv.org/abs/2611.02079), [2611.12122](https://arxiv.org/abs/2611.12122); transfer to this model and TB4.1 tasks is unknown. | First complete matched external comparison and enough untouched tasks to evaluate selection. Revisit if a preregistered optimizer beats a fixed profile on held-out solve rate while staying within 1.3x tokens/solved task. |
| Adaptive budget, restart, and stall predictors — **not built** (existing `--stall-threshold` only emits off-by-default repeat-failure notices) | Stall prediction adds statistically equivalent to zero over a difficulty prior; 0/103 live restart comparisons passed certification. SPLIT's allocation result spans 7 benchmarks and 6 verifier kinds, not evidence for an unvalidated ROF policy. [2611.08285](https://arxiv.org/abs/2611.08285), [2610.36544](https://arxiv.org/abs/2610.36544), [2610.23255](https://arxiv.org/abs/2610.23255). | Calibrated trajectories with public checks and a matched no-predictor baseline. Revisit only on held-out solve gain and token guardrail pass. |
| More memory, staleness eviction, procedure learning — **procedure loading and offline distillation exist, off/manual; adaptive memory and eviction not built** | Pilot retention: WAL 1 clean pass after two zero controls; Session arms all scored 0; React transfer was interrupted. One-task results do not establish general transfer. Papers report mechanical extraction retaining more recurring detail (79.1% vs ≤16.0% for LLM summaries), prefix edits forcing cache re-prefill, and conflict-driven context rot; study settings differ from ROF. [2610.31710](https://arxiv.org/abs/2610.31710), [2610.28577](https://arxiv.org/abs/2610.28577), [2611.04169](https://arxiv.org/abs/2611.04169). Procedure proposals remain unvalidated. | Complete paired replications on fresh development tasks, then cross-task holdouts. Revisit each knob separately; promote only with repeatable capability gain and no >1.3x token ratio. Procedure files must be development-derived, hashed, and held-out tested. |
| Automatic LLM compaction — **implemented as opt-in `--compaction FRAC`, default off and unvalidated** | In one recurring-detail study, LLM summaries retained ≤16.0% versus 79.1% for mechanical extraction; this does not establish coding-agent impact. [2610.31710](https://arxiv.org/abs/2610.31710). No ROF capability comparison exists. | Compare against the unchanged control with trace-level fidelity checks, then untouched holdouts. Revisit promotion only after matched held-out solve gain within the token guardrail. |
| ROF subagent concurrency / MAS — **not built** | Dynamic concurrency is horizon-dependent. Full-text verification found +14.3pp for Claude Code on LoopsBench and −24.0pp on SWE-bench Verified, with token use 1.41–3.31x across the evaluated systems. Matched on/off runs covered 354 tasks / 2,124 executions; these settings differ from ROF. Transfer to ROF is uncertain. [2610.10263](https://arxiv.org/abs/2610.10263). | A defined task decomposition and token-matched independent-resampling control. Revisit only if held-out task success improves under the 1.3x guardrail. |
| Review, reflection, and alternative candidates — **review and candidate helper implemented off/manual; a general reflection policy is not built** (`--review on`, `tools/rof_candidates.py`) | Extra inference and fixed-public-check selection are not measured capability gains. All 18 self-inspection comparisons were negative only for 1.5B/3B/7B models on math; retry-with-feedback beats resampling in simulated robot tasks, but its feedback-content interpretation was overstated. Neither transfers directly to frontier coding agents. [2607.28576](https://arxiv.org/abs/2607.28576), [2611.06863](https://arxiv.org/abs/2611.06863); see `lit-2026-11-ERRATA.md` E3. Candidate helper is explicit, not runtime branching; its check is not a tamper-resistant grader. | Public-check and provider readiness, then a token-matched independent-resampling control and untouched holdouts. Promote only on net held-out success gain within guardrail; hidden tests never select candidates. |
| Schema / KV-server optimization — **not built** | Schema constraints suppressed tool calls on multiple open-weight models; hosted-endpoint transfer is unverified. The report describes schema-token savings from deterministic compilation on local models, but no present ROF bottleneck is measured; current requests do not enable schema-constrained response formatting. Cache rolling-drop beats threshold compaction in a different setting and is unavailable through the hosted API. [2606.25605](https://arxiv.org/abs/2606.25605), [2605.04107](https://arxiv.org/abs/2605.04107), [2610.28577](https://arxiv.org/abs/2610.28577). | Profile a measured failing workload first. Revisit only if a public benchmark task demonstrates the bottleneck and an isolated change improves task success or resource feasibility. |

External source pointers above identify subject-matter evidence, not proof of
transfer to this harness. The reviewed reports are `lit-2026-11-SYNTHESIS.md`,
`lit-2026-11-ERRATA.md`, `lit-2026-11-harness-architecture.md`,
`lit-2026-11-context-engineering.md`, `lit-2026-11-tools-inference.md`, and
`lit-2026-11-eval-verifiers.md`. Benchmark claims require controlled ROF
results.
