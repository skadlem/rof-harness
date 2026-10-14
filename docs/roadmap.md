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
  actions), reward-verified **6 Resolved / 19 Unresolved / 1 infra**
  (2026-10-12; re-derived from the summary artifact 2026-10-13 — an
  earlier note that read 6/17/1 summed to 24 and was an arithmetic slip).
  The env-mutation family (installed-by-the-agent imports,
  a live server) is winnable in-container and structurally impossible
  under the file-copy grading bridge.
- The file-artifact widening read **4/18 solved**
  (break-filter-js-from-html, hf-model-inference,
  log-summary-date-ranges, sam-cell-seg; 2026-10-12).
- Every report self-describes (2026-10-13): the provenance block
  carries the binary git sha (embedded at build time), the agent-leg
  budget recipe including the token-cap shape, the apply mode, and the
  estimand label — `Ungraded` (plain eval), `HarborOracle` (`--agent`),
  `InContainerReward` (`--agent --in-container`). Reports written before
  the block existed still deserialize.
- The budget-shape knob landed OFF by default (2026-10-13):
  `--budget-shape gross|miss-plus-out` on `rof run` and `rof eval`.
  `miss-plus-out` charges `(input - cache_read) + output` against the
  token cap (reported usage and dollars stay gross); it is a different
  estimand from `gross`, recorded on every provenance block, and its
  first arm run is still owner-gated spend.

## Next (ordered, spend-gated)

1. **Near-miss rep top-ups** (if they earn the spend): the
   partial-credit cells (1/3, 1/2, 2/4, 2/3 test shapes) are the
   cheapest capability information per cell.
2. **S-1 compaction experiment**: the `--compaction` knob is landed
   OFF by default and unvalidated; the first paid run validates
   `frac`/`keep_tokens` or revises them.
3. **Budget-shape arm run** — MEASURED 2026-10-14 (first arm,
   4 near-miss cells, owner-authorized): the pre-registered gate
   (more solves at ≤1.3× dollars-per-solved) **failed** — 0/4
   solves on both shapes, arm cost 1.42×, tests flat-to-down. The
   mechanism itself is confirmed (the token-dead cells migrated to
   the step cap, consuming 1.6–1.7× the gross arm's input), and the
   frozen recipe now binds both caps nearly together (84.6%/99.8%
   of the miss+out cap at step death), so raising either cap alone
   just moves the wall. The knob stays OFF by default, opt-in via
   `--budget-shape`, flagged mechanism-validated /
   capability-unconverted.
4. **Rivals grid comparisons** — DONE 2026-10-14 (free): the legal
   tables are computed and published in the local analysis artifact
   (L1/L2 screening, L4/L5 single-arm cost+solve tables). Cross-harness
   cost is not computable (rival logs carry no usage fields); pass-rate
   deltas sit below the noise floor. Estimands and arms never mix.

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
  during batch rollback (fail-soft, no verdicts affected), and —
  measured 2026-10-14, budget-shape arm 1 — can also **hard-halt a
  run** when the end-of-run patch digest (`git diff --no-index`)
  cannot read them (`halt=snapshot` on install-windows-3.11; upgrade:
  digest via a root-readable copy or chmod at harvest).
