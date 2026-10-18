# Roadmap

Ordered and spend-gated. Design decisions anchor to `docs/decisions.md`;
measured numbers carry a date and live with the run artifacts.

## Current state (2026-10-19)

- **Reasoning effort is now a request knob** (`--reasoning-effort` on `run`
  and `eval`), because a $0 probe showed the harness ran every arm at the
  endpoint's default while the gateway honors `none|low|high|xhigh|max`
  (0/722/1060/1149/2976 reasoning tokens on one prompt; 236 -> 568 inside
  `rof run` at equal caps; api.deepseek.com ignores the field). The level
  rides `EndpointProfile`, is off by default (byte-identical body) and is
  recorded in the report provenance, so effort rows never pool with
  frozen-recipe rows. Reasoning length is bounded by `max_tokens`, and the
  overflow ladder raises that ceiling on a truncated attempt.
- **The unpriced-spend warning gate is closed.** A run whose model has no
  price-book entry now says on stderr, once, that the `$` cap is inert and
  that the token/step caps are the guards.
- A **stronger-model ceiling read is unavailable** (no stronger model on
  hand), so the capability question at this model tier is asked with the
  one dial that does not need a new model: same model, higher effort
  (`dphn-effort`, pre-registered before spend).

## Current state (2026-10-18)

Five arms measured on the in-container leg in one session (68 graded cells,
deepseek-v4.1-flash via a vLLM gateway, reward-file estimand, frozen recipe):
the envelope axis (token-cap shape, doubled step envelope) is **dead** — it
moves walls and not capability; the command allowlist was a real friction and a
fix (`--exec-policy any`) removes it with no capability effect; the file tools
refused the workspace spelled `/app/...` as the task statements spell it, which
is fixed (correctness; the solved regression cell now finishes in ~half the
steps and tokens); and one **capability-positive** lever exists: a generic
completion checklist in the goal converts deliverable-verification defects
(5/6 → 6/6) but changes nothing on cells that fail on accuracy. Each arm's rows
self-describe through the provenance block; no arm's rows pool with another's.

## Current state (2026-10-16)

- A second experiment endpoint is wired: `deepseek-ai/DeepSeek-V4.1-Flash`
  behind a vLLM gateway (`lighthouse.dphn.ai/run/text-v/v1`). Verified by
  live probes: tool calls work, prefix-cache reads are reported, thinking
  arrives under `reasoning` (the adapter now reads that as a fallback and
  still prefers `reasoning_content` when both keys are present), and
  DeepSeek's `thinking:{type:disabled}` knob is accepted but ignored. The
  endpoint is **unpriced** for this harness (it reports a cost on only
  some routes), so dollars are `null` there by design and the token/step
  caps are the operative guards.
- The **compaction premise was refuted by measurement before spend**: the
  landed `--compaction FRAC` trigger compares a *per-request* prompt
  estimate against `frac x token budget`, but the frozen recipe's budget
  is *cumulative billed tokens*. Measured per-request prompts in the
  banked arms top out at 18.0k tokens (median 2-3k), so the registered
  0.7 x 300k = 210k trigger cannot fire, and the 20k verbatim-keep budget
  exceeds a whole prompt. The knob is a context-window guard mis-keyed to
  a spend cap; the registered validation arm is inert and returns to
  design.

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

0. **Reasoning-effort arm (`dphn-effort`) - pre-registered 2026-10-19,
   running.** Same model/endpoint/binary/recipe/estimand, two arms that
   differ by one flag: control (no level sent) vs `--reasoning-effort max`.
   Cells = the three headroom cells of the checklist arms (mcmc 5/6,
   video-processing 2/5, query-optimize 0-1/6) plus custom-memory-heap-crash
   (0/6), 2 reps each, with log-summary-date-ranges (2/2 everywhere) as the
   regression cell at 1 rep. PASS: a treatment row passes a NAMED test that
   never passed in any control row of the arm (or solves a cell the control
   never solved), with the regression cell unchanged. MECHANISM: median
   reasoning tokens per row at least 1.5x control. GUARDRAIL: gross tokens
   per test-passed within 1.3x of control - the cap counts reasoning at full
   weight, so this can fail even when capability moves. KILL: no named test
   flips in any treatment row => the effort axis is dead at this tier and no
   further effort or wording variants run. Rows carry
   `provenance.reasoning_effort`; never pooled with frozen-recipe rows.

0. **Completion-checklist goal lever — first capability-positive result,
   and now scoped (2026-10-18, 22 rows over two arms).** One added
   completion-checklist block (list every explicit requirement; verify each
   in place; **re-execute each deliverable from a working directory other
   than the one it was developed in**; only then declare done) converted a
   cell the control had never solved into a 6/6 solve (5/6 -> 6/6, identical
   token spend, path-robustness code 11-12x in checklist deliverables vs
   0-1x in control), and changed **nothing** on three cells that fail on
   accuracy or correctness: identical failing test sets, flat tokens,
   regression cell untouched. So the lever is real but **rerun-context
   specific**; the kill rule fired and no further checklist arms run. The
   remaining failures on this slice are semantic and capability-bound at
   this model tier, which makes a stronger-model ceiling read the next
   honest experiment rather than another prompt variant.
