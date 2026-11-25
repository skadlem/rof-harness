# Research and implementation handoff: Slice A 5/18, all new mechanisms opt-in and unmeasured

Active execution reference: the
[phased implementation plan](superpowers/plans/2026-11-11-capability-phases.md). Phases 0–4 are
implemented and locally verified, all new mechanisms opt-in; Phase 5 ran one interrupted control
and stopped on a new unknown request, no matched pair completed, both ledgers and reservations
intact. The [phase 0–2](phase-0-2-implementation.md) and
[phase 3–5](phase-3-5-implementation.md) records separate implementation, chosen fixtures,
historical outcomes and current campaign evidence.

## State (2026-11-23)

Entry point: [`SESSION-NEXT.md`](../SESSION-NEXT.md). The recommended next iteration is a frozen
12-task OpenThoughts-TBLite slice, pending owner direction; the root handoff records the partial 4.1 CPU
oracle preflight (storage/build failures) and no paid baseline on either slice.
[Slice A plan](superpowers/plans/2026-11-11-next-after-slice-a.md).

> Now (2026-11-25): the
> [TB4.1 CPU option screen](experiments/2026-11-25-tb41-option-screen.md) completed
> (4 tasks × 4 arms × 3 reps = 48 runs, 0 infra errors, $3.68 peak / $1.81 live USD):
> **0/48 solved** across control, `exec_output: "head-tail"`, `rollback_scope: "errors"`, and
> `max_tokens: 32768` at the 300k gross token cap (47/48 token halts, 1/48 32k truncation halt).
> Follow-up Step 1 plumbing (`bd482ba`: `--budget-tokens 0`, `--budget-spend-cents`, `--max-tokens 0|omit`,
> `MAX_TOKENS_CEILING = 393_216`) + [uncapped calibration](experiments/2026-11-25-tb41-uncapped-calib.md)
> on `photonic-waveguide-routing` (0.75 h, oracle 1.0) ran 2 trials before hitting the `$1.50` live
> stop (`$1.88` live / `$1.62` peak; 0/2 solved): `append_1000` lifted cache hit **62.4% → 92.9%**
> (**99.5% steady-state**) and got within `0.13 µm` of valid geometry in 25 turns vs `control_5_2`'s
> `0.00 µm` overlap at 188 turns, but exposed a 600 s non-streaming `REQUEST_TIMEOUT` silent
> double-billing / 29-minute stall on `t05` (`attempts=4`, `+$0.30` unrecorded server spend).
> All defaults unchanged; `.env.deepseek` live balance is **`$1.59 USD`** (Pi key `¥157.46 CNY`).

- **Baseline:** 5/18 on the faithful Slice A slice (`crates/eval/slices/tb-slice-a.json`, frozen
  recipe 300k/60/120); published frontier expectation on the same tasks 12.08/18, gap not
  attributable (flash-class vs frontier-class models).
- **Balance:** $1.56 of the $3.12 top-up. Report `dollars` are PEAK upper bounds, about 2x the
  bill; re-read the live balance before spending.
- **Closed, each measured:** more envelope (0 of 11 converted at 3.3x tokens), completion
  checklist, `sam-cell-seg` confirmation (8/9, 6/9, no solve), allowlist removal on the DNA pair
  (22 denials gone, no test flipped), zero-write gate (no within-task power), learned stuck
  detection (literature: within-task AUC 0.48–0.52).
- **Fixed:** snapshot rollback/patch no longer loses rows to root-owned artefacts (`dc7164e`).
- **Named deliverable, surviving lead, unmeasured.** All three published-easy misses never wrote
  the named file; 0 of 13 banked runs lacking it at step 15 solved, 0 of 2 solves would have been
  stopped. Implemented off: `--deliverable PATH` logs `deliverable-missing` at step 15 (shadow, no behaviour
  change), `--deliverable-steer on` adds one directive. Forcing arm **0 of 4** (directive read and
  ignored on both DNA tasks; the control row's write was lost to a root-owned file, since fixed in
  `write`/`edit`) and 1 solve in 18 flagged runs (a first `dna-insert` solve), so the predicate is
  not a safe stop signal; Slice A stays 5/18, balance $1.24 before and $0.80 after a 0 of 9
  variance rep that left the three misses stable (`dna-insert` 1 in 5).
  [allowlist-and-named-deliverable](experiments/2026-11-11-allowlist-and-named-deliverable.md),
  [deliverable-forcing-arm](experiments/2026-11-12-deliverable-forcing-arm.md),
  [easy-miss-variance](experiments/2026-11-13-easy-miss-variance.md).
