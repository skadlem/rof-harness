# Capability phases 0–2 (2026-11-11)

Implementation follows the [phase plan](superpowers/plans/2026-11-11-capability-phases.md).
All new runtime mechanisms are opt-in. No external benchmark outcomes were
produced; the stopped paid campaign and reservations are unchanged.

## Reference and ownership

The [reference manifest](experiments/2026-11-11-phases-0-2-reference.json) pins
clean source `411846d`, its exported/rebuilt release runtime, loader/libraries,
adapter, installed Harbor and ownership overlays, task proposal hash, and
complete nonsecret chosen recipe. Local reference runtime
`target/phase0-reference-runtime`: SHA-256
`dc170bc3a99f28699206163533b18ab5a5d574f64fa737e3227a793a09a749f2`, bundled
loader usage check exit 2 without inference, no benchmark outcomes. Historical
pilot rows keep their original binary.

Ownership: three GPT-6 Luna workers exclusively owned tools, loop and offline
analysis; the orchestrator owned CLI, Harbor and documentation integration;
fresh review is separate from implementation. After the Luna usage limit
interrupted the last worker/reviewer turns, root took sole ownership of the
remaining integration fixes in `agent-loop`, `tools-std`, `rof` and
`agent-budget`, with no concurrent writers left. No conditional phase 3/4 stubs
were added.

## Trace audit and tool scope

The [actor-only audit](experiments/2026-11-11-tool-trace-audit.json) records 10
historical WAL files (9 with tool calls), 847 calls/results, 39 tool errors, zero
lookup calls, and 11 argv-shape rejections across 9 traces (6 `&&`, 3 pipe, 2
redirection). Existing error guidance already explains argv/no-shell behavior
and no remaining execution-contract defect was reproduced, so no speculative
execution API change was added. The 71 literal truncation markers do not
establish an unrecoverable-output count. Two logs are earlier Raman development
runs; these counts are not a new benchmark score.

Evidence mode is a chosen research treatment, not a fix inferred from lookup
failures. Oversized directories are bounded before sorting; an over-cap
directory is omitted with a scan-limit notice, keeping retained results
deterministic. Legacy lookup keeps streaming enumeration and its existing cap.

## Paired reporting input

`python3 tools/rof_compare.py --input EXPERIMENT.json --output REPORT.json`. The
JSON has exactly three top-level keys:

| Key | Required content |
| --- | --- |
| `protocol` | `dataset`, `release_hash`, `task_hashes`, `model`, `endpoint`, `runner`, `grade_path`, `budgets`, `splits`, `allowed_difference`, `ledger`, `pairs` |
| `arms` | `control` and `treatment`, each with `runtime_sha256` and `config` |
| `rows` | One normalized agent-trial record per arm/task/replicate; missing records remain visible |

Release and task hashes are lowercase SHA-256 digests; keep the release
tag/source commit alongside them in campaign provenance. `task_hashes` maps task
ID to digest. `pairs` lists `task_id`/`replicate_id` objects, including planned
pairs absent from both arms. `ledger` has `path` and `sha256` and references
existing financial evidence without computing an invoice. `allowed_difference`
has the changed config `key` and treatment `value`; arm configs differ only
there and are recorded in full, defaults included. Runtime hashes are per arm.
Match model, endpoint, runner, budgets and graders; sharing this protocol is a
caller attestation, not an independent audit of the underlying artifacts.

Each row contains exactly:

| Field | Content |
| --- | --- |
| `arm`, `task_id`, `attempt_id`, `replicate_id` | Nonempty IDs; arm is control or treatment. An attempt is an agent trial, not a provider retry. |
| `artifact_reward` | Finite numeric reward or null. At least 1 counts as a solved artifact; fractions remain available as rewards. |
| `run_status`, `run_completed`, `accounting_complete` | Explicit status plus boolean completion/accounting facts. Reaching a configured budget terminal is a completed protocol attempt, even with a zero native reward. |
| `usage` | Null, or exactly `input_tokens`, `cached_input_tokens`, `output_tokens`: nonnegative integers, cached input no greater than total input. Complete accounting requires known usage; include all retries in trial totals. |
| `artifact_path`, `artifact_sha256` | Source path and lowercase SHA-256 digest. The helper validates format but does not open or independently rehash artifacts. |

