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
- **The reasoning-effort dial has now been measured and is not a
  capability lever** (`dphn-effort`, 18 rows, 2026-10-19): zero named-test
  flips, and the manipulation itself only moved median reasoning tokens
  1.23x (per-pair 0.42x-2.4x), so the endpoint's default reasoner already
  matches the task on real tool-loop turns. Every **passive** dial — caps,
  step envelope, tool allowlist, path resolution, goal text, reasoning
  effort — is therefore measured out at this model tier. Passive is the
  operative word: a **state-triggered** gate (a deterministic rule that
  inspects trajectory state and blocks an action) is a different class,
  only ever tested once here (`VerifyHold`, 2026-10-06, no-shipped on two
  cells that could not show the effect) — see the literature sweep below.
- **Literature sweep 2026-10-19** (`research/lit-sweep-2026-10-19.md`, four
  researcher lanes, primary sources): three results change the picture and
  two levers are now foreclosed by measurement. (a) Deterministic
  state-triggered gating is real at our model class — LivePlan
  (arXiv:2608.06701) monitor-only, no advisor LLM, +1.7 to +5.8 pp on
  213-234 instances; the full method needs an advisor *stronger* than the
  executor, which we do not have. (b) Reward-free harness search is real at
  our exact family — SelfSearch (arXiv:2609.37968) 65.2 -> 73.0 on TB 2.1
  with a deepseek-v4-flash executor at matched limits and 12% lower cost,
  search $4.03 — but it is a re-architecture (editable substrate + 20
  episodes), not a knob. (c) Agent-written oracles *hurt* at a fixed model
  (ExecCritic, 61.2 -> 57.3), and prompt-text levers on TB 2.1 are null at
  13x our n (-1.50 to +2.25 pp, arXiv:2610.22289). Google's Harness²
  (arXiv:2610.14224) was reviewed at primary-source level and the deferred
  search gate **stays closed**: no attempts-matched ablation is reported,
  its floor is 4 executor rollouts + 3 importer sessions per task against a
  1.3x tokens-per-solved rule, its wins are per-task workspace-fact
  injection, and its release has no base fallback.
- **Measurement rules from the sweep (binding on future claims).** At 21 tasks
  the paired MDE is 40.8 / 32.2 / 28.8 pp at 1 / 2 / 3 reps and never drops
  below 20.4 pp, so no arm is justified by a smaller expected solve-rate
  delta; task count is the power lever, reps buy reliability only. The honest
  headline is **pass^1 with a paired, task-clustered CI** (pass@k is an order
  statistic, and using unit-test counts as `n` inflates scores by 0.85-0.97
  absolute). **Named-test counts are never capability percentages** — one
  pilot reads 0.80 macro test-pass against 0.20 task resolution, and 19.7 % of
  previously passing patches are semantically wrong. Verifier/environment
  variance (94.3 % -> 65.5 % from a reward defect; -5.3 to -9.2 pp from tool
  descriptions) is larger than the effects this repo chases, so per-row
  pre-flight classification plus a known-answer control belongs in provenance,
  not in prose.

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