- **Fixed (2026-11-13), effect unmeasured:** `exec` stripped backslashes inside double quotes
  (`grep "a\|b"` matched nothing; 78 of 1,698 banked calls, 13 of 49 traces) and showed a silent
  non-zero exit as empty output (79 of 475 failed exec calls). Every banked row predates the fix,
  so 5/18 was measured with both bugs; the 13-miss re-run gave 2 solves, none from the exec fixes
  (infra-corrected 6/18, single rep). Paid work then stopped, balance $0.36.
  [exec-observation-bugs](experiments/2026-11-13-exec-observation-bugs.md),
  [slice-a-misses-rerun](experiments/2026-11-13-slice-a-misses-rerun.md).
- **Agent tree stage 1 (2026-11-13), off, unmeasured:** `--agents FILE` gives `spawn`/`wait` for
  read-only children with a pool carved from `--budget-tokens` (`crates/rof/src/agents.rs`);
  nothing is wired into `rof eval`, so no benchmark row can use it, and stages 2 (worktree
  writers) and 3 (`message` edges) are rejected at load. Demoted 2026-11-14: no paper shows
  subagents lifting a weak model at matched tokens, Spine (arXiv 2610.37224) costs 3.8x for no
  gain, and child usage only reaches stderr (`agents.rs:512`) while the report reads root totals
  (`research/lit-sweep-2026-11-14.md`). Scout arm last, fixed input/output contract only.
- **Failed-trace audit (2026-11-14), zero model calls:** 47 of 52 failed runs are token deaths and
  2 end claiming verification, so the TB4 false-verification pattern does not explain the misses
  and no new silent tool-feedback class appeared. Efficiency finding only, no paid arm: 472 of
  2,330 requests (20%) are a lone helper-script write that `GOAL_TAIL` instructs, and `sh -c` runs
  still do it (15.6% against
  21.5%), so `--shell on` alone probably would not change it; argument stubbing has about 10%
  pooled headroom, scope collapse is not estimated, and cache savings are bounded at half the bill.
  [failed-trace-audit](experiments/2026-11-14-failed-trace-audit.md).
- **Miss diagnosis (2026-11-15):** none of the three stable misses is a knowledge gap; the model
  reaches the answer and runs out before writing the file. It paces by the step counter (25 of 27
  runs cite steps, 11 tokens), reads the `N/M` token figure both ways, and the batch rollback
  notice fires on 21% of tool batches. `--budget-line requests` (off by default) prints replies
  left at the last reply's cost; wired into `rof run`, `rof eval` (`run_config.budget_requests`)
  and Harbor (`budget_line`). The [A/B](experiments/2026-11-23-budget-line-ab.md) gave 1/4
  treatment and 0/4 control solves, it did not repeat, and the guardrail is undefined, so the flag
  stays opt-in. Rollback notice scope and write-and-run batching are unbuilt, needing design
  agreement and a paid arm on other tasks.
  [miss-diagnosis](experiments/2026-11-15-miss-diagnosis.md).
- **Closed:** the 8,000-token `max_tokens` cap is not the token-death cause (3.4% of spend); exact
  re-reads are 3.5% of tool calls, so raising `observation_keep` is unsupported
  ([reread-census](experiments/2026-11-12-reread-census.md)).
- **Product surface (owner request, off, unvalidated):** `rof chat`, `--resume`, `--stream`,
  `--mcp`; see the scope-reversal entry in `decisions.md`. Not capability bets.
