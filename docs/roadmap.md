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

1. **Rivals comparison recompute** (free): paired bootstrap + Wilson
   over the stored matrix data. Define the comparison grid first; never
   mix estimands (harbor-graded rows vs in-container reward-file rows)
   or arms (different binaries, models, or recipes) in one table.
2. **Budget-shape arm** (the structural bet): charge cache-miss +
   output tokens, not gross input, against the token cap. Measured
   motivation (2026-10-08 forensics): 90–98% of the 300k cap is context
   re-reads (out:in 0.02–0.12); runs die at 21–53 steps having done
   ~5–25k tokens of novel work. This is a new estimand: record it in
   `docs/decisions.md` before the first run.
3. **Near-miss rep top-ups** (if they earn the spend): the
   partial-credit cells (1/3, 1/2, 2/4, 2/3 test shapes) are the
   cheapest capability information per cell.
4. **S-1 compaction experiment**: the `--compaction` knob is landed
   OFF by default and unvalidated; the first paid run validates
   `frac`/`keep_tokens` or revises them.

## Deferred / gated (build only when the gate opens)

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