Raw artifact transitions include interrupted rewarded artifacts. Completed,
accounted transitions need both arms' terminal/accounting facts and known native
rewards. Missing/incomplete pairs stay listed and cannot become rescues;
configured cap failures stay in the denominator. Task-level means average
replicates first, then weight tasks equally; conservative reward summaries keep
incomplete expected cells at zero.

Usage totals charge every supplied trial, failed/interrupted included; cached
input is a subset of input, not added again. Known subtotals stay visible, but
totals and token ratios are null when expected usage is missing. Tokens per
solved task use task-mean gross costs divided by task-mean completed, accounted
solve fractions; zero solved tasks or unknown expected native reward gives null.
Retention and promotion both default false: the helper chooses no candidate,
promotes no runtime default, and cannot clear the campaign's unknown usage stop.

## Available runtime controls

The native Harbor adapter adds `repo_lookup`, `collapse_hysteresis`,
`repo_evidence` and `completion_check`. Absent/false values preserve the previous
argv. Evidence requires lookup; hysteresis must be positive if supplied.

Evidence mode groups literal matches via explicit test/configuration and
language-declaration heuristics. One shared result limit and an 8,000-character
ceiling bound the output; shortened snippets, omitted hits and scan limits are
reported. No semantic call graph or complete dependency discovery is claimed.

The completion predicate runs the exact operator-supplied public command through
`test` under existing policy, execution wrapper, logging, cancellation, rollback
and budgets: at most twice, with one repair continuation. A passing predicate
permits read-only completion; earlier unrelated tests cannot satisfy it.
Unmet/unknown predicates produce `FailureKind::Completion` (exit 8), a task
failure; budget/cancellation outcomes keep their normal classification. The
harness-origin declaration/result pair also forms valid provider history with no
fabricated provider usage. Generic verification nudges are disabled here.
`--review on` and the completion predicate cannot be combined until their shared
intervention contract is specified. `BudgetGuard::exceeded_except_steps` reuses
the existing non-step cap checks, so final-step grace cannot hide simultaneous
token/action/wall/spend limits. Shared tool settlement charges each reserved
action once and preserves the same-action tripwire. Per-hunk proof commands are
distinct. Hidden native grader feedback cannot supply the predicate or guide
repair.

## Validation and remaining gates

Verified on 2026-11-11: 323 Rust tests across the four affected crates, 14
comparison tests, 7 Harbor adapter tests; scoped all-target Clippy, Rust fmt and
Python Ruff. Six captured Harbor argv recipes parsed through the built CLI and
stopped at deliberately missing credentials (exit 4); the adapter self-check
passed. Logs: `target/phase012-tests.log`, `target/phase012-clippy.log`,
`target/phase012-argv-check.json`.

Two fresh Luna reviewers found the reporting null-denominator and
directory-collection bugs before their usage limits interrupted follow-up review;
root fixed them and completed source review and checks. Additional regressions
cover provider-valid harness call/result pairing, simultaneous caps, action
charging, call-ID collisions, multi-step repair, cancellation and
rollback/preservation. No final independent review after the usage limit is
claimed. Tests use scripted providers/public fixture commands; no paid DeepSeek
inference ran. Remote CI has not run these local changes.

Before external experiments: reconcile the existing unknown request without
resetting its reservation; verify the then-current official benchmark release;
obtain full pinned task packages; validate images/resources and native oracles;
freeze the new candidate runtime, paired recipes and analysis. The four-task
proposal is metadata-only, not a representative suite. Neither feature has shown
a benchmark gain or earned default admission.