0. **State-triggered gate arm (`dphn-gate`) — PRE-REGISTERED 2026-10-19,
   awaiting a go; not launched.** The one class the six measured arms never
   tested (they are all passive). Deterministic rule, no LLM in the loop:
   when the next action is a Done/declare and no *successful re-execution of
   the agent's own deliverable from a foreign working directory* has happened
   since the last write, block the declare and inject one fixed,
   task-agnostic line naming that requirement (max 2 fires/run, anti-repeat
   latch, cap-tail suppression; reuses the `VerifyHold` shape from
   `nudge-verify`). The gate never points at `/tests`: it asks for the
   agent's own artifact only, so no verifier visibility is granted. Cells
   mcmc-sampling-stan (5/6; the failing test is a deliverable requirement),
   video-processing (2/5; `test_jump_analyzer_imports` has failed in all
   eight prior rows), query-optimize (1/6; correctness control where the
   gate should do nothing) + log-summary-date-ranges (2/2) as regression;
   control vs gate, 3 reps, same-window pairs, one binary, 24 rows. PASS: a
   named test that no control row of *any* prior arm passed flips in >=1
   treated rep **and** in a second rep of the same cell (a single flip is
   inside the per-test noise floor: at a 0.86 % flake base rate a 6-test cell
   sees one spurious flip ~5 % of the time, and 3 reps cannot certify a test
   non-flaky), with a visible change in the deliverable that test names and
   the regression cell unchanged. MECHANISM: the gate fires in every
   treated row that declares without re-execution. GUARDRAIL: tokens per
   test-passed <=1.3x and solves not lower. KILL: zero flips over 3 cells x
   3 reps => the class joins the passive dials as measured out. Prior is
   mixed and cheap to resolve: LivePlan's monitor-only subset is +1.7..+5.8
   pp (n~220 instances), this repo's own `VerifyHold` gate no-shipped on two
   cells that could not show the effect (one container-only task where local
   verification was impossible, one voided grading instrument), and the
   checklist converted this exact defect class as *text* once (5/6 -> 6/6).
   The question is narrow: does enforcing the ritual beat suggesting it?
1. **Model swap — RETIRED 2026-10-19 (owner directive + vendor card).**
   `deepseek-v4.1-flash` is the frontier-tier model here, and its **1017
   snapshot (2026-10-17)** is a large step above its predecessor on agentic
   benchmarks (vendor card: Terminal-Bench 4.0 **42.3** vs 31.2 for the prior
   snapshot, 37.9 GLM-5.3, 39.9 GPT-5.6 Sol; DeepSWE v1.1 77.0 vs 74.2;
   MIT weights, 1M context). A cheaper-tier substitute is therefore not the
   lever, and no further model reps are bought. The sweep's shortlist stays in
   `research/models-2026h2-shortlist.md` as a record, and one correction from
   it survives: every published number for those candidates is a **vendor**
   claim, so vendor rows rank candidates and never predict our solve rate.
2. **Zero-cost instrumentation: a pairwise, answer-key-free judge.** From
   Harness2's selector and RRSI (`research/harness-steals-2026h2.md` steal
   1 + 2): score two anonymised deliverables of the same task against each
   other instead of against a hidden verifier, so arms stop returning "tie at
   zero solves". Validate it **for free** against the banked (solved, failed)
   workdirs before trusting it, and add RRSI's noise-adjusted floor + cost
   rule as pure functions in `eval`. Instrument only — it cannot make a
   capability claim on its own.

3. **Wire fidelity against the vendor's own encoder.** DeepSeek released
   `github.com/deepseek-ai/deepseek-recipe` (MIT, Rust) — a protocol-aware
   toolkit that converts Chat Completions/Messages/Responses requests into
   DeepSeek V4/V4.1 prompts and parses output back. `provider-openai`
   hand-rolls exactly that surface and infers the wire from 400s. Two things
   it already settled: (a) reasoning effort on V4.1 is a **prompt line**
   (`Reasoning Effort: {1-100}`, low=50 / absent=75 / max=100), not a request
   field — see Next 4; (b) `reasoning_content` presence in thinking mode is
   vendor behavior with an empty string allowed, which corroborates our
   shape-based belt. The item itself is a **diff, not a port**: read the
   vendor's conversion for the shapes we construct, and pin any disagreement
   with a test. No dependency, no rewrite.
4. **Native effort channel (`dphn-effort2`) — PRE-REGISTERED and RUNNING
   2026-10-19.** The `dphn-effort` kill measured the JSON field, not the
   model's channel: one prompt through the same gateway gives 576 reasoning
   tokens plain, 1,038 with `reasoning_effort:"max"`, and **2,327** with the
   vendor's own prompt marker; one in-loop probe row gave **19,495** vs
   4,816-7,055 control (2.8-4.0x). So the capability half of that verdict is
   inconclusive for effort and stands only for the field. This arm sends the
   vendor line (`Reasoning Effort: 100 (range 1-100, ...)`) prepended to a
   task copy's instruction, control vs marker x 3 reps over video (2/5),
   mcmc (5/6) and log-summary (2/2 regression) = 18 rows, same binary.
   MECHANISM: median reasoning tokens >=1.5x. PASS: a named test no prior
   control row passed flips in >=2 of 3 treated reps. GUARDRAIL: tokens per
   test-passed <=1.3x. KILL: zero flips => the effort axis is dead **with a
   properly delivered treatment**, which is a far stronger statement than the
   field arm's kill.
