# Harness capability experiments

Archive handle recovery and Harbor `archive_output` / `shell` forwarding are
implemented and verified locally, including end-to-end recovery through
`output_read`. The retention pilot has one clean WAL pass at observation keep
32 and thinking keep 256; the React retention artifact also received reward 1,
but its provider attempt and accounting were incomplete. These selected-task
results do not establish general capability improvement. Other experimental
tool mechanisms remain unmeasured on benchmark tasks, and defaults remain in
force. See [`research-next-steps.md`](research-next-steps.md) for the spend
stop and prepared recipes, and
[`superpowers/plans/2026-11-11-capability-phases.md`](superpowers/plans/2026-11-11-capability-phases.md)
for the phased evaluation plan.

The goal is competitive coding-agent performance supported by external benchmark
results and same-model comparisons. These switches expose hypotheses for testing;
local regression tests do not establish benchmark improvements. Each switch keeps
the previous behaviour as its default control.

## Default tool contract

After a trace audit, the built-in tool descriptions were clarified: `exec`
parses one allowlisted command into argv without a shell, closes stdin, and does
not support pipes, redirection or command chaining. Quote arguments containing
spaces. `write` and `edit` operate on paths under the tool root. This is a
description-only correction; it does not change tool behavior. `test` documents
the same argv constraints and its PASS/FAIL verdict; archived variants reuse the
ordinary descriptions with archive and retrieval guidance. The frozen pilot
binary remains SHA-256
`c9a25f9041e54540e03feeb1079a63f9ed0a3dd08c88d841fc7116af52ba3aab`, and any
benchmark gain from the wording change is unmeasured.

## Independent controls

The `rof run` command exposes the following experimental controls. The native
Harbor adapter (`tools/harbor_rof.py`) currently forwards
`max_tokens`, `observation_keep`, `thinking_keep`, `preserve_failed_checks`,
`archive_output`, `shell`, `workflow`, `reasoning_effort`, `repo_lookup`,
`repo_evidence`, `collapse_hysteresis`, and `completion_check`. The legacy
`rof eval --agent` path is separate and does not currently pass
`thinking_keep` into its run configuration. The remaining listed features are
not Harbor adapter options; do not treat their presence in the ROF CLI as
availability in the current Harbor pilot. The table lists run flags; the
paragraphs below identify the separate adapter and legacy-eval limitations.

| Flag | Treatment | Default |
| --- | --- | --- |
| `--preserve-failed-checks on` | Keep edits after an executed verification check fails; the check remains failed | off |
| `--workflow minimal` | Allow exploration before editing; retain normal finish instructions | legacy |
| `--observation-keep N` | Retain N recent tool observations verbatim | 5 |
| `--thinking-keep N` | Retain reasoning text for N assistant messages | 2 |
| `--collapse-hysteresis N` | Batch advances of the observation-collapse boundary | 0 |
| `--stall-threshold N` | Notice repeated identical failed checks with unchanged source evidence | 0 (off) |
| `--archive-output on` | Archive command output and expose paginated retrieval through `output_read` | off |
| `--shell on` | Add shell composition through the execution wrapper | off |
| `--processes on` | Add persistent pipe sessions with start/poll/input/stop | off |
| `--repo-lookup on` | Add bounded literal occurrences and heuristic test-file classification | off |
| `--repo-evidence on` | Group literal, definition, test-path and configuration candidates; requires lookup | off |
| `--completion-check CMD` | Run the exact public predicate on Done, with at most one repair continuation and two checks | none |
| `--multi-edit on` | Validate a set of file edits before applying them | off |
| `--review on` | Run one fresh-context review and, for a concrete finding, one repair continuation | off |
| `--procedure-file PATH` | Load an applicable procedure with a recorded content digest (repeatable) | none |

`--compaction FRAC` also exists on `rof run`, default off. Its current trigger
compares prompt size against a fraction of the cumulative token allowance;
capacity/headroom correction is planned before any compaction efficacy run.
The legacy eval CLI does not expose thinking retention, collapse hysteresis,
or compaction. Evidence mode and completion checks are also run/native-Harbor
controls only. Historical restoration, capacity controls and source-backed
checkpoints remain planned, not shipped.

