# Output fidelity and experiment readiness implementation plan

**Goal:** preserve access to archived observations, expose existing experiment controls through Harbor, and leave an actionable handoff for future sessions.

**Approved design:** the owner accepted the preceding research assessment and requested Luna implementation. Reuse the existing archive and CLI mechanisms; preserve current defaults. Capability is the objective; cost is a guardrail. Research reports remain unchanged.

## Contracts and ownership

- Tools worker exclusively owns `crates/tools-std/`. Public APIs stay compatible. Archived exec/test return their run-owned handle before output so it survives the existing 120-character observation preview. Preserve PASS/FAIL, bounded archive behavior, and existing policy semantics. Reuse ordinary execution descriptions with an archive/retrieval addition.
- Adapter worker exclusively owns `tools/harbor_rof.py` and its focused Python test. Add opt-in `archive_output` and `shell` booleans, default false, forwarding explicit enabled CLI switches only. Existing workflow, failed-check preservation, retention, and response-cap controls stay unchanged. Tests exercise actual adapter command assembly with a fake environment; no provider calls.
- Documentation worker owns `docs/research-next-steps.md`, `docs/harness-experiments.md`, and the current section of `docs/roadmap.md`. Record implementation status, frozen experiment recipes, spend stop, and a deferred-work register with reasons, evidence, prerequisites and revisit criteria. No unsupported performance claims.
- Integration owner owns README discovery, local session-start pointer, plan status, and any cross-crate regression in `crates/rof/`. No new dependencies or runtime subsystems. No worker commits another worker's files.

## Execution

- [x] Write and observe a failing archive-handle recovery regression; prefix the handle and retain execution contracts. Run scoped tools tests, Clippy with all targets, and formatting.
- [x] Write and observe failing adapter forwarding checks; expose the two booleans. Validate defaults and existing controls without network/model calls.
- [x] Write discoverable next-step/deferred documentation. Record recipe structure and proposed public-metadata task IDs separately from completed benchmark results.
- [x] Exercise archived-output recovery through the real transcript fold in a scripted integration. Run affected tests, all-target Clippy, formatting, Python checks, and the exact tracked-reference CI gate.
- [x] Review the diff, resolve findings, update status, and include this plan with the completed implementation commit. Leave existing untracked owner directories untouched.

Local verification on 2026-11-11: 166 Rust tests across the two affected packages,
four Harbor adapter tests, scoped all-target Clippy and formatting, Ruff, adapter
usage-context self-check, and four real CLI recipes stopping before inference
all passed. Independent diff review found a stale checklist, now corrected.
The proposed four-task metadata record has eight public file hashes checked
against its pinned source manifest. Task packages and native oracles remain
unvalidated; no paid experiment ran and the spending ledger was unchanged.

## Benchmark sequence after accounting is reconciled

Do not restart the stopped campaign or clear unknown usage. Its $2.158150338 upper bound includes the $0.324172800 unknown-request reservation, within the existing $2.82 ceiling. Any future paid run must pass fresh idle-ledger and release/task readiness checks.

1. Freeze fresh development and untouched holdout tasks from the current official CPU release; current pinned reference is TB4.1.0. Verify native oracle readiness before inference.
2. Compare a fixed reference recipe with one changed mechanism: retention, archived output retrieval, minimal workflow, failed-check preservation, shell, or response cap. Do not combine knobs in an attribution experiment.
3. Charge all attempts/retries; preserve artifact reward, run completion, and usage completeness separately. A zero-solve control has no defined tokens-per-solved ratio.
4. Test the surviving profile on untouched holdouts. Extra review/reflection/candidates require a token-matched independent-resampling control. No hidden grader feedback may guide runtime decisions or candidate selection.

No experiments in this plan establish a suite score until actually executed under a documented coverage protocol.
