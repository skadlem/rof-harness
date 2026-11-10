# ROF capability improvements: phased implementation plan

**Status:** planned, not implemented. The output-fidelity foundation is complete;
the tasks below start from `3e205ea`. The owner requested this implementation plan
and documentation refresh on 2026-11-11. No new runtime changes or paid runs are
part of writing it.

**Goal:** improve externally verified coding-task completion through better
repository evidence, trace-grounded tools, and targeted execution feedback.

**Architecture:** extend the existing sequential loop, optional tools, native
Harbor adapter, and offline analysis helpers. Keep the durable log as the source
of historical observations. Separate deployment admission from development
candidate retention; implement no automatic optimizer, planner, or runtime MAS.

**Tech stack:** existing Rust crates, Python standard-library analysis, installed
Harbor/Pydantic for adapter checks, and the existing Docker task runner.

## Global constraints and execution order

- All unmeasured mechanisms remain opt-in. Preserve absent-flag behavior and the
  six-tool default registry; fixes must not silently promote a research profile.
- Existing public traits and enum variants stay compatible. Freeze additive
  interfaces below before implementation workers depend on them; each owning
  worker lands its contract declarations before consumers start.
- One exclusive writer per crate directory. Root owns `crates/rof/` integration
  and project documentation; other owners never edit these files concurrently.
- No new crates, graph service, embeddings, learned judge, or third-party package.
  One existing internal dependency is allowed for historical log reading.
- Keep original benchmark instructions and official graders unchanged. Native
  hidden verifier results cannot drive repair, retrieval, procedures, or selection.
- Capability is primary. The 1.3x tokens-per-solved guardrail governs default
  admission, not whether a promising development candidate can be retained.
  The authorized USD cap and all outstanding reservations remain hard limits.
- The current campaign is stopped: one unknown request; $2.158150338 upper bound
  **includes** its $0.324172800 reservation, within $2.82. No paid phase starts
  until accounting is reconciled under the existing stop rule. Never reset it.
- Verify current official benchmark release before future spend. TB4.1.0 CPU is
  the last verified pin, not an assertion about the latest release on execution day.
- Research input remains unchanged. Preserve historical results and frozen pilot
  binaries; build and hash a new runtime for each new code comparison.

Chosen ordering: **0 → 1 → 2 → 5 → 6**. Phase 3 joins only when traces show a
historical-evidence recovery problem. Phase 4 joins only when a compaction
experiment is justified. Neither conditional phase blocks earlier benchmark
comparisons. Implementation checks are free of model calls; phase 5 is spend-gated.

## Team and parallel-work contracts

Root orchestrates GPT-6 Luna workers, checks contracts and evidence, integrates
the CLI/adapter paths, and decides which experiment to schedule. Use at most
three workers alongside root. Spawn with `model="gpt-6-luna"` and a self-contained
task; replace a finished builder with a fresh Luna reviewer rather than adding
a fifth concurrent agent.

| Owner | Exclusive files | Parallel work permitted |
|---|---|---|
| Luna tools | `crates/tools-std/` | Root CLI wiring after the tool/config contract is frozen; a loop worker may own `agent-loop` separately. |
| Luna loop | `crates/agent-loop/` | Tools and offline reporting; serialize completion, history rendering, and checkpoint edits within this crate. |
| Luna context | `crates/context/` | Offline reporting and adapter work; its additive config/types must land before loop consumers. |
| Luna analysis/adapter | Assigned `tools/rof_compare.py`, its test, or `tools/harbor_rof.py` and its test | These paths can have separate workers only with explicit disjoint assignments. |
| Root integration | `crates/rof/`, docs, README | Integrate frozen sibling APIs; never edit a worker-owned crate during its task. |

Every worker task contains: starting commit, exact allowed paths, input/output
contract, default behavior, failing case, commands, and stop conditions. Require
an observed failing regression before nontrivial behavior changes. Return the
diff, red/green evidence, and limitations. Root reviews source and runs integration
checks before committing. No worker edits another owner's files or pushes.

## Phase 0 — freeze the reference and contracts

**Owner:** root, with read-only Luna source inspection. **Dependencies:** none.