1. **Near-miss rep top-ups** (if they earn the spend): the
   partial-credit cells (1/3, 1/2, 2/4, 2/3 test shapes) are the
   cheapest capability information per cell.
2. **S-1 compaction experiment** — BLOCKED, premise refuted
   2026-10-16 (see Current state): the landed trigger keys on per-request
   prompt size while the recipe's budget is cumulative billed tokens, so
   it cannot fire at any plausible `frac`. Re-design first (trigger on
   spent-vs-cap, keep-budget scaled to real prompts), then re-register a
   pass rule; do not run the original arm.
3a. **File-tool `/app` parity — FIXED 2026-10-18 (correctness, no capability
   claim).** Joining denials to their tool arguments showed the largest
   file-tool denial class was the workspace spelled the way task statements
   spell it (`/app/<file>`): the host-side tools root elsewhere, so agents
   were refused the files they were told to edit and fell back to `exec cat`.
   The resolver now strips a leading `/app/` (strictly additive, traversal /
   secrets / git refusals unchanged, pinned by test). A two-binary A/B (12
   rows) shows the class gone, no test moved on the failing cells, and the
   already-solved regression cell reached the same result in about half the
   steps and tokens. What is left is container-only paths (`/tests`), which
   host-side tools cannot serve — serving them would let the agent read the
   verifier's tests, a fairness decision deliberately not taken.
3. **In-container exec policy — MEASURED 2026-10-18: mechanism confirmed,
   capability unconverted.** `--exec-policy allowlist|any` landed (default
   allowlist, recorded as a required provenance field). The measured
   friction is real — 83% of banked in-container rows hit a tool denial and
   the denied set included the tasks' own toolchains — and the treatment
   removed it completely (allowlist denials 3-9 per row -> 0). But on the
   three cells chosen because their zeros looked like denial artefacts, the
   freed toolchain converted nothing: the same named tests fail in every
   arm, at the same steps and tokens, and the solved regression cell stays
   solved. So the zeros on this slice are capability/semantic, not policy:
   the published in-container solve numbers do not need a re-label, the knob
   stays flagged mechanism-validated / capability-unconverted, and no
   further policy arms are planned.
4. **Envelope arm — MEASURED 2026-10-16/18, KILL RULE TRIGGERED.** Three
   matched arms (control = frozen recipe; shape = miss-plus-out at the
   same cap; joint = miss-plus-out plus a doubled step/action envelope)
   over two headroom cells and one already-solved regression cell. The
   envelope moved walls but not capability: on one cell it converted a
   token-death into a step-death and then into 87 of 120 steps (1.85x
   steps, 3.0x gross input) while the same single failing test survived in
   every arm — a semantic requirement, not budget. Tests per treated cell
   were flat, tokens per test passed were 1.85x/2.35x against a 1.3x rule.
   No further cap/envelope arms; near-miss top-ups stay parked.
5. **Budget-shape arm run** — MEASURED 2026-10-14 (first arm,
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
6. **Rivals grid comparisons** — DONE 2026-10-14 (free): the legal
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
- ~~**Unpriced-model spend warning**: gate triggered 2026-10-16, CLOSED
  2026-10-19.~~ A run whose model has no price-book entry prints one stderr
  warning naming the inert `$` cap and the operative token/step caps
  (`cli::budget_for`; `provider_openai::prices_model` is the single
  predicate), so an unpriced run never looks guarded. The other half of the
  gate — a per-endpoint price book — is still unbuilt by choice: no price is
  invented.
- **Parked nits** (only if they earn it): search depth cap, `/app`
  word-boundary in goal mapping, assemble budget accounting,
  finalize_dump exit code.

## Known ceilings (documented upgrade paths)

- In-container containers hardcode `--cpus=2 / --memory=4096m`
  (upgrade: parse `[environment]` from task.toml per task).
- Timeout/cancel kill the docker-exec client; the in-container process
  may outlive it (upgrade: docker kill or exec-session tracking).
- Root-owned in-container artifacts survive host-side `git clean`
  during batch rollback. Measured across 70 banked in-container rows:
  snapshot-class halts in 5, of which 4 are this class — fail-soft in
  3 (the row still grades) but a **hard halt** in 2, including one that
  voided a run at step 3 (2026-10-16) and one at the end-of-run patch
  digest (`halt=snapshot` on install-windows-3.11, 2026-10-14).
  Upgrade: make the workdir agent-owned inside the container (chown
  before rollback/digest) or digest via a root-readable copy.
