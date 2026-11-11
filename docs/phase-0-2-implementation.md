# Capability phases 0–2 (2026-11-11)

Implementation follows the [phase plan](superpowers/plans/2026-11-11-capability-phases.md).
All new runtime mechanisms remain opt-in. No external benchmark outcomes were
produced by this work; the stopped paid campaign and reservations are unchanged.

## Reference and ownership

The [reference manifest](experiments/2026-11-11-phases-0-2-reference.json) pins
clean source `411846d`, its separately exported/rebuilt release runtime,
loader/libraries, adapter, installed Harbor and ownership overlays, task proposal
hash and complete nonsecret chosen recipe. The local reference runtime lives at
`target/phase0-reference-runtime`; its SHA-256 is
`dc170bc3a99f28699206163533b18ab5a5d574f64fa737e3227a793a09a749f2`.
The bundled loader's usage check returned exit 2 without inference. That runtime
has no benchmark outcomes. Historical pilot rows retain their original binary.

Three GPT-6 Luna workers exclusively owned tools, loop and offline analysis;
the orchestrator owned CLI, Harbor and documentation integration. Fresh review
is separate from implementation. After the Luna usage limit interrupted the
last worker/reviewer turns, root took sole ownership of the remaining integration
fixes in `agent-loop`, `tools-std`, `rof` and `agent-budget`; no concurrent writers
remained. No conditional phase 3/4 stubs were added.

## Trace audit and tool scope

The [actor-only audit](experiments/2026-11-11-tool-trace-audit.json) records ten
historical WAL files (nine with tool calls), 847 calls/results, 39 tool errors,
and zero lookup calls. Eleven argv-shape rejections across nine traces involved
six `&&`, three pipe and two redirection attempts. Existing error guidance
already explains argv/no-shell behavior; no remaining execution-contract defect
was reproduced, so no speculative execution API change was added. The 71 literal
truncation markers do not establish an unrecoverable-output count. Two logs are
from earlier Raman development runs; these counts are not a new benchmark score.

Evidence mode is a chosen research treatment, rather than a fix inferred from
lookup failures. Oversized directories are bounded before sorting; an over-cap
directory is omitted with a scan-limit notice, preserving deterministic retained
results. Legacy lookup retains streaming enumeration and its existing cap.

## Paired reporting input

Run `python3 tools/rof_compare.py --input EXPERIMENT.json --output REPORT.json`.
The JSON has exactly three top-level keys:

| Key | Required content |
| --- | --- |
| `protocol` | `dataset`, `release_hash`, `task_hashes`, `model`, `endpoint`, `runner`, `grade_path`, `budgets`, `splits`, `allowed_difference`, `ledger`, `pairs` |
| `arms` | `control` and `treatment`, each with `runtime_sha256` and `config` |
| `rows` | One normalized agent-trial record per arm/task/replicate; missing records remain visible |

Protocol release and task hashes are lowercase SHA-256 digests; preserve the
release tag/source commit alongside them in the campaign provenance.
`task_hashes` maps task ID to digest. `pairs` lists objects with `task_id` and
`replicate_id`, including planned pairs absent from both arms. `ledger` contains
`path` and `sha256`; it references existing financial evidence without computing
an invoice. `allowed_difference` contains the changed config `key` and treatment
`value`. Arm configs must differ only in that field; record full configs, including
defaults. Runtime hashes are recorded per arm. Match model, endpoint, runner,
budgets and graders; sharing this protocol is a caller attestation, not an
independent audit of the underlying artifacts.

Each row contains exactly:

- `arm`, `task_id`, `attempt_id`, `replicate_id` — nonempty IDs; arm is control or
  treatment. An attempt is an agent trial, not a provider retry.
- `artifact_reward` — finite numeric reward or null. Reward at least 1 is counted
  as a solved artifact; fractions remain available as rewards.
- `run_status`, `run_completed`, `accounting_complete` — an explicit status and
  boolean completion/accounting facts. Reaching a configured budget terminal is
  a completed protocol attempt, even when its native reward is zero.
