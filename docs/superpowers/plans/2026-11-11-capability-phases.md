# ROF capability improvements: phases 0–4 implemented default-off, phase 5 stopped incomplete, phase 6 evidence-gated

**Status:** phases 0–4 implemented and locally verified from clean reference `411846d`
([phases 0–2 record](../../phase-0-2-implementation.md),
[phases 3–5 record](../../phase-3-5-implementation.md)); phase 5 ran one interrupted control and stopped;
phase 6 remains evidence-gated. The output-fidelity foundation is complete. The owner authorized phases
0–2, then phases 3–5 on 2026-11-11; paid development runs require the bounded continuation and preflight
in those records. Reference provenance:
[manifest](../../experiments/2026-11-11-phases-0-2-reference.json).

**Goal:** improve externally verified coding-task completion through better repository evidence,
trace-grounded tools, and targeted execution feedback. Extend the existing sequential loop, optional
tools, native Harbor adapter and offline analysis; keep the durable log as the source of historical
observations; separate deployment admission from development-candidate retention; implement no automatic
optimizer, planner, or runtime MAS. Stack unchanged: existing Rust crates, Python stdlib,
installed Harbor/Pydantic, existing Docker runner.

## Global constraints

- Unmeasured mechanisms remain opt-in; absent-flag behavior and the six-tool default registry (view,
  search, edit, write, exec, test) are preserved, so no fix silently promotes a research profile.
- Public traits and enum variants stay compatible. Freeze additive interfaces before consumers start.
- One exclusive writer per crate directory; root owns `crates/rof/`, docs and README.
- No new crates, graph service, embeddings, learned judge, or third-party package. One existing internal
  dependency (`agent-log`) serves historical log reading.
- Benchmark instructions and official graders stay unchanged. Native hidden verifier results cannot
  drive repair, retrieval, procedures, or selection.
- Capability is primary: the 1.3x tokens-per-solved guardrail governs default admission, not whether a
  promising development candidate can be retained. Authorized USD caps and outstanding reservations stay
  hard limits.
- Verify the then-current official benchmark release before any future spend. TB4.1.0 CPU is the last
  verified pin, not an assertion about the latest release on execution day.
- Preserve historical results and frozen pilot binaries; build and hash a new runtime for each new code
  comparison.