- **Balance:** $1.45 before the forcing arm (about $0.43 at PEAK rates, roughly half billed).
- **Validity fixes (2026-11-12):** eval drops preflight-excluded instances, plain 400/422 is no
  longer retried, request extras cannot overwrite existing body keys.
- **Rival comparison downgraded (2026-11-12):** the published-rate table is unmatched external
  context and n=2 shared rival tasks is not a comparison; no rival runs planned.

The ledger paragraphs below describe the earlier lookup campaign and stand as history.

## Objective and current state

Build competitive coding-agent capability, judged on pinned external tasks and same-model
comparisons. Default admission requires total tokens per solved task at or below 1.3x the matched
reference; candidates may be retained above that ratio without being promoted or authorized for
spend. Cost is not the objective. Current external reference: Terminal-Bench CPU 4.1.0 (63 tasks,
tag `v4.1.0`, commit `4070d6cef56a7d20cb5dc98d5e10ada88a6b9440`, verified 2026-11-10, rechecked
2026-11-11; recheck again later).

**Ledger update 2026-11-11:** the balance is **$3.12**, a fresh top-up the owner authorises
spending when needed (with a standing instruction to be mindful). These figures stand as history,
neither reset nor reused. Spend order is triage-first: no paid arm until the zero-cost
classification of banked in-container rows names a qualifying cell. The owner authorized a new
subledger capped at $0.661849662, preserving the old $2.158150338 upper bound (including its
$0.324172800 unknown reservation) under the total $2.82 ceiling. That campaign also stopped:
Cargo control artifact 0.0, 26 forwarded calls, 25 receipts, then HTTP-200 body-read
`ConnectionResetError` without usage; no treatment/Freight row ran and neither expected task pair
is complete. New upper bound $0.361028760 including its separate $0.324172800 unknown
reservation; aggregate upper bound **$2.519179098**. The raw $0.300820902 gap is not usable while
stopped. Do not reset either ledger or infer missing usage. The
[phase 3–5 record](phase-3-5-implementation.md) and
[campaign summary](experiments/2026-11-11-lookup-campaign-summary.json) carry paths, hashes and
the incomplete verdict; original selected-task outcomes remain in the
[historical pilot](current-benchmark-pilot.md), and no new capability claim is made.

ROF's pilot internal deadline is 3,600s; the task-agent allowance is 28,800s. Verifier budgets
are separate and task-specific (for example WAL 1,800s, React 900s); do not conflate these
clocks. Archive-handle and Harbor adapter implementation is verified: root exercised collapse
plus `output_read` end to end, and 166 Rust tests, 4 adapter tests, Clippy and formatting passed
(reported 2026-11-11). Those fixes have no established benchmark effect; retention has the
selected-task evidence above, broader gains remain unestablished, and defaults stay unchanged.

## Prepared, not run: next experiment protocol

> Now (2026-11-25): the 48-run 300k screen completed with **0/48 solved** (0/12 per arm;
> 47/48 halted on tokens): [2026-11-25-tb41-option-screen](experiments/2026-11-25-tb41-option-screen.md).
> Future screens require non-zero control solve rate (easier TB4.1 tasks or the 1.2M pilot envelope).

1. Freeze a fresh development slice plus untouched holdouts from the then-current official CPU
   release before inference; record release/task hashes, runtime hash, Harbor overlay hashes, full
   config, oracle result and budget ledger, with a passing native oracle and release/hash preflight.
2. Freeze the pilot control from `current-benchmark-pilot.md` (1,200,000 gross tokens, 120 steps,
   240 actions, observation/thinking keep 5/2, legacy workflow, no failed-check preservation, Harbor
   `max_tokens=8000`) and change exactly one field per treatment (table below); Harbor always passes
   `--exec-policy any` and `--steer off` (`tools/harbor_rof.py`), defaults otherwise unchanged.
3. Charge every attempt and preserve failed-check state, final artifacts, native reward,
   completion, per-request usage, retries and wall time separately; a zero-solve control has no
   defined tokens-per-solved-task ratio.