- [ ] Record a clean reference source commit, runtime SHA-256, runner/overlay
  hashes, and the complete nonsecret recipe in the new experiment manifest.
  Do not reuse old pilot outcomes as a new-binary control.
- [ ] Confirm existing mechanisms before adding code: `repo_lookup`, minimal
  workflow, failed-check preservation, shell, output archive, thinking retention,
  collapse hysteresis, review, and proof commands already exist in some paths.
- [ ] Freeze the per-phase interfaces below. Record any necessary signature
  revision before dispatching consumers; do not invent stubs for conditional work
  before its gate is met.
- [ ] Preserve the current task/split proposal in `docs/next-benchmark-slice.json`.
  Its packages/oracles and historical exposure remain unvalidated; it is not a
  representative suite or a certified historically unseen holdout.

**Exit:** reviewable manifest and owner assignments, with all planned behavior
clearly distinguished from shipped behavior. No model calls.

## Phase 1 — make experiments observable and reachable

### 1A. Paired reporting, rescue/harm, and candidate status

**Owner:** Luna analysis. **Files:** create `tools/rof_compare.py` and
`tools/test_rof_compare.py`; reuse the zero-solve semantics in
`crates/eval/src/report.rs` without changing that crate.

Frozen Python entry points:

```python
def compare(experiment: dict) -> dict: ...
def main(argv=None) -> int: ...
```

Input is a JSON object with `protocol`, `arms`, and `rows`. `protocol` pins
dataset/release/task hashes, model/endpoint identity, runner/grade path, budgets,
splits and allowed treatment difference. `arms` pins each runtime hash and config.
Each row contains `arm`, `task_id`, `attempt_id`, `replicate_id`, `artifact_reward`
(number or null), `run_status`, `run_completed`, `accounting_complete`, `usage`
(null or input/cached-input/output totals including retries), and artifact source
paths/hashes. Explicitly supplied JSON is normalized evidence, not an excuse to
invent missing usage or infer success from an actor exit code.
`attempt_id` identifies an agent trial; provider retries belong in that trial's
usage rather than masquerading as independent task repetitions. `usage` keys
are exactly `input_tokens`, `cached_input_tokens`, `output_tokens`, all
nonnegative integers with cached input no larger than input. `protocol.pairs`
lists the expected `(task_id, replicate_id)` pairs so missing rows are visible
even when both arms lack the same task. Require at most one row per arm/pair.
The helper reports token/capability evidence; financial reconciliation remains
in the existing reservation ledger, linked by source path/hash in `protocol`.
It neither computes an invoice nor converts an unknown charge/reservation to $0
or authorizes more spend.

`run_completed` means the attempt reached a valid protocol terminal state, not
that the model declared success. Configured token/step/wall-limit terminations
remain system attempts with their native artifact rewards; they are not removed
from the task denominator as infrastructure failures.

- [ ] Write failing cases for 00/01/10/11 outcomes, duplicate attempts, mismatched
  tasks/protocols, unknown usage, budget-limited failure, interrupted reward-1
  artifacts, unequal repeat counts, and zero-solve ratios.
- [ ] Implement deterministic JSON output with per-task paired rows, rescue,
  harm, unchanged outcomes and explicit incomplete pairs. Report raw artifact
  results separately from completed/accounted comparisons; list every attempt.
- [ ] Compute input/output totals across all attempts, including failures and
  retries. Never add cached-input twice. Return null for tokens-per-solved or
  the treatment/control ratio when denominators or usage are unavailable.
- [ ] Average repetitions within task before equal weighting across tasks.
  Keep incomplete tasks visible; do not improve a score through exclusions.
- [ ] Record `retained_for_research` separately from `eligible_for_promotion`.
  Do not implement automatic default promotion, significance claims on the
  four-task proposal, or an automatic search scheduler.

Run: `python3 -m unittest discover -s tools -p 'test_rof_compare.py' -v`.
CLI: `python3 tools/rof_compare.py --input EXPERIMENT.json --output REPORT.json`.

**Exit:** synthetic fixtures preserve the interrupted React distinction and
zero-solve semantics; no campaign or provider is contacted.

### 1B. Expose existing lookup and rendering controls through Harbor

**Owner:** Luna adapter; root owns any `rof` integration test.
**Files:** `tools/harbor_rof.py`, `tools/test_harbor_rof.py`.