5. **Reasoning-effort arm (`dphn-effort`) — MEASURED 2026-10-19, KILL RULE
   TRIGGERED: the effort dial is not a capability lever.** 18 in-container
   rows on one binary, the arms differing by one flag (control vs
   `--reasoning-effort max`). No named verifier test flipped in any treatment
   row that a control row had not already passed: query-optimize 1/6,1/6 ->
   0/6,0/6, video the same three fails in all four rows, cmhc 0/6 everywhere,
   and the arm's only solve was a *control* row. The manipulation itself
   under-delivered — median reasoning tokens +1.23x against a pre-registered
   1.5x, per-pair ratios 0.42x-2.4x — so the honest claim is "this knob does
   not raise effort on tool-loop turns", not "effort does not help"; the raw
   probe's 497 -> 2976 tokens came from a short ungrounded prompt, and on real
   turns the endpoint's default already sits near the task's natural reasoning
   length. Guardrail also failed (1.54x tokens per test-passed). No further
   effort or wording variants. With this, every harness-side dial that does
   not need a different model has been measured: same named failures survive
   more budget, more steps, a full toolchain, `/app`-correct paths, a stricter
   goal and more reasoning.

6. **Near-miss rep top-ups** (if they earn the spend): the
   partial-credit cells (1/3, 1/2, 2/4, 2/3 test shapes) are the
   cheapest capability information per cell.
7. **S-1 compaction experiment** — BLOCKED, premise refuted
   2026-10-16 (see Current state): the landed trigger keys on per-request
   prompt size while the recipe's budget is cumulative billed tokens, so
   it cannot fire at any plausible `frac`. Re-design first (trigger on
   spent-vs-cap, keep-budget scaled to real prompts), then re-register a
   pass rule; do not run the original arm.
8. **File-tool `/app` parity — FIXED 2026-10-18 (correctness, no capability
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
9. **In-container exec policy — MEASURED 2026-10-18: mechanism confirmed,
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
10. **Envelope arm — MEASURED 2026-10-16/18, KILL RULE TRIGGERED.** Three
   matched arms (control = frozen recipe; shape = miss-plus-out at the
   same cap; joint = miss-plus-out plus a doubled step/action envelope)
   over two headroom cells and one already-solved regression cell. The
   envelope moved walls but not capability: on one cell it converted a
   token-death into a step-death and then into 87 of 120 steps (1.85x
   steps, 3.0x gross input) while the same single failing test survived in
   every arm — a semantic requirement, not budget. Tests per treated cell
   were flat, tokens per test passed were 1.85x/2.35x against a 1.3x rule.
   No further cap/envelope arms; near-miss top-ups stay parked.
11. **Budget-shape arm run** — MEASURED 2026-10-14 (first arm,
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
12. **Rivals grid comparisons** — DONE 2026-10-14 (free): the legal
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
  **Reviewed and closed 2026-10-19** (`research/harness2-teardown.md`,
  `research/harness-steals-2026h2.md`): Harness2 (arXiv:2610.14224) is the
  verifier-free member of this cluster and does **not** open either
  condition. It reports no attempts-matched ablation (best-of-k is defined
  and never numbered), its cost floor is 4 executor rollouts + 3 importer
  sessions per task, its wins are per-task workspace-fact injection on
  34-100 tasks x 3 streams with deltas inside 1 SD, and it needs a mounted
  8-file harness surface this binary does not have. SelfSearch
  (arXiv:2609.37968) is the same shape without a reward signal and with a
  10-generation, 2-lineage editable-substrate requirement. Condition (b)
  is weakened, not satisfied: a reward-free search needs no suite to
  *drive* it, but a suite is still needed to *validate* a gain.
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