Pipe sessions, multi-edit, review and procedure loading remain outside this
Harbor adapter. New controls remain off by default and unmeasured on external
tasks; exposing an option does not establish a capability gain.

Shell and process sessions require `--exec-policy any`. They inherit the
configured execution wrapper in container evaluation; host execution grants
host code execution. Pipe sessions do not provide a pseudo-terminal. Repository
lookup is a text index rather than a semantic call graph. Output archives are
outside the deliverable workspace; the output retrieval tool accepts run-owned
handles. Archive limits and omitted bytes must be explicit in returned results.

`--thinking-keep` controls transcript retention; `--keep-thinking` controls
the provider's thinking-suppression setting. `rof run` and the native Harbor
adapter support `thinking_keep`; the legacy `rof eval --agent` path currently
sets it to `None`. The WAL pilot's passing profile used observation keep 32
and thinking keep 256 on one task. Broader replication remains pending in the
[pilot record](current-benchmark-pilot.md).

The Harbor adapter exposes the response-budget control as `kwargs.max_tokens`,
default 8,000. It sets the first request's cap. On truncation, the provider
ladder can raise it through 16,000 and 32,000 to a ceiling of 32,768; the last
rung is capped, not a doubling. All completed retention experiments used an
8,000 starting cap. A larger starting cap is a separate allocation experiment.

Prepared, not run: use `--archive-output on` to retrieve output beyond the
120-character preview through its run-owned handle (archive cap: 64 MiB per
command; omitted bytes are reported); use `--shell on` only with
`--exec-policy any`. Compare either knob alone against the fixed profile. Also
test failed-check preservation, retention, minimal workflow, and starting
response cap one at a time. The exact preflight, holdout and spend rules are in
[`research-next-steps.md`](research-next-steps.md). These recipes do not imply
any benchmark outcome.

Preserving a failing check does not grant proof to its edits. Mutation or policy
errors retain the existing rollback behaviour. Multi-file editing is not an
operating-system transaction and cannot roll back external services or packages.

## Repository evidence and public completion checks

`--repo-lookup on --repo-evidence on` keeps the existing tool arguments (`symbol`,
optional `limit`) and selects enriched output through run configuration. It groups
literal locations, heuristic definition candidates, test paths and configuration
paths under one shared hit limit and an 8,000-character output ceiling. Omitted
results and scan limits are explicit. These candidates are literal matches, not
semantic dependency resolution. Without evidence mode, legacy lookup is unchanged.
Harbor uses `repo_lookup: true` and `repo_evidence: true`; evidence without lookup
is rejected. `collapse_hysteresis` accepts a positive integer; absence retains
ROF's existing default of zero.

`--completion-check "python3 -m unittest"` uses the normal `test` tool, command
policy, execution wrapper, action allowance, deadline and cancellation. It parses
argv without a shell. Supply only a task/operator-owned public check; hidden
native graders must never be used. A passing unrelated earlier test cannot
satisfy this exact predicate. Passing can complete a valid read-only task.
After the first failed predicate the model gets one repair continuation within
the original budgets; its next Done is checked once more. Unmet/unknown checks
are task failures (`FailureKind::Completion`, exit 8), while actual budget or
cancellation outcomes retain their usual meaning. Tool calls/results and the
harness origin are logged without fabricated provider usage. Failed checks obey
`preserve_failed_checks`. Generic verification nudges are disabled for this path.

Harbor's `completion_check` is the exact nonempty command string. `rof run`
rejects combining completion checks with `--review on`: composing their repair
opportunities has not been specified or measured. No universal completion rule
or benchmark improvement is claimed; false rejection must be measured before
promotion. Per-hunk `--proof-cmd` admission remains a separate mechanism.

## Offline paired reporting