```python
repo_lookup: bool = False
collapse_hysteresis: int | None = Field(default=None, gt=0)
```

- [ ] Observe failing forwarding checks, then pass `--repo-lookup on` only when
  enabled and `--collapse-hysteresis N` only when supplied.
- [ ] Keep existing goal, budgets, exec policy, retention, workflow, shell and
  archive behavior unchanged. Both controls must remain independently testable.
- [ ] Capture actual adapter argv and validate it with the built ROF parser,
  using a deliberately missing credential variable to stop before inference.
- [ ] Do not claim parity with the legacy `rof eval --agent` CLI. Current native
  experiments use the installed Harbor adapter, which invokes `rof run`.

Run: `/home/madiyar/.local/share/uv/tools/harbor/bin/python -m unittest discover -s tools -p 'test_harbor_rof.py' -v`.

**Exit:** false/absent options preserve the old argv; enabled values reach the
real CLI. This lane can run alongside 1A and the trace audit below.

## Phase 2 — improve coding evidence and targeted feedback

### 2A. Trace-grounded tools and evidence-aware lookup

**Owner:** Luna tools. **Files:** `crates/tools-std/src/experimental.rs`,
`runner.rs`/individual tool files only where a reproduced defect requires them,
and `crates/tools-std/tests/experimental.rs`. Root registers integration tests
under `crates/rof/` after the contract is stable.

- [ ] Audit recorded development actor traces, excluding hidden grader content.
  Count repeated malformed calls, unrecoverable outputs, and missing repository
  evidence. Preserve trace IDs and exact tool schemas/binary identities.
- [ ] Reproduce one remaining tool-contract failure locally. If none remains,
  record that result and skip speculative execution API changes. In particular,
  do not add a second shell tool or automatic LLM tool optimizer.
- [ ] Add `repo_evidence: bool = false` to `ExperimentalToolsConfig`, CLI
  `--repo-evidence on|off`, and the Harbor option; require `repo_lookup` when it
  is enabled. Preserve the tool's required `symbol`, optional `limit`, and
  default output. The control cannot select enriched mode through an argument:
  activation is an explicit run configuration for clean ablation.
  Evidence mode groups bounded literal locations, definition candidates, test
  path candidates and configuration path candidates. These are explicit
  heuristics, not a semantic caller graph or guaranteed complete dependencies.
- [ ] Reuse current traversal, containment, cancellation and scan caps. Keep
  one shared result limit across categories. Bound evidence-mode output to
  `common::OUT_CAP` (8,000 characters), with an explicit omission/scan-limit
  notice and `ToolOutcome.truncated`; preserve legacy-mode output. The normal
  loop's `tool_core::MAX_MODEL_CHARS` bound remains unchanged. Current lookup
  has no tool-local character cap, so this is an evidence-mode contract rather
  than a claim about its old behavior. Deduplicate repeated path/line hits and
  define test/config path heuristics explicitly in fixture tests.
- [ ] Test old calls, mixed-language definition candidates, duplicate hits,
  test/config paths, deterministic ordering, output limits, and existing escape
  and cancellation behavior. A planted caller/config dependency must be findable
  in evidence mode without asserting that arbitrary semantic dependencies resolve.

**Exit:** unchanged default-tool behavior and a small evidence-mode treatment.
Benchmark the exposed original lookup before crediting the enriched version.
Rivet and AT-AT motivate this bet, not an expected ROF effect size.

### 2B. A bounded, public completion predicate

**Owner:** Luna loop, in parallel with 2A. **Files:**
`crates/agent-loop/src/run.rs`, `state.rs`, `verify.rs`, focused loop tests.
Root later wires `crates/rof/src/cli.rs`, `main.rs`, fixture initializers,
`tools/harbor_rof.py`, and adapter tests while the adapter worker is idle.

Frozen additive contract: `RunConfig.completion_check: Option<String>` defaults
to `None`; CLI `--completion-check CMD`; Harbor `completion_check: str | None`.
The command is explicitly supplied by the task/operator. Zero exit satisfies
this predicate, not a claim that arbitrary task requirements are proven.
Add `FailureKind::Completion` at the contract gate and adapt exhaustive consumers;
do not rename existing variants or misclassify an unmet predicate as budget or
infrastructure failure.

