# Repository reliability hardening implementation plan

**Goal:** Enforce filesystem containment, bound execution, preserve all paid usage, and make evaluation evidence reproducible and honest.

**Approved design:** The user approved the review and the coordinator/three-agent, dependency-ordered workflow. Independent fixes use isolated worktrees; the coordinator owns shared execution and engine interfaces. Keep model/persistence JSON compatible while adding typed internal boundaries. No new workflow framework.

**Stack:** Rust, Tokio, serde, reqwest; existing offline integration tests.

## Constraints

- Preserve existing defaults except explicit reliability fixes and finite deadlines.
- No external provider requests or benchmark claims; exercise local fixtures only.
- Reproduce each bug before implementation; reviewer must not be its author.
- Maintain old report/config deserialization via serde defaults.
- Worktrees: `/tmp/rof-containment`, `/tmp/rof-execution`, `/tmp/rof-evaluation`.

## Tasks and ownership

- [x] Containment agent: `tests/security_containment.rs`, `src/tools/path.rs`, containment portions of `src/tools/mod.rs`, `src/context/retriever.rs`. Regressions must deny absolute external reads, external file symlinks, and aliases into `.git`; allow ordinary files/new writes. One canonical resolver validates lexical containment, resolved parent/target containment, and protected resolved components.
- [x] Execution agent: `tests/retry_accounting.rs`, `src/llm/mod.rs`, `src/llm/openrouter.rs`. Local responses first truncate with usage, then succeed; aggregate both usages. Exhaustion preserves known usage in a typed error. Keep fallback usage and trace/budget consumers accurate.
- [x] Evaluation agent: `tests/eval_integrity.rs`, `src/eval/runner.rs`, evaluation metrics/comparison if needed. Missing task source with `expect_pass=false` must be inconclusive and unmatched. Preserve task identity on join failure; keep report reads compatible.
- [x] Coordinator: shared async subprocess runner used by `proc.run` and implementer checks. Tests prove timeout/cancellation cleanup, bounded output, heartbeat responsiveness, and exact command policy. Commands run without a shell; Unix process groups prevent surviving descendants.
- [x] Coordinator: request/connect/task deadlines. Local stalled server and slow goal regressions; timeout becomes explicit incomplete outcome and preserves recorded evidence.
- [x] Configuration agent (after containment integration): snapshot thinking, per-role caps, re-ask budget, compaction, reasoning effort and deadlines into effective config; runtime consumers stop re-reading behavior environment. Verify config round trip and label differences.
- [x] Coordinator: typed run/task outcomes at engine/evaluator boundary; legacy JSON entrypoints wrap typed results. Extract shared baseline/check/diff/rollback transitions without changing planned/direct policy. Existing mode tests preserve behavior.
- [x] Evaluation agent (after integrity integration): persistent trace health, report `complete` and recording errors; failed sink remains observable. `/dev/full` fixture demonstrates failed recording and incomplete report.
- [x] Configuration/evaluation agent: report provenance includes target dirty state and harness revision; identify actual services/provider settings without secrets.
- [x] Coordinator: CI runs locked tests, formatting, Clippy. README retains setup, current defaults/security limits and one benchmark summary; move historical experiments to a versioned document and retain attribution.
- [x] Independent agents review others' diffs and failure boundaries. Coordinator resolves findings, runs complete tests, Clippy with warnings denied, formatting, and checks final diff/status.

## Verification commands

Run each agent's regression target red, then green. Use `cargo test --locked --offline --test <target>`. Final integration: `cargo test --locked --offline`, `cargo clippy --locked --offline --all-targets -- -D warnings`, `cargo fmt --check`. Record exact evidence in the completion note. No live benchmark required.

## Execution evidence

- Each independent fix was reproduced red before production changes. Agent worktrees retained separate commits for containment, usage accounting, evaluation integrity, trace health, provenance and effective configuration.
- Independent reviews found additional automatic-context aliases, cancellation accounting, baseline deadline coverage, and two safe-path compatibility regressions; all were fixed with regressions.
- Full integrated offline suite: 624 passed, 0 failed, 0 ignored. Formatting passes. Clippy passes with warnings denied.
- API compatibility: existing JSON entrypoints remain wrappers around typed outcomes; old reports and partial configs still load. Artifacts and extensible reporting details remain JSON.
- Task deadlines interrupt asynchronous waits, including acceptance baselines; synchronous Git/filesystem work may delay cancellation. Unix process groups cover ordinary descendants; new sessions and cross-platform job isolation require OS controls.
- Known usage from completed HTTP attempts survives cancellation. In-flight provider work without a returned usage record remains unknown; no inferred billing is fabricated.
- No live benchmark or provider inference was run. Historical benchmark claims were moved without re-scoring.
