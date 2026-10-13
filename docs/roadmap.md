# Roadmap

Ordered and spend-gated. Design decisions anchor to `docs/decisions.md`;
measured numbers carry a date and live with the run artifacts.

## Current state (2026-10-12)

- The live frontier is in-container eval: `rof eval --agent
  --in-container` runs the agent inside the task environment container
  (installed packages and live services persist into grading) and grades
  from the task-written reward file (estimand fix landed with the
  2026-10-12 commit sequence; exit-code-only grading was retired after it
  read always-zero verifier exits as passes).
- Measured on the frozen TB Slice A (21 tasks): 26 banked in-container
  cells (glm-5.3-flash, frozen recipe 300k tokens / 60 steps / 120
  actions), reward-verified **6 Resolved / 17 Unresolved / 1 infra**
  (2026-10-12). The env-mutation family (installed-by-the-agent imports,
  a live server) is winnable in-container and structurally impossible
  under the file-copy grading bridge.
- The file-artifact widening read **4/18 solved**
  (break-filter-js-from-html, hf-model-inference,
  log-summary-date-ranges, sam-cell-seg; 2026-10-12).

## Next (ordered, spend-gated)

1. **Provenance labels on every report** (small): a provenance block on
   each `RunReport` — binary git sha, budget recipe, estimand label,
   apply mode — so every arm is self-describing by its own file instead
   of by driver-script convention. This is the 2026 comparability
   literature's core demand — report the full harness specification
   alongside any comparison, and treat model+harness as the tested
   solver (arXiv:2607.22585, arXiv:2608.26218) — and it feeds every
   later move. DECISIONS entry first, then struct + wiring + tests.
2. **Rivals comparison grid** (free): cross-harness comparison over the
   stored matrix data — same models, same frozen tasks, only the
   harness varies — with per-cell rollouts published.
   **Cost and tokens-per-solved are the primary axes; pass rate is
   secondary.** The measured noise floor (arXiv:2610.04433: 447 tasks
   resolve ~5 points; 45 tasks catch a 13-point gap only half the
   time) means pass-rate harness deltas are not resolvable at this
   slice size (21 tasks), while harness cost effects are — up to 3x
   per task and 40x tokens-per-solved at fixed model and tasks
   (arXiv:2610.04433, arXiv:2607.22585). Never mix estimands
   (harbor-graded rows vs in-container reward-file rows) or arms
   (different binaries, models, or recipes) in one table.
3. **Budget-shape arm** (the structural bet): charge cache-miss +
   output tokens, not gross input, against the token cap. Measured
   motivation (2026-10-08 forensics): 90–98% of the 300k cap is context
   re-reads (out:in 0.02–0.12); runs die at 21–53 steps having done
   ~5–25k tokens of novel work. This is a new estimand: record it in
   `docs/decisions.md` before the first run.
4. **Near-miss rep top-ups** (if they earn the spend): the
   partial-credit cells (1/3, 1/2, 2/4, 2/3 test shapes) are the
   cheapest capability information per cell.
5. **S-1 compaction experiment**: the `--compaction` knob is landed
   OFF by default and unvalidated; the first paid run validates
   `frac`/`keep_tokens` or revises them.

## Deferred / gated (build only when the gate opens)

Reconsideration gate = the concrete conditions under which building it
starts making sense. Both gates below were reviewed against the 2026
literature on 2026-10-13 and deliberately stay closed.

- **Harness-search outer loop** (an agentic proposer rewrites harness
  code, reads every prior candidate's traces and scores from disk, and
  keeps the best by frozen-model score; the arXiv:2603.28052 lineage
  plus the 2026 cluster — arXiv:2604.25850, arXiv:2608.01918,
  arXiv:2608.23041, arXiv:2609.38349): deferred, reconsider when BOTH
  (a) a cross-harness grid shows a reproducible capability or cost gap
  this harness could plausibly close by code search, and (b) the suite
  is large enough to measure the loop's own improvement — by the
  arXiv:2610.04433 noise-floor calibration, hundreds of tasks for
  pass-rate deltas, or a cost-metric loop on the frozen slice. One
  published run costs ~60 harness candidates over hours with up to
  10M tokens of diagnostics per evaluation; at 21 tasks its gains
  would be indistinguishable from rerun noise.
- **Orchestration / policy meta-layer above harnesses** (a layer that
  supervises running agents — cross-harness delegation, live policies,
  session control): deferred, reconsider when this harness runs
  unattended on machines its author does not own or is driven by more
  than one operator. The measured core is already shipped: the command
  allowlist and policy gate cut attack effects at small utility cost
  (arXiv:2610.07639), and multi-agent orchestration stays out of
  scope by the no-planner/no-MAS rule.
- **Bet C (candidate racing)**: unbuilt until a workload shows
  proof-gate interventions (zero in all measured runs to date).
- **Unpriced-model spend warning**: deferred until a non-test unpriced
  model runs for real.
- **Parked nits** (only if they earn it): search depth cap, `/app`
  word-boundary in goal mapping, assemble budget accounting,
  finalize_dump exit code.

## Known ceilings (documented upgrade paths)

- In-container containers hardcode `--cpus=2 / --memory=4096m`
  (upgrade: parse `[environment]` from task.toml per task).
- Timeout/cancel kill the docker-exec client; the in-container process
  may outlive it (upgrade: docker kill or exec-session tracking).
- Root-owned in-container artifacts survive host-side `git clean`
  during batch rollback (fail-soft; no verdicts affected to date).