- `usage` — null, or exactly `input_tokens`, `cached_input_tokens`, `output_tokens`;
  nonnegative integers, cached input no greater than total input. Complete
  accounting requires known usage. Include all retries in the trial totals.
- `artifact_path`, `artifact_sha256` — source path and lowercase SHA-256 digest.
  The helper validates format; it does not open or independently rehash artifacts.

Raw artifact transitions include interrupted rewarded artifacts. Completed,
accounted transitions require both arms' terminal/accounting facts and known
native rewards. Missing/incomplete pairs remain listed; they cannot become
rescues. Configured cap failures remain in the denominator. Task-level means
average replicates first, then give tasks equal weight; conservative reward
summaries explicitly retain incomplete expected cells at zero.

Usage totals charge every supplied trial, including failed/interrupted trials.
Cached input is a subset of input, not added again. Known subtotals remain
visible, but totals and token ratios are null when expected usage is missing.
Tokens per solved task use task-mean gross costs divided by task-mean completed,
accounted solve fractions; zero solved tasks or an unknown expected native reward
gives null. Research retention and
promotion both default false: this helper does not choose a candidate or promote
runtime defaults. It cannot clear the campaign's unknown usage stop.

## Available runtime controls

The native Harbor adapter adds `repo_lookup`, `collapse_hysteresis`,
`repo_evidence` and `completion_check`. Absent/false values preserve the previous
argv. Evidence requires lookup; hysteresis must be positive if supplied.

Evidence mode groups literal matches using explicit test/configuration and
language-declaration heuristics. One shared result limit and an 8,000-character
ceiling bound the result. Shortened snippets, omitted hits and scan limits are
reported. No semantic call graph or complete dependency discovery is claimed.

The completion predicate runs the exact operator-supplied public command through
`test`, with existing policy, execution wrapper, logging, cancellation, rollback
and budgets. It runs at most twice and offers one repair continuation. A passing
predicate permits read-only completion. Earlier unrelated tests cannot satisfy
it. Unmet/unknown predicates produce `FailureKind::Completion` (exit 8), a task
failure; actual budget/cancellation outcomes retain their normal classification.
The harness-origin declaration/result pair also forms valid provider history,
with no fabricated provider usage. Generic verification nudges are disabled for
this path. `--review on` and the
new completion predicate cannot be combined until their shared intervention
contract is specified. `BudgetGuard::exceeded_except_steps` reuses the existing non-step cap checks, so
final-step grace cannot hide simultaneous token/action/wall/spend limits. The
shared tool settlement charges each reserved action once and preserves the
same-action tripwire. Per-hunk proof commands are distinct. Hidden native
grader feedback cannot supply the predicate or guide repair.

## Validation and remaining gates

Verified on 2026-11-11: 323 Rust tests across the four affected crates, 14
comparison tests and 7 Harbor adapter tests passed. Scoped all-target Clippy,
Rust fmt and Python Ruff checks passed. Six captured Harbor argv recipes parsed
through the built CLI and stopped at deliberately missing credentials (exit 4).
The adapter self-check also passed. Local logs: `target/phase012-tests.log`,
`target/phase012-clippy.log`, `target/phase012-argv-check.json`.

Two fresh Luna reviewers identified the reporting null-denominator and directory
collection bugs before their usage limits interrupted follow-up review. Root
fixed them and completed source review and checks. Additional regressions cover
provider-valid harness call/result pairing, simultaneous caps, action charging,
call-ID collisions, multi-step repair, cancellation and rollback/preservation.
No claim is made of a final independent review after the usage limit. Tests use
scripted providers/public fixture commands; no paid DeepSeek inference ran.
Remote CI has not run for these local changes.

Before external experiments: reconcile the existing unknown request without
resetting its reservation; verify the then-current official benchmark release;
obtain full pinned task packages; validate images/resources and native oracles;
freeze the new candidate runtime, paired recipes and analysis. The existing
four-task proposal is metadata-only and not a representative suite. Neither
new feature has demonstrated a benchmark gain or earned default admission.
