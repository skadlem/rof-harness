# Research and implementation handoff (2026-11-11)

**Active execution reference:** the
[phased implementation plan](superpowers/plans/2026-11-11-capability-phases.md)
assigns subagents, freezes additive contracts, and defines per-phase gates for
the direct paper audit below. Phases 0–4 are implemented and locally verified,
with all new mechanisms opt-in. Phase 5 ran one interrupted control and stopped
on a new unknown request; no matched pair completed. Both ledgers/reservations
remain intact. The observed-need gates and paid stop below govern future work.
The [phase 0–2 record](phase-0-2-implementation.md) and
[phase 3–5 record](phase-3-5-implementation.md) distinguish implementation,
chosen fixtures, historical outcomes and current campaign evidence.

## Objective and current state

Build competitive coding-agent capability, judged on pinned external tasks and
same-model comparisons. Default admission requires total tokens per solved task
at or below 1.3x the matched reference; development candidates can be retained
above that ratio without being promoted or authorized for additional spend.
Cost is not the objective. Current external
reference is Terminal-Bench CPU 4.1.0 (63 tasks, tag `v4.1.0`, commit
`4070d6cef56a7d20cb5dc98d5e10ada88a6b9440`, verified 2026-11-10). The latest official release was rechecked as v4.1.0 on 2026-11-11 before the
bounded continuation; recheck again for later campaigns.

**Ledger update 2026-11-11:** the account balance is **$3.12**, a fresh top-up the
owner authorises spending when needed (with a standing instruction to be mindful).
The campaign figures below stand as history and are neither reset nor reused.
Spend order is triage-first: no paid arm until the zero-cost classification of
banked in-container rows names a qualifying cell. The original official-account ledger remains unchanged. The owner authorized a
new subledger capped at $0.661849662, preserving the old $2.158150338 upper bound
(including its $0.324172800 unknown reservation) under the total $2.82 ceiling.
The new campaign also stopped: Cargo control artifact 0.0, 26 forwarded calls,
25 receipts, then HTTP-200 body-read `ConnectionResetError` without usage.
No treatment/Freight row ran and neither expected task pair is complete.
The new upper bound is $0.361028760, including its separate $0.324172800 unknown
reservation; aggregate upper bound is **$2.519179098**. The raw $0.300820902 gap is
not usable while stopped. Do not reset either ledger or infer missing usage.
The [phase 3–5 record](phase-3-5-implementation.md) and
[campaign summary](experiments/2026-11-11-lookup-campaign-summary.json) carry paths,
hashes and the incomplete verdict. Original selected-task outcomes remain in the
[historical pilot](current-benchmark-pilot.md); no new capability claim is made.

ROF's pilot internal deadline is 3,600s; the task-agent allowance is 28,800s.
Verifier budgets are separate and task-specific (for example, WAL 1,800s and
React 900s). Do not conflate these clocks. Archive-handle and Harbor adapter
implementation is verified: root exercised collapse plus `output_read` end to
end; 166 Rust tests, 4 adapter tests, Clippy and formatting passed (reported
2026-11-11). Those fixes have no established benchmark effect. Retention has the
selected-task evidence above; broader capability gains remain unestablished.
Defaults stay unchanged.

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

All eight public metadata/instruction hashes and the complete selected packages
were checked on 2026-11-11. Both development oracles and valid bad controls were
validated; holdout oracles remain unrun. Before building, a read-only host snapshot showed 12 CPUs, about
15.9 GB total Docker memory, and 3.6 GB free disk. The selected task metadata
requests 2 CPUs and 4–8 GiB RAM, with a 10 GiB storage ceiling per task. That
ceiling is not a minimum disk requirement; measure actual image/build footprint
and working headroom before building. No unrelated images were pruned; rebuildable Rust artifacts were cleaned to
make room for the native preflight. The current single interrupted control and
its limits are recorded above.

## Deferred work register

All new proposals stay out of the default runtime. Historical restoration and
capacity/source checkpoints are now implemented/off; their observed-need gates
remain unestablished. Existing opt-in code is marked
implemented/off; other proposals are not built. Revisit only at the stated gate.
The direct paper-summary audit below adds candidates and qualifications to these
gates; it does not promote them. Their mechanics were implemented by explicit
owner request; efficacy remains conditional. Cross-run memory, trained predictors, automatic search, and runtime MAS remain
outside that plan. A targeted public completion predicate in Phase 2 is distinct
from the deferred general review/reflection policy.

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

## Direct paper-summary audit: additional candidates (2026-11-11)

The integration owner personally read all 72 per-paper summaries, including the
authors' abstracts where available and the copied lane analysis. Relevant methods, results and
limitations were additionally checked in eight full-text sources through the
Papers service: 2610.07863, 2610.34967, 2610.38620, 2610.27524, 2607.15516,
2611.02079, 2610.34679 and 2610.20927. This is not a review of 72 full papers.
Recommendation: keep the current implementation as control and test the narrow
coding mechanisms below before broad harness search. No additional paid model
calls or runtime changes occurred. Transfer to ROF remains unmeasured.

