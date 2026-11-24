# Output fidelity and experiment readiness — archive access and Harbor controls implemented, verified locally (2026-11-11)

**Goal:** preserve access to archived observations, expose existing experiment controls through
Harbor, and leave an actionable handoff for future sessions. **Approved design:** the owner
accepted the preceding research assessment and requested Luna implementation. Reuse the
existing archive and CLI mechanisms; preserve current defaults. Capability is the objective;
cost is a guardrail. Research reports remain unchanged.

## Contracts

- `crates/tools-std/` owns the archive handle: archived exec/test return their run-owned handle
  before output so it survives the 120-character observation preview. Public APIs stay
  compatible; PASS/FAIL, bounded archive behaviour and existing policy semantics are preserved.
- `tools/harbor_rof.py` gets opt-in `archive_output` and `shell` booleans, default false,
  forwarding explicit enabled CLI switches only. Workflow, failed-check preservation, retention
  and response-cap controls stay unchanged.
- No new dependencies or runtime subsystems. No worker commits another worker's files.

## Execution (done)

All five checklist items landed: the archive-handle regression, adapter forwarding checks,
next-step/deferred documentation, archived-output recovery through the real transcript fold as
a scripted integration, and diff review with the plan in the implementation commit.

Local verification on 2026-11-11: 166 Rust tests across the two affected packages, four Harbor
adapter tests, scoped all-target Clippy and formatting, Ruff, adapter usage-context self-check,
and four real CLI recipes stopping before inference all passed. Independent diff review found a
stale checklist, now corrected. The proposed four-task metadata record has eight public file
hashes checked against its pinned source manifest. Task packages and native oracles remain
unvalidated; no paid experiment ran and the spending ledger was unchanged.

## Benchmark sequence after accounting is reconciled

Do not restart the stopped campaign or clear unknown usage. Its $2.158150338 upper bound
includes the $0.324172800 unknown-request reservation, within the existing $2.82 ceiling;
subledger arithmetic is in [phase 3–5](../../phase-3-5-implementation.md). Any future paid run must
pass fresh idle-ledger and release/task readiness checks.

1. Freeze fresh development and untouched holdout tasks from the current official CPU release;
   current pinned reference is TB4.1.0. Verify native oracle readiness before inference.
2. Compare a fixed reference recipe with one changed mechanism: retention, archived output
   retrieval, minimal workflow, failed-check preservation, shell, or response cap. Do not
   combine knobs in an attribution experiment.
3. Charge all attempts/retries; preserve artifact reward, run completion and usage completeness
   separately. A zero-solve control has no defined tokens-per-solved ratio.
4. Test the surviving profile on untouched holdouts. Extra review/reflection/candidates require
   a token-matched independent-resampling control. No hidden grader feedback may guide runtime
   decisions or candidate selection.

No experiments in this plan establish a suite score until actually executed under a documented
coverage protocol.

> Now: the owner authorized a bounded continuation subledger of $0.661849662 on 2026-11-11
> ([phase 3–5](../../phase-3-5-implementation.md)), whose second campaign also stopped with an aggregate upper bound of
> $2.519179098.