- [ ] Write failing scripted cases: passing predicate, failing predicate then
  successful repair, two failed predicate checks, policy rejection, timeout,
  cancellation, and exhausted step/token budget. Include a valid no-edit task.
- [ ] Intercept `ClaimOutcome::Done` in `settle_success` before terminal
  settlement can close the run. Route the exact command through the existing
  policy-checked, wrapped `test` dispatch and logging/budget path. Do not use the
  host proof executor or a hidden native verifier. Reserve action capacity before
  dispatch and add no allowlist grants. Record the invocation as harness-origin
  with a matching tool-call/result pair; do not fabricate provider usage.
- [ ] Allow at most one repair continuation and two predicate executions total.
  Both share the original usage totals, action caps, deadline and cancellation.
  A first failed check sets continuation state once and supplies its new result
  as feedback; a second proposed completion executes the final check. Unmet or
  unknown predicates return `Outcome::Failed { kind: FailureKind::Completion,
  message }`; genuine budget exhaustion keeps its normal outcome. Preserve a
  gradeable artifact. The existing last-step Done grace cannot buy an extra
  provider request for repair after a failed predicate.
- [ ] Disable repeated generic verification nudges for this completion path so
  they cannot multiply the repair allowance. A previous unrelated passing check
  cannot satisfy the exact configured predicate. Default `None` is unchanged.
- [ ] Record command, exit/timeout/denial, output and intervention count using
  existing logs/events. Preserve failed-check rollback semantics as configured.

**Exit:** bounded behavior is verified locally and does not falsely require a
patch on read-only tasks. Measure false rejection on reference successes before
promotion; no universal completion rule is inferred from JevAct.

## Phase 3 — conditional historical restoration

**Gate:** actor traces show necessary historical observations cannot be recovered
by existing `view` or `output_read`. Root records the concrete case before coding.

**Owners:** Luna tools and Luna loop in parallel after additive config contracts
are frozen; root then integrates `rof` and Harbor. No simultaneous second writer
for `tools-std` or `agent-loop`.

**Files:** `crates/tools-std/{Cargo.toml,src/experimental.rs,tests/experimental.rs}`;
`crates/agent-loop/src/{request.rs,run.rs}` and fold tests; root CLI/registry/tests.
Reuse `agent-log::read_log`; add the existing internal `agent-log` dependency to
`tools-std`, with no reverse dependency or new crate.

Frozen interfaces: `ExperimentalToolsConfig.history_log: Option<PathBuf>` is
host-supplied and fixed to the current run; `RunConfig.history_read: bool = false`
controls rendering metadata. CLI/Harbor `history_read` defaults off. The tool is
`history_read({item_id: string, offset?: integer, limit?: integer})`.

- [ ] Bind the resolved current WAL path at registry construction and read it
  only during the primary run, after existing WAL rotation has completed;
  refuse `--history-read on` with `--no-log`. Bind it to the primary run only;
  do not accidentally expose previous archived WALs or the review phase log.
- [ ] Treat `Item.id` as WAL-local: fresh runs reuse IDs such as `item-5`.
  The fixed WAL prevents reading an older run, but an unqualified ID cannot
  distinguish colliding names across runs. Advertise current `Item.id` values
  in collapsed tool-result stubs only
  when enabled, preserving their existing output archive handles. Never rewrite
  old durable rows or assistant/tool-call associations.
- [ ] Resolve only `ItemKind::ToolResult` in the fixed current WAL. The model
  supplies no path. Return a historical marker, original item/call IDs, sequence,
  observation time, original error status, and a bounded page of stored text.
  A successful lookup of an originally failed command is a successful lookup,
  not a newly failed command to replay or roll back.
- [ ] Use 8,000 characters by default, at most 32,768 per page. Bound source log
  size before whole-log loading (chosen 64 MiB initial ceiling); return a clear
  limit result rather than unbounded memory use. Respect cancellation. These
  chosen limits are validated by focused boundary tests, not paper constants.
- [ ] Test mutated-file history versus current reads, unknown IDs, non-tool IDs,
  prior-WAL isolation despite colliding local IDs, paging, malformed/torn logs,
  disabled defaults, and append-only
  retrieval through the actual loop. No original command is executed again.