Accounting stays stopped and unchanged: the original upper bound `$2.158150338` includes the
`$0.324172800` unknown reservation; the authorized continuation subledger is capped at `$0.661849662`;
aggregate reservations/spend cannot exceed `$2.82`. Never reset the old ledger or claim the unknown
request reconciled; a new unknown stops the new campaign. Ledger paths and rules:
[phases 3–5 record](../../phase-3-5-implementation.md#bounded-phase-5-authorization).

Experiment order was **0 → 1 → 2 → 5 → 6**; by later owner request phases 3–4 shipped default-off before
phase 5. Their observed-need gates still govern efficacy experiments and default admission, and neither
mechanism enters the initial lookup comparison. Implementation checks are free of
model calls; phase 5 is spend-gated.

## Team and parallel-work contracts

Three GPT-6 Luna workers owned tools, loop and offline analysis; root owned CLI, Harbor and documentation
integration: [split and limits](../../phase-0-2-implementation.md#reference-and-ownership). Every worker task
states starting commit, allowed paths, input/output contract, default behavior, failing case, commands and stop
conditions, and requires an observed failing regression before a nontrivial change. Root reviews source
and runs integration checks before committing; workers touch only their files, never push.

## Phases 0–2 — done (2026-11-11)

Phase 0 froze clean reference `411846d` with runtime SHA-256, runner/overlay hashes and the complete
nonsecret recipe in the [manifest](../../experiments/2026-11-11-phases-0-2-reference.json), and froze the
per-phase contracts. `docs/next-benchmark-slice.json` stays a metadata-only four-task proposal: not a
representative suite and not a certified historically unseen holdout.

Phase 1 added `tools/rof_compare.py` (frozen `compare(experiment: dict) -> dict`,
`main(argv=None) -> int`) and Harbor forwarding for `repo_lookup` / `collapse_hysteresis`. Failing cases
first covered the 00/01/10/11 outcome matrix, duplicate attempts and unknown usage; the JSON protocol,
per-attempt usage semantics, equal weighting and retention/promotion split are in the
[paired-reporting section](../../phase-0-2-implementation.md#paired-reporting-input). Run
`python3 -m unittest discover -s tools -p 'test_rof_compare.py' -v` and
`python3 tools/rof_compare.py --input EXPERIMENT.json --output REPORT.json`; the Harbor venv
`/home/madiyar/.local/share/uv/tools/harbor/bin/python -m unittest discover -s tools -p 'test_harbor_rof.py' -v`.
No parity with the legacy `rof eval --agent` CLI: native experiments use the installed adapter, which
invokes `rof run`.

Phase 2 added evidence-aware lookup and the bounded completion predicate; contracts still governing the
code, not in the record:

- `repo_evidence: bool = false`, CLI `--repo-evidence on|off`, requires `repo_lookup`. Activation is an
  explicit run configuration, not an argument the control can select, so the ablation stays clean.
  Evidence mode groups bounded literal, definition, test-path and config-path candidates: explicit
  heuristics, not a semantic caller graph. Output is bounded to `common::OUT_CAP` (8,000 chars) with an
  omission/scan-limit notice and `ToolOutcome.truncated`; legacy output and the loop's
  `tool_core::MAX_MODEL_CHARS` (40,000) bound are unchanged. Current lookup has no tool-local character
  cap, so this is an evidence-mode contract, not a claim about its old behavior. No second shell tool, no
  automatic LLM tool optimizer.
- `RunConfig.completion_check: Option<String>` (default `None`), CLI `--completion-check CMD`, Harbor
  `completion_check: str | None`. Zero exit satisfies this operator-supplied predicate, not a proof of
  arbitrary task requirements. `FailureKind::Completion` was added without renaming existing variants and
  maps to exit 8. At most one repair continuation and two predicate executions, sharing the original
  usage totals, caps, deadline and cancellation, with no allowlist grants; a repair continuation may span
  multiple model/tool steps. The last-step Done grace cannot buy an extra provider request. `--review on`
  cannot be combined with it until a shared intervention contract exists.
- Rivet and AT-AT motivate this bet, not an expected ROF effect size. Measure false rejection on
  reference successes before promotion; no universal completion rule is inferred from JevAct.

Validation ([record](../../phase-0-2-implementation.md#validation-and-remaining-gates)): 323 scoped Rust
tests (budget 7, loop 142, CLI 84 + 1 build-provenance, standard tools 66 + experimental 23), 21 Python
tests (14 comparison, 7 Harbor adapter), all-target Clippy, fmt, Ruff and six captured Harbor argv
recipes passed. No provider inference ran. Two fresh Luna reviews found the null-reward
denominator and unbounded-directory collection bugs; the fixes and further completion regressions are
tested. Usage limits interrupted the final review turns, so root finished the source review and reran the
gates. No paid benchmark or capability improvement is claimed.

## Phases 3–4 — implemented, default-off, efficacy gates still open

The historical actor sample showed no irrecoverable observation and no context overflow, so these
default-off mechanisms enter no efficacy experiment or default admission until their gates are met;
synthetic mutation/pressure fixtures validate mechanics only. Independent Luna and
`codex review --commit` checks of phases 0–2 found no actionable defect (its own Python run errored; the
prescribed environments passed separately). Validation (352 Rust tests across context,
tools, loop and CLI; 10 adapter tests; Clippy, fmt, Ruff; the actual loop restored an old observation
after its live file was overwritten):
[record](../../phase-3-5-implementation.md#implemented-locally-2026-11-11).

Phase 3 (gate: traces show necessary history unrecoverable by `view` or `output_read`):
`history_read({item_id, offset?, limit?})` behind `--history-read on`, with host-supplied absolute
`ExperimentalToolsConfig.history_log` and `RunConfig.history_read` defaulting false. It reads only
historical ToolResults from the fixed current WAL, append-only: no command replay, no rewriting of old
durable rows. Pages default 8,000 characters and cap at 32,768; the source snapshot is bounded before
whole-log load (chosen 64 MiB, validated by boundary tests, not a paper constant). `Item.id` is WAL-local. `--history-read on` is refused with `--no-log` or
`--review on`.

- [ ] Open: compare rendering hysteresis independently of restoration. A histogram of cached-input
  receipts can diagnose it, but hosted KV-memory/throughput wins from ReFold are not portable claims.

Out of scope: runtime resume, replaying effects, cross-run memory.

Phase 4A (gate: an experiment requires compaction because observed prompt pressure threatens
continuation): `CompactionConfig.context_capacity_tokens: Option<u64>`, `output_reserve_tokens: u64`, and
`compaction_due(estimate, capacity, cfg)` keeping its signature; the caller passes usable input capacity
(`context_capacity_tokens - output_reserve_tokens`), never the cumulative run allowance, and the function
rejects any other value. Trigger: `estimate > frac * usable`. CLI `--context-capacity N`,
`--output-reserve N` (adapter: underscores). Capacity must be positive and above the reserve, never
guessed from a model name; the reserve must cover the provider's largest output allocation,
`max(initial_max_tokens, 32768)`. Old compaction calls need the new values: an intentional validation
change. They establish units and bounds, not a summarization benefit.

Phase 4B: `CheckpointKind {Fact, Hypothesis, Plan}` and `CheckpointEntry {kind, text, source_item_ids}`
live in `context` (no new memory crate); `CompactMessage.source_item_id: Option<String>`;
`--checkpoint-sources on|off` requires `--compaction`. Fact references must resolve to records actually
shown in the summarized prefix; unknown or unattributed claims stay hypotheses, plans keep their label,
and a resolving ID is provenance, not proof the interpretation is true. A malformed response leaves
history intact, records a refused checkpoint and still charges the summary call; no extra source-judging
model call. The original summary format remains the control. SJC-style judge efficacy and automatic
memory learning stay deferred.

## Phase 5 — controlled external development experiments

**Owner:** root as sole campaign/spend writer. Luna workers may audit actor traces, runtime/config hashes
and results independently, without reading hidden feedback into the solving agent or sharing the live
credential.

- [x] Preserve the stopped accounting state and obtain explicit owner revision allowing a new subledger
  within verified remaining cap/reservations. Recheck official release/task relevance and freeze all
  exclusions before outcomes are viewed.
- [x] Obtain complete pinned tasks, check actual disk/build footprint, run native oracle readiness checks
  and an appropriate known-bad artifact control, and bind task/image/runtime/runner hashes. The prior
  disk snapshot is historical, not a current capacity guarantee. Prune no unrelated owner images
  automatically.
- [x] Freeze development/holdout assignments, comparison rule, model endpoint, generation limits and
  budgets. The four-task slice is a small convenience pilot; task coverage must grow before a suite-level
  conclusion.
- [ ] Test one change at a time. Start with the existing exposed lookup versus no lookup; then evidence
  mode versus the same lookup control. Test minimal workflow, preserved checks, retention and response
  caps independently rather than attributing a combined profile to one mechanism.
- [ ] Compare targeted completion feedback with an equal-budget repair/resampling opportunity when
  claiming benefit from the predicate rather than extra calls. Review/reflection/candidate generation
  also require resample-only controls.
- [ ] Run the Phase 1 helper, preserve rescue and harm, and retain cap-limited failures and incomplete
  rows visibly. A candidate may be retained for research despite temporary token overhead; neither
  retention nor a small pilot changes production defaults. Spend constraints can end the experiment
  without proving or disproving the hypothesis.

**Exit:** reviewable paired per-task evidence, fully accounted attempts, and an explicit verdict of
promising, unsupported, harmful, or incomplete. Do not force a significance claim or claim published
unmatched external rows are controls.

**Outcome (2026-11-11): incomplete.** Both development oracle/bad controls passed readiness, but only the
Cargo control ran (artifact reward 0.0). One response body reset after HTTP 200 without usage: 26
requests, 25 receipts. Its reservation remains held and the runner stopped before all other cells.
Combined upper bound `$2.519179098` stays below `$2.82`, with zero usable headroom while stopped. Do not
check off the paired experiment merely because it launched; accounting detail and continuation rules are
in the [record](../../phase-3-5-implementation.md#phase-5-outcome-incomplete-stopped-on-a-new-unknown-request).

## Phase 6 — held-out confirmation, admission, and maintenance

- [ ] Freeze the surviving profile and evaluate it on prospectively untouched tasks without using holdout
  outcomes to revise it. Report task coverage, attempt counts, release, limits and uncertainty; do not
  call a selected slice a full CPU-suite score or frontier-level performance.
- [ ] Promote only with repeatable held-out capability evidence and a defined tokens-per-solved ratio
  within 1.3x. When a control solves zero, record the undefined ratio and keep the candidate experimental
  pending a usable reference.
- [ ] Keep published external agent/model/protocol comparisons labelled as unmatched context. Frontier
  claims require a relevant benchmark/coverage protocol; no paid comparison against an owner's personal
  agent setup.
- [ ] Update the handoff, deferred register, architecture, experiment controls, README and this checklist
  in the same change. Each deferred idea retains its reason and revisit evidence; measured failure is not
  silently relabelled unbuilt.
- [ ] Commit only verified owned changes, with user-only attribution. Push only when authorized and
  permitted; verify CI for that exact commit after a push. A local check is never a claim that remote CI
  ran.

## Per-crate finish gate and root integration checks

For each changed crate `X`: `cargo test --locked -p X`,
`cargo clippy --locked -p X --all-targets -- -D warnings`, `cargo fmt -p X -- --check`, and no unfinished
placeholders. Root then runs affected cross-crate tests, focused Python unittest discovery, the adapter
usage-context self-check, Ruff when available, and the tracked-reference and diff checks; unrelated
suites are not re-run after the relevant checks pass without a new concern. Every phase ends with a fresh
Luna diff review against its contract: a phase is not complete because its implementation exists, and an
experiment is not validated because its local tests pass.

## Deferred beyond this plan

Automatic harness search, learned restart/allocation predictors, semantic graph infrastructure, training a
memory network, server KV-cache control, cross-run learning, crash resume and runtime MAS stay in the
[deferred register](../../research-next-steps.md#deferred-work-register). The independent resampling
control and paired report precede any reconsideration of review/reflection/racing; the compaction and
historical tools do not authorize those larger systems.
