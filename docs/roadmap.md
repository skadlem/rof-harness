# Roadmap

Ordered and spend-gated plan of open work. Decision records and measurements live in
[decisions](decisions.md); current state, deferred gates and campaign handoff live in
[research-next-steps](research-next-steps.md).

## Open work (ordered)

1. **TB4.1 CPU option screen (0/48 at 300k), uncapped calibration (0/2), & off-peak streaming pass (0/4 binary, 66/85 subtests, 2026-11-25):**
   [2026-11-25-tb41-option-screen](experiments/2026-11-25-tb41-option-screen.md),
   [2026-11-25-tb41-uncapped-calib](experiments/2026-11-25-tb41-uncapped-calib.md), and
   [2026-11-25-tb41-offpeak-streaming](experiments/2026-11-25-tb41-offpeak-streaming.md).
   Plumbing landed in `bd482ba` + `47e9811` (uncapped tokens, USD spend cap, omitted `max_tokens`,
   384K ceiling, SSE per-chunk idle timeout, `usage_unknown` propagation, Harbor `stream`).
   The 4-task off-peak streaming pass (`$0.71` live / `$1.42` peak, `.env.deepseek` balance
   `$0.87 USD`) verified 95.5%–98.0% cache hit, 0 timeouts, 0 `usage_unknown`, and 3/4 voluntary
   completions (`15/21`, `10/13`, `29/37`, `12/14` subtests). Defaults unchanged. Next decision:
   wire/test `--review on` on the 3 near-miss completed tasks or establish a non-zero solve floor
   on `freight-dispatch-shift`/`cargo-flight-dispatch` (or `deepseek-v4-pro` sanity check).
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