| Candidate or evaluation change | Evidence and smallest ROF application | Validation and main limitation |
|---|---|---|
| Evidence bundles for repository edits | [Rivet, 2610.34967](https://arxiv.org/abs/2610.34967) distinguishes target locations, helper definitions, caller usages and configuration constraints. Its 4K/8K context experiments report higher Pass@1 across 16 settings. Infer a small extension to existing `repo_lookup` and context selection, starting with explicit symbol occurrences, tests and configuration paths; avoid a graph service. | Compare with the same lookup/context budget on fresh coding tasks; measure missing evidence and solves. Literal matches cannot claim semantic caller resolution. The paper's tight context budgets differ from ROF. |
| Tool behavior as an optimization surface | [AT-AT, 2610.34679](https://arxiv.org/abs/2610.34679), Table 3: OpenCode held-out TB2 pass@1 49.32% to 51.36%, TBLite 75.60% to 77.20%, after optimizing tool descriptions and permitted implementations. Diagnose repeated malformed calls and recovery failures in existing traces, then change one contract or behavior. | Native held-out results must improve; count optimizer calls if an optimizer is introduced. These are older benchmarks and different models. The optimizer uses an LLM and is not free. |
| Targeted executable completion checks | [JevAct, 2610.38620](https://arxiv.org/abs/2610.38620), single-trial Airline component ablation: ReAct 50%, executable checks alone 68%, full gate 72%. Infer bounded feedback from a task-specified public command or explicit deliverable predicate. Existing `VerifyState` notices passing checks; it does not establish arbitrary task completion. | Use only public requirements/checks, never native hidden tests as feedback. Measure false rejection on baseline successes. One hand-off gate was disabled on Airline after flagging 26% of otherwise solved episodes; this is not evidence for blanket completion blocking. |
| Restore historical observations, with stable rendering boundaries | [ReFold, 2610.07863](https://arxiv.org/abs/2610.07863) restores a logged step by appending its original output as a new turn, preserving the current prefix. Existing `output_read` covers archived command output; it does not restore every historical observation. Reuse the durable log, mark restored observations as historical, and examine existing `--collapse-hysteresis` before introducing another context mechanism. | First verify bounded retrieval after file mutation and preservation of call/result identity. Then test restoration and rendering cadence separately. Self-hosted vLLM cache/throughput gains cannot be promised on the hosted API. |
| Source-backed facts in checkpoints | [SJC, 2610.20927](https://arxiv.org/abs/2610.20927) identifies compressor-authored plans being reused as facts. The current `context::SUMMARY_PROMPT` asks for key decisions and next steps. Preserve source IDs for observed facts and distinguish hypotheses and planned actions. | Test continuation against unchanged history before enabling compaction. Provenance is not correctness, and aggressive filtering can remove useful correct entries; do not simply delete every unquoted statement. The source's judge adds inference cost. |
| Separate discovery from deployment admission | [ParetoSaddler, 2611.02079](https://arxiv.org/abs/2611.02079) keeps useful intermediate candidates even when they temporarily violate deployment cost preferences. In Appendix A, the local cost safeguard is disabled because fixing premature termination can increase useful work. Keep such configurations as research candidates rather than discarding them immediately. | The existing total USD ceiling remains hard. Default promotion still requires held-out capability evidence and the 1.3x tokens-per-solved guardrail. Retaining a candidate is not permission to spend on it or deploy it. |
| Measure rescue and harm, including baseline successes | [MemDev-Bench, 2610.27524](https://arxiv.org/abs/2610.27524) evaluates both Improvement and Preservation tasks. Raw histories preserve process information that compressed memories omit; useful memories can also regress otherwise solvable tasks. Record both baseline-fail/treatment-pass and baseline-pass/treatment-fail transitions for context or procedure experiments. | Select tasks and splits before observing treatment outcomes. These measures generalize as an evaluation design, not as a predicted memory benefit. Do not reopen retired task tuning merely to add a preservation case. |
| Separate context capacity from cumulative token allowance | **Fixed** (verified 2026-11-11): `agent-loop/src/request.rs:445-448` passes `context_capacity_tokens - output_reserve_tokens` into `context::compaction_due`, and `--compaction` requires both `--context-capacity` and `--output-reserve`. The earlier inspection (run-wide `max_tokens` reaching `compaction_due`) no longer describes the code. | Remaining check: a scripted-provider test should vary the run allowance at fixed context capacity and vice versa. Still a default-off feature with no benchmark result. |

These are candidates, not a scheduled sweep. The unchanged control costs no new
implementation effort but cannot establish whether these mechanisms help. Start
with trace-grounded tool changes and evidence selection; use restoration if traces
show unrecoverable historical evidence, and defer checkpoint changes until a
compaction experiment is justified. Keep the largest uncertainty explicit:
independent ROF benchmark effects have not been measured.

### Report interpretations requiring correction

- The CAPC report's quality-loss interpretation is too broad. In
  [2607.15516](https://arxiv.org/abs/2607.15516), Table 10's 0.703/0.603 values
  measure tool-selection set overlap with the vanilla output, not task accuracy.
  Table 14 reports the same task-success count for CAPC and vanilla, 36/50, at
  average costs $0.1145 and $0.1244 per task. Equal counts on 50 tasks do not
  establish general quality equivalence. Avoid changing the hosted provider's
  caching scheme based on this Anthropic-specific study.
- A run exhausting its configured response/token budget remains evidence about
  the evaluated system. Record that termination cause separately; do not erase
  its task result or remove it from the denominator by relabeling it as
  infrastructure failure. The inference report's blanket recommendation to
  classify truncation as infrastructure is not an evaluation rule to adopt.
- The reports' broad rejection of verification misses JevAct's distinction
  between targeted executable predicates, unconditional re-check prompts, and
  model-specific effects. Its ablation supports testing a narrow predicate;
  it does not prove that adding a generic reviewer improves ROF.
- The two DOI-only summaries contain no retrieved abstract. Their CLEO parity
  and TurnCost efficiency figures remain unverified here and do not support
  the recommendations above.

Original summaries and lane reports were left unchanged. This addendum records
interpretations and proposed experiments; it makes no new benchmark claim.