4. Confirm survivors on untouched holdouts. No hidden-verifier output may choose runtime actions,
   procedures or candidates, and the control is resampled only when testing extra inference (review,
   reflection, candidate generation), so added calls have a token-matched comparison.

Harbor receives adapter options, not raw CLI arguments. Single-field treatment changes:

| Experiment | Harbor option change |
|---|---|
| Observation retention | `observation_keep: 32` |
| Reasoning retention | `thinking_keep: 256` |
| Minimal workflow | `workflow: "minimal"` |
| Preserve executed failing checks | `preserve_failed_checks: true` |
| Archived output retrieval | `archive_output: true` |
| Shell composition | `shell: true` |
| Response allocation — **screened 0/12 vs 0/12 at 300k, stays opt-in** (eliminated 21 ladder retries; 1 hard 32k reasoning truncation halt; see `experiments/2026-11-25-tb41-option-screen.md`) | `max_tokens: 32768` |
| Output shape: status first, head and tail, partial on timeout — **screened 0/12 vs 0/12 at 300k, stays opt-in** (22 `[exit N]`-first results, 0 timeouts; see `experiments/2026-11-25-tb41-option-screen.md`) | `exec_output: "head-tail"` |
| Rollback only on tool errors, notice only when files changed — **screened 0/12 vs 0/12 at 300k, stays opt-in** (cut rollback notices 36 → 1; see `experiments/2026-11-25-tb41-option-screen.md`) | `rollback_scope: "errors"` |

These are chosen settings; paired native task outcomes determine whether they help. Select an
experiment within verified remaining headroom (the table schedules no arms) and freeze a newly
built runtime for both arms with its hash recorded. The historical pilot binary and results remain
unchanged.

Prepared recipes, not benchmark runs or claimed results. Archive cap 64 MiB per command
(`crates/tools-std/src/runner.rs`); for that arm verify retrieval past the 120-character preview
through the returned run-owned handle. A preserved failing check must stay failed with its edits
persisting, and a nonexecuted check is not proof. Harbor `max_tokens` changes the first request
cap (default 8,000), leaving provider retry escalation unchanged. No hidden verifier input, paid
run or new result is authorized by these recipes.

## Proposed task slice and readiness

The [machine-readable task record](next-benchmark-slice.json) proposes development tasks
`freight-dispatch-shift` and `cargo-flight-dispatch`, with `coq-block-bound` and
`vf2-speedup-networkx` as prospective holdouts. Selection used public metadata and instructions for
coding relevance and resource fit: a convenience slice with incomplete historical exposure
auditing, so these tasks are neither historically unseen nor representative.

All eight public metadata/instruction hashes and the complete selected packages were checked on
2026-11-11; both development oracles and valid bad controls were validated, holdout oracles remain
unrun. A read-only snapshot showed 12 CPUs, about 15.9 GB total Docker memory and 3.6 GB free disk.
The metadata requests 2 CPUs and 4–8 GiB RAM with a 10 GiB storage ceiling per task, which is not a
minimum disk requirement: measure the actual image/build footprint and working headroom first. No
unrelated images were pruned; rebuildable Rust artifacts were cleaned for the native preflight.

## Deferred work register

All new proposals stay out of the default runtime; existing opt-in code is marked
implemented/off, historical restoration and capacity/source checkpoints are implemented/off with
their observed-need gates unestablished, and the rest is not built. Revisit only at the stated
gate. The paper audit below adds candidates but promotes nothing: their mechanics exist by
explicit owner request and efficacy remains conditional. Cross-run memory, trained predictors,
automatic search and runtime MAS stay outside that plan; the Phase 2 public completion predicate
is distinct from the deferred review/reflection policy.