- [ ] Compare rendering hysteresis independently of restoration. A histogram
  of cached-input receipts can diagnose it, but hosted KV-memory/throughput wins
  from ReFold are not portable claims.

**Exit:** the demonstrated information-loss case is recoverable, with historical
meaning explicit. Runtime resume, replaying effects, and cross-run memory remain
out of scope.

## Phase 4 — conditional compaction correctness and provenance

**Gate:** an experiment requires compaction because observed prompt pressure
threatens useful continuation. Default-off compaction is not a prerequisite for
the Phase 2 experiments.

### 4A. Fix capacity units before any compaction efficacy run

**Owners:** Luna context first, Luna loop next, root CLI/Harbor integration last.
**Files:** `crates/context/src/compact.rs`, context tests;
`crates/agent-loop/src/request.rs`, loop tests; root CLI/run config and adapter.

Add `context_capacity_tokens: Option<u64>` and `output_reserve_tokens: u64` to
`CompactionConfig`, defaulting to unknown/zero with compaction disabled. Preserve
`compaction_due(estimate: u64, capacity: u64, cfg: &CompactionConfig) -> bool`;
its caller supplies usable input capacity, never the cumulative run allowance.
CLI names: `--context-capacity N`, `--output-reserve N`; adapter equivalents use
the same names with underscores.

- [ ] Require known positive capacity and a reserve below capacity when
  compaction is requested. Unknown model capacity must not be guessed from its
  name. Explicit old compaction calls need these new values; document this
  intentional validation change to the experimental path.
- [ ] Trigger at `estimate > frac * (capacity - reserve)`. Validate the reserve
  against the largest output allocation permitted by the active provider retry
  profile, including a starting cap above its escalation ceiling. Keep spend
  and token accounting in the unchanged run-wide BudgetGuard.
  For the current OpenAI-compatible client that maximum is
  `max(initial_max_tokens, 32768)`. Root supplies this knowledge at CLI/provider
  wiring; the generic loop cannot inspect a nonexistent provider-profile API.
- [ ] Test fixed capacity/varying total allowance, fixed allowance/varying
  capacity, insufficient generation headroom, invalid values, disabled-path
  equivalence, and metered summary usage. Expose the compaction fraction through
  Harbor only with the validated capacity/reserve controls.

**Exit:** deterministic unit/loop checks establish the units and bounds; this
does not establish any summarization capability benefit.

### 4B. Distinguish observed facts, hypotheses and plans

**Owners:** Luna context for additive types/prompt, then the same Luna loop owner
for parsing/fold behavior; root wires an independent opt-in switch afterward.
**Files:** `crates/context/src/compact.rs`, public exports if needed;
`crates/agent-loop/src/{request.rs,state.rs}` and their tests.

Proposed types live in `context`, not a new memory crate:

```rust
pub enum CheckpointKind { Fact, Hypothesis, Plan }
pub struct CheckpointEntry {
    pub kind: CheckpointKind,
    pub text: String,
    pub source_item_ids: Vec<String>,
}
```

Also add `source_item_id: Option<String>` to `CompactMessage`; default/unattributed
messages use `None`. The context owner freezes that field before loop consumers
start. The loop must construct IDs alongside its ordered source items before
`compact_view` loses them; an injected earlier checkpoint has no invented item ID.

- [ ] Add `checkpoint_sources: bool = false` to `CompactionConfig`, CLI
  `--checkpoint-sources on|off`, and the Harbor boolean. Keep the original
  summary format as the control after 4A so provenance is an isolated treatment.
- [ ] Include that explicit ID/content mapping in the summarizer view; request
  these entries in plain
  generated JSON. Resolve fact references to eligible original user/tool records.
  Unknown/unattributed claims remain labelled hypotheses rather than established
  facts. Preserve useful plans under their own label; do not delete every claim
  that lacks a quotation or accept an ID as proof its interpretation is correct.
- [ ] A malformed response leaves history intact and records a refused
  checkpoint. Never let parse failure erase the only recoverable history.
- [ ] Test exact IDs, unknown IDs, conflicting hypotheses, user requirements,
  malformed output, protected recent tool groups, and unchanged default format.
  Charge the existing summary call; add no source-judging model call.