`python3 tools/rof_compare.py --input EXPERIMENT.json --output REPORT.json`
compares explicit normalized evidence without contacting a provider. The input
pins a shared protocol, control/treatment runtime hashes and configs, and every
agent-trial row. Expected task/replicate pairs expose missing attempts in either
arm. The report separates native artifact rewards from completed/accounted
comparisons, shows rescue/harm/unchanged outcomes and incomplete pairs, and
retains failures and interrupted attempts. Gross tokens include cached input
once, plus output; provider retry usage belongs in its agent-trial row.

See the [input/output contract](phase-0-2-implementation.md) for the schema,
weighting and zero/unknown semantics. This is an evidence report, not an invoice,
a budget override or an automatic promotion decision.

## External evaluation

The current implementation path uses the native Harbor adapter and controlled
ROF ablations on a pinned release. Published external agent results provide
context; they are not matched controls when model, endpoint or protocol differ.
A future compatible agent comparison must match those variables and the task
slice. Freeze development and held-out tasks before spend. The current four-task
proposal is a convenience pilot, not a representative suite. Publish per-task
outcomes, infrastructure failures, retries, settings, usage, wall time and
uncertainty where estimable. Do not reinterpret named-test counts as task solve
rates or drop budget-limited failures from the denominator.

Run a control and one changed mechanism before combining changes. Review extra
inference and candidate generation must be counted in the system's total budget.
Hidden benchmark tests are only for final grading; they cannot be a runtime
review, procedure-learning or candidate-selection signal.

## Review and procedure retrieval

Review receives the original task and a bounded patch, with file reads/search
available for further inspection. Its context is fresh and its tool access is
read-only. It has at most two logical model requests (provider retries still
apply); a concrete structured finding can
start one repair phase. Every phase shares the original BudgetGuard, deadline
and cancellation token. Review refusal or invalid output is recorded as unknown;
it is not approval of the patch. A suggested reproduction is a hypothesis for
the repair phase to investigate using available checks.

Each procedure file is one JSON object with nonempty `id`, `cue`, `instruction`
and `source_repositories` fields. The last field is an array of repository IDs;
the others are strings. The cue matches the original task case-insensitively.
File content is frozen once, bounded and hashed. Eval provenance records hashes
and selected IDs. Procedure loading does not establish learned transfer.

## Offline procedure distillation

`python3 tools/rof_procedures.py --corpus CORPUS.json` prepares a contrastive
prompt without model calls. The corpus has a `pairs` array; each pair contains
`task_id`, `success` and `failure`. Both attempts name the same `repo_id` and a
`path` to an agent-visible trace. Use development trajectories, not held-out
evaluation data. The helper bounds the input, rejects grader/secret markers,
and records source hashes. These filters do not establish that arbitrary input
traces are free from contamination; the caller must supply appropriate data.

Explicit `--execute --model MODEL --endpoint URL --budget-tokens N
--output-dir NEW_DIR` launches ROF to write procedure proposals. Proposals remain
labelled unvalidated with provenance; cross-repository held-out evaluation is
required before promotion. This command is paid when connected to a paid model.

## Alternative candidates

`python3 tools/rof_candidates.py PLAN.json` runs two candidates in separate
copies of a source directory. Required plan fields are `source_dir`, `binary`,
`out_dir` (new), `model`, `goal`, `alternative_goal`, `check` (a public command argv
array), `budget_tokens`, `budget_steps`, `budget_actions` and
`total_timeout_seconds`. Optional fields are `endpoint` and `extra_args`.
The runner splits declared caps between candidates and uses a shared deadline.
ROF's token caps retain their existing grace/overshoot semantics; this helper
does not provide a hard provider spending quota. Use the campaign's reservation
proxy when spending paid-pilot funds.

The first candidate passing the fixed public check is selected; if neither
passes, the control is retained as a fallback, not labelled a success. Both
workspaces, logs, final cumulative usage and selection evidence are preserved
under `out_dir`. Explicit local check inputs are fingerprinted to reject a
candidate changing its own check. This is not a tamper-resistant grader: host
processes have host permissions and separate work directories do not isolate
external services or container state. Hidden benchmark tests cannot be used as
the selection command. This helper is an explicit experiment, not automatic
branching inside the runtime loop.