| Idea and status | Why deferred; evidence and transfer limit | Prerequisite and revisit criterion |
|---|---|---|
| Automatic harness-search optimizer — **not built** | Gains on a disjoint split, but harness-choice studies find model choice dominates and harness changes move cost more than accuracy; no ROF selection result. Lineage [2603.28052](https://arxiv.org/abs/2603.28052) and the 2026 cluster [2604.25850](https://arxiv.org/abs/2604.25850), [2608.01918](https://arxiv.org/abs/2608.01918), [2608.23041](https://arxiv.org/abs/2608.23041), [2609.38349](https://arxiv.org/abs/2609.38349); also [2611.02079](https://arxiv.org/abs/2611.02079), [2611.12122](https://arxiv.org/abs/2611.12122); transfer unknown. | Both conditions, reviewed against the 2026 literature 2026-10-13 and still closed: (a) a cross-harness grid shows a reproducible capability or cost gap this harness could plausibly close by code search, and (b) the suite is large enough to measure the loop's own improvement — hundreds of tasks by [2610.04433](https://arxiv.org/abs/2610.04433)'s noise-floor calibration for pass-rate deltas, or a cost-metric loop on the frozen slice. One published run is ~60 harness candidates over hours with up to 10M diagnostics tokens per evaluation; at 21 tasks its gains are indistinguishable from rerun noise. Also matched external comparison plus untouched tasks first: revisit if a preregistered optimizer beats a fixed profile on held-out solve rate within 1.3x tokens/solved task. |
| Adaptive budget, restart and stall predictors — **not built** (`--stall-threshold` emits only off-by-default repeat-failure notices) | Equivalent to zero over a difficulty prior; 0/103 live restart comparisons certified. SPLIT spans 7 benchmarks and 6 verifier kinds, not evidence for a ROF policy. [2611.08285](https://arxiv.org/abs/2611.08285), [2610.36544](https://arxiv.org/abs/2610.36544), [2610.23255](https://arxiv.org/abs/2610.23255). | Calibrated trajectories with public checks and a matched no-predictor baseline; revisit only on held-out solve gain and guardrail pass. |
| More memory, staleness eviction, procedure learning — **procedure loading and offline distillation off/manual; adaptive memory and eviction not built** | WAL 1 clean pass after two zero controls, Session arms all 0, React transfer interrupted; one-task results do not establish transfer. Mechanical extraction kept 79.1% of recurring detail against ≤16.0% for LLM summaries, with cache re-prefill and context rot, in settings unlike ROF. [2610.31710](https://arxiv.org/abs/2610.31710), [2610.28577](https://arxiv.org/abs/2610.28577), [2611.04169](https://arxiv.org/abs/2611.04169); procedure proposals unvalidated. | Paired replications on fresh tasks, then cross-task holdouts; each knob separately, promoted only on repeatable gain at no >1.3x ratio. Procedure files development-derived, hashed, held-out tested. |
| Automatic LLM compaction — **opt-in `--compaction FRAC`, default off, unvalidated** | LLM summaries kept ≤16.0% against 79.1% for mechanical extraction in one study; no coding-agent impact or ROF comparison. [2610.31710](https://arxiv.org/abs/2610.31710). | Unchanged control with trace-level fidelity checks, then holdouts; promote only after matched held-out gain within the guardrail. |
| ROF subagent concurrency / MAS — **not built** | Horizon-dependent: +14.3pp Claude Code on LoopsBench, −24.0pp SWE-bench Verified, 1.41–3.31x tokens over 354 tasks / 2,124 executions, in settings unlike ROF; transfer uncertain. The measured allowlist/policy core cuts attack effects at small utility cost ([2610.07639](https://arxiv.org/abs/2610.07639)). [2610.10263](https://arxiv.org/abs/2610.10263). | A defined decomposition and token-matched resampling control; revisit only on held-out gain under the 1.3x guardrail. A supervising orchestration/policy layer above harnesses: reconsider only when ROF runs unattended on machines its author does not own or is driven by more than one operator (gate reviewed and kept closed 2026-10-13). |
| Review, reflection and candidates — **review and candidate helper off/manual; general reflection policy not built** (`--review on`, `tools/rof_candidates.py`) | Extra inference and public-check selection are not measured capability gains: 18 self-inspection comparisons were negative only for 1.5B/3B/7B on math, and retry-with-feedback beat resampling in simulated robots but its feedback reading was overstated. [2607.28576](https://arxiv.org/abs/2607.28576), [2611.06863](https://arxiv.org/abs/2611.06863); `lit-2026-11-ERRATA.md` E3. The helper is explicit, not runtime branching, and not a tamper-resistant grader. | Public-check and provider readiness, then a token-matched resampling control and holdouts; promote only on net held-out gain, hidden tests never select candidates. |
| Schema / KV-server optimization — **not built** | Schema constraints suppressed tool calls on several open-weight models, hosted transfer unverified; no ROF bottleneck is measured and requests enable no schema-constrained response formatting. Deterministic-compilation savings are local-only; cache rolling-drop beats threshold compaction elsewhere and is unavailable on the hosted API. [2606.25605](https://arxiv.org/abs/2606.25605), [2605.04107](https://arxiv.org/abs/2605.04107), [2610.28577](https://arxiv.org/abs/2610.28577). | Profile a measured failing workload first; revisit only if a benchmark task shows the bottleneck and an isolated change improves success or feasibility. |
| Dynamic / lossy context (per-read summarizer) — **not built** (related `--compaction`/`--checkpoint-sources`, `--history-read`, `--archive-output` + `output_read` are implemented, off, unbenchmarked) | Payloads it would shrink are `view` bodies, `search` dumps and `exec` output; cmhc rows die at the 300k cap after 23-33 reads / 0-1 writes, so the target is candidate-visible, not measured. Named costs: silent failure (the dropped line is the one that mattered) and one extra unverifiable model call per long read. | BOTH (a) an offline replay of banked event logs attributes the binding wall on failing cells to result bodies, not reasoning or round-trips, and (b) retention can be scored without a judge (e.g. re-fetch fidelity against banked workdirs). Cheaper first step: measure the `view` cap hit rate before adding a summarizer. |
| Bet C, candidate racing — **not built** (unwired selection helper removed 2026-11-25) | No workload has shown proof-gate interventions (zero in all measured runs). | Wire only when a workload shows proof-gate interventions. |

These pointers are subject-matter evidence, not proof of transfer. Reviewed reports:
`lit-2026-11-SYNTHESIS.md`, `lit-2026-11-ERRATA.md`, `lit-2026-11-harness-architecture.md`,
`lit-2026-11-context-engineering.md`, `lit-2026-11-tools-inference.md`,
`lit-2026-11-eval-verifiers.md`. Benchmark claims require controlled ROF results.

## Direct paper-summary audit: additional candidates (2026-11-11)

The integration owner read all 72 per-paper summaries including the copied lane analysis, and
checked eight in full text through the Papers service: 2610.07863, 2610.34967, 2610.38620,
2610.27524, 2607.15516, 2611.02079, 2610.34679, 2610.20927. Not a review of 72 full papers.
Recommendation: keep the current implementation as control and test these narrow mechanisms before
broad harness search. No paid model calls or runtime changes occurred; transfer to ROF remains
unmeasured.

| Candidate or evaluation change | Evidence and smallest ROF application | Validation and main limitation |
|---|---|---|
| Evidence bundles for repository edits | Rivet, [2610.34967](https://arxiv.org/abs/2610.34967): higher Pass@1 across 16 settings at 4K/8K from separating target locations, helper definitions, caller usages and config constraints. Extend `repo_lookup` and context selection to symbol occurrences, tests and config paths, not a graph service. | Same lookup budget on fresh tasks, measuring missing evidence and solves. Literal matches cannot claim semantic caller resolution; budgets differ from ROF. |
| Tool behavior as an optimization surface | AT-AT, [2610.34679](https://arxiv.org/abs/2610.34679) Table 3: TB2 pass@1 49.32% to 51.36%, TBLite 75.60% to 77.20%, by optimizing tool descriptions and permitted implementations. Change one contract after diagnosing malformed calls in traces. | Native held-out results must improve; count optimizer calls. Older benchmarks, other models, the LLM optimizer is not free. |
| Targeted executable completion checks | JevAct, [2610.38620](https://arxiv.org/abs/2610.38620), Airline single trial: ReAct 50%, checks alone 68%, full gate 72%. Bounded feedback from a public task command or deliverable predicate; `VerifyState` notices passing checks but does not establish arbitrary task completion. | Public checks only, never hidden tests; measure false rejection on baseline successes. One gate flagged 26% of solved episodes on Airline and was disabled. |
| Restore historical observations, with stable rendering boundaries | ReFold, [2610.07863](https://arxiv.org/abs/2610.07863): appends a logged step's original output as a new turn, preserving the prefix. `output_read` covers archived output only; reuse the durable log, mark restorations historical, examine `--collapse-hysteresis` first. | Verify bounded retrieval after file mutation and call/result identity; test restoration and rendering cadence separately. vLLM cache gains are unavailable on the hosted API. |
| Source-backed facts in checkpoints | SJC, [2610.20927](https://arxiv.org/abs/2610.20927): compressor-written plans reused as facts. `context::SUMMARY_PROMPT` asks for decisions and next steps; preserve source IDs and mark hypotheses and planned actions. | Test continuation on unchanged history before enabling compaction. Provenance is not correctness, filtering can drop correct entries, and its judge adds cost. |
| Separate discovery from deployment admission | ParetoSaddler, [2611.02079](https://arxiv.org/abs/2611.02079): keeps candidates that temporarily violate cost preferences (Appendix A disables the local safeguard so fixing premature termination can add useful work). Keep them as research candidates. | The USD ceiling stays hard; promotion needs held-out evidence and the 1.3x guardrail. Retaining a candidate is not permission to spend or deploy. |
| Measure rescue and harm, including baseline successes | MemDev-Bench, [2610.27524](https://arxiv.org/abs/2610.27524): Improvement and Preservation tasks; raw histories keep process information compressed memories omit, and useful memories can regress solvable tasks. Record both baseline-fail/treatment-pass and baseline-pass/treatment-fail transitions. | Choose tasks and splits before observing outcomes; an evaluation design, not a memory benefit. Do not reopen retired task tuning for a preservation case. |
| Separate context capacity from cumulative token allowance | **Fixed** (2026-11-11): `crates/agent-loop/src/request.rs:483-488` passes `context_capacity_tokens - output_reserve_tokens` into `context::compaction_due`; `--compaction` requires `--context-capacity` and `--output-reserve`. Run-wide `max_tokens` no longer reaches `compaction_due`. | Remaining: a scripted-provider test varying the run allowance at fixed capacity and vice versa. Still default-off with no benchmark result. |

These are candidates, not a scheduled sweep: the unchanged control costs no implementation effort
but establishes nothing. Start with trace-grounded tool changes and evidence selection, use
restoration if traces show unrecoverable historical evidence, and defer checkpoint changes until a
compaction experiment is justified. Largest uncertainty: independent ROF benchmark effects have not
been measured.

### Report interpretations requiring correction

- The CAPC report's quality-loss reading is too broad. In
  [2607.15516](https://arxiv.org/abs/2607.15516), Table 10's 0.703/0.603 values measure
  tool-selection set overlap with the vanilla output, not task accuracy; Table 14 reports the same
  success count for CAPC and vanilla, 36/50, at average costs $0.1145 and $0.1244 per task. Equal
  counts on 50 tasks do not establish quality equivalence, so do not change the hosted provider's
  caching scheme on this Anthropic-specific study.
- A run exhausting its configured response/token budget remains evidence about the evaluated
  system: record that termination cause separately, and do not erase its task result or drop it
  from the denominator by relabeling it infrastructure failure. The inference report's blanket
  recommendation to classify truncation as infrastructure is not an evaluation rule to adopt.
- The reports' broad rejection of verification misses JevAct's distinction between targeted
  executable predicates, unconditional re-check prompts and model-specific effects. It supports
  testing a narrow predicate; it does not prove a generic reviewer improves ROF.
- The two DOI-only summaries contain no retrieved abstract; their CLEO parity and TurnCost
  efficiency figures remain unverified here and support none of the recommendations above.

Original summaries and lane reports were left unchanged. This addendum records interpretations
and proposed experiments; it makes no new benchmark claim.
