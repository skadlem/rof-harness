# Roadmap

Ordered and spend-gated plan of open work. Decision records and measurements live in
[decisions](decisions.md); current state, deferred gates and campaign handoff live in
[research-next-steps](research-next-steps.md).

## Open work (ordered)

1. **TB4.1 CPU option screen (completed 2026-11-25, 0/48 solved at 300k):**
   [2026-11-25-tb41-option-screen](experiments/2026-11-25-tb41-option-screen.md). All 48 runs
   completed with 0 infra errors ($3.68 peak / $1.81 live USD) across control,
   `exec_output: "head-tail"`, `rollback_scope: "errors"`, and `max_tokens: 32768` at 300k gross
   tokens; 0/12 solved in every arm (47/48 token halts, 1/48 32k truncation halt). All three
   mechanisms verified in WALs; defaults unchanged. Next benchmark step: select a non-zero-floor
   TB4.1 CPU slice (or 1.2M pilot budget) before running further option screens.
2. **Phased capability sequence (2026-11-11):**
   [capability-phases](superpowers/plans/2026-11-11-capability-phases.md). Phases 0–4 landed
   locally and opt-in ([phase-0-2](phase-0-2-implementation.md),
   [phase-3-5](phase-3-5-implementation.md)); Phase 5 paused on an unknown-usage reset.
   Remaining open sequence: development screening → held-out confirmation on untouched TB4.1.0
   CPU tasks, admitting promotions only on held-out solve gain within ≤1.3× tokens per solved
   task and the USD ceiling.
3. **Opt-in mechanisms awaiting benchmark validation or observed-need gates:**
   - `--compaction FRAC` (with `--context-capacity` and `--output-reserve`),
     `--history-read on` and `--checkpoint-sources on`: implemented off; gated on observed
     prompt pressure and irrecoverable-evidence loss in traces (`architecture.md`,
     `research-next-steps.md`).
   - `--repo-lookup on` / `--repo-evidence on`, `--completion-check`, `--archive-output on`,
     `--shell on`, `--budget-line requests`: implemented off; single-knob development arms when
     prioritized (`harness-experiments.md`, `research-next-steps.md`).
   - `--agents FILE` (stage 1 read-only `spawn`/`wait`): implemented off, unwired from
     Harbor; stages 2–3 rejected at load (`decisions.md`).
   - **Parked nits** (only if earned): search depth cap, `/app` word-boundary in goal mapping,
     assemble budget accounting, `finalize_dump` exit code.
   - **Superseded Slice A items (2026-11-06):** broader portfolio baseline (0b: `dna-assembly`,
     `raman-fitting`, `model-extraction-relu-logits`, `break-filter-js-from-html`,
     `make-mips-interpreter`, ~$0.60) and near-miss rep top-ups (item 6) are parked/superseded
     under the TB2.1 → TB4.1.0 benchmark transition ([deepseek-official-pilot](deepseek-official-pilot.md)).

## Historical state and closed items (collapsed)

- `2026-11-11` Output fidelity handoff, $2.82 ceiling, $0.661849662 subledger and Phase 5 stop:
  see [phase-3-5-implementation](phase-3-5-implementation.md) and
  [research-next-steps](research-next-steps.md#objective-and-current-state).
- `2026-11-10` Official DeepSeek campaign, TB4.1.0 selection and direction reset: see
  [deepseek-official-pilot](deepseek-official-pilot.md),
  [harness-capability-plan](harness-capability-plan.md) and
  [harness-leaderboard-review](harness-leaderboard-review.md).
- `2026-11-06` Corrected-pipeline capability map, `dphn-steer2`, `dphn-seedfix-base`/`base2`,
  `--exec-policy any` (`dphn-env1`), workspace/tests/timeout/git fixes, and `mcmc` exclusion: see
  [decisions](decisions.md).
- `2026-11-05` Steering pilot ($10 cumulative cap, HTTP 403/524 stop, voided 2026-11-06): see
  [steering-experiment](steering-experiment.md) and [decisions](decisions.md).
- `2026-11-03` Reliability fixes (`32f6b36`) and parser replay: see [fix-plan](fix-plan.md) and
  [decisions](decisions.md).
- `2026-10-19` Reasoning effort (`dphn-effort`, `dphn-effort2` killed), model-swap retirement,
  literature sweep and measurement rules: see [decisions](decisions.md).
- `2026-10-18` Five in-container arms (envelope kill, exec policy, `/app` parity, completion
  checklist): see [decisions](decisions.md),
  [allowlist-and-named-deliverable](experiments/2026-11-11-allowlist-and-named-deliverable.md) and
  [next-after-slice-a](superpowers/plans/2026-11-11-next-after-slice-a.md).
- `2026-10-16` Second vLLM gateway endpoint and compaction premise refutation: see
  [decisions](decisions.md).
- `2026-10-12/14` In-container eval estimand, provenance block, `--budget-shape` arm and rivals
  grid: see [decisions](decisions.md) and [current-benchmark-pilot](current-benchmark-pilot.md).
- Deferred work register (`harness-search`, `adaptive predictors`, `memory/compaction`, `MAS`,
  `review/candidates`, `schema/KV`, `Bet C`): see
  [research-next-steps](research-next-steps.md#deferred-work-register).
- Known container/rollback ceilings (`--cpus=2 / --memory=4096m`, docker-exec descendants,
  root-owned artifact rollback): see [decisions](decisions.md) and
  [architecture](architecture.md#guarantee-limits-and-runner-scope).