**Exit:** provenance references resolve and categories remain explicit. SJC-style
judge efficacy and automatic memory learning remain deferred.

## Phase 5 — controlled external development experiments

**Owner:** root as sole campaign/spend writer. Luna workers can audit actor traces,
runtime/config hashes and results independently, without reading hidden feedback
into the solving agent or sharing the live credential.

- [ ] Reconcile the stopped accounting state before any paid request; verify
  remaining headroom under the preserved cap/reservations. Recheck official
  release/task relevance and freeze all exclusions before outcomes are viewed.
- [ ] Obtain complete pinned tasks, check actual disk/build footprint, run native
  oracle readiness checks and an appropriate known-bad artifact control, and bind
  task/image/runtime/runner hashes. The prior disk snapshot is historical, not
  a current capacity guarantee. Prune no unrelated owner images automatically.
- [ ] Freeze development/holdout assignments, comparison rule, model endpoint,
  generation limits and budgets. Treat the proposed four-task slice as a small
  convenience pilot. Increase task coverage before a suite-level conclusion.
- [ ] Test one change at a time. Start with the existing exposed lookup versus
  no lookup; then evidence mode versus the same lookup control. Test minimal
  workflow, preserved checks, retention and response caps independently rather
  than attributing a combined profile to one mechanism.
- [ ] Compare targeted completion feedback with an equal-budget repair/resampling
  opportunity when claiming benefit from the predicate rather than extra calls.
  Review/reflection/candidate generation also require resample-only controls.
- [ ] Run the Phase 1 helper, preserve rescue and harm, and retain cap-limited
  failures and incomplete rows visibly. A candidate may be retained for research
  despite temporary token overhead; neither retention nor a small pilot changes
  production defaults. Spend constraints can end the experiment without proving
  or disproving the hypothesis.

**Exit:** reviewable paired per-task evidence, fully accounted attempts, and an
explicit verdict of promising, unsupported, harmful, or incomplete. Do not force
a significance claim or claim published unmatched external rows are controls.

## Phase 6 — held-out confirmation, admission, and maintenance

- [ ] Freeze the surviving profile and evaluate it on prospectively untouched
  tasks without using holdout outcomes to revise it. Report task coverage,
  attempt counts, release, limits and uncertainty; do not call a selected slice
  a full CPU-suite score or frontier-level performance.
- [ ] Promote only with repeatable held-out capability evidence and a defined
  tokens-per-solved ratio within 1.3x. When a control solves zero, record the
  undefined ratio and keep the candidate experimental pending a usable reference.
- [ ] Keep published external agent/model/protocol comparisons labelled as
  unmatched context. Frontier claims require a relevant benchmark/coverage
  protocol; no paid comparison against an owner's personal agent setup.
- [ ] Update the handoff, deferred register, architecture, experiment controls,
  README and this checklist in the same change. Each deferred idea retains its
  reason and revisit evidence; measured failure is not silently relabelled unbuilt.
- [ ] Commit only verified owned changes, with user-only attribution. Push only
  when authorized and permitted; verify CI for that exact commit after a push.
  A local check is never a claim that remote CI ran.

## Per-crate finish gate and root integration checks

For each changed crate `X`, run `cargo test --locked -p X`,
`cargo clippy --locked -p X --all-targets -- -D warnings`, and
`cargo fmt -p X -- --check`; check changed code for unfinished placeholders.
Root runs affected cross-crate tests after integrating frozen APIs. Run focused
Python unittest discovery, adapter usage-context self-check, Ruff when available,
and the repository's tracked-reference and diff checks. Do not repeatedly run
unrelated suites after the relevant checks pass without a new concern.

Every phase ends with a fresh Luna diff review against its contract. Root checks
the findings and actual command output. A phase cannot become complete merely
because its implementation exists, and an experiment cannot become validated
merely because its local tests pass.

## Deferred beyond this plan

Automatic harness search, learned restart/allocation predictors, semantic graph
infrastructure, training a memory network, server KV-cache control, cross-run
learning, crash resume, and runtime MAS stay in the existing
[deferred register](../../research-next-steps.md#deferred-work-register).
The independent resampling control and paired report precede any reconsideration
of review/reflection/racing. The proposed compaction and historical tools do not
authorize those larger systems.
