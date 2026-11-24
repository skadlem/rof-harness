# Phases 3–5 implementation and development campaign

## Implemented locally (2026-11-11)

The owner explicitly requested implementation after phases 0–2 were reverified.
Two Luna builders owned `tools-std` and `context`; root integrated `agent-loop`,
`rof` and Harbor. Fresh Luna review found a checkpoint allowlist issue, now
fixed: fact references must identify records actually shown in the summarized
prefix.

Historical restoration and compaction remain **off by default**. The historical
actor sample did not establish an irrecoverable observation or context overflow;
chosen mutation/pressure fixtures prove mechanics, not benchmark benefit.
Observed-need gates still govern efficacy experiments and default admission.

`--history-read on` registers `history_read` against the current primary WAL. The
registry binds its pathname before rotation/opening; execution reads the new
current file. Collapsed observations advertise WAL-local item IDs and preserve
archive handles. The tool takes an item ID and character offset/page limit,
returns only historical ToolResults with original IDs/time/error metadata, and
never replays a command; reading an originally failed observation is a successful
lookup. Pages default to 8,000 characters, maximum 32,768; the source snapshot is
bounded to 64 MiB and parsed through the existing log validator. Torn final rows
keep the reader's existing semantics; interior corruption fails. IDs may collide
across WALs, so isolation comes from the fixed current path, not the bare ID.
History plus `--no-log` or `--review on` is rejected to keep this primary-only.

Compaction compares prompt pressure against a declared usable context window,
not the cumulative trial token budget. An opt-in CLI call must provide all three:

```text
--compaction 0.7 --context-capacity 131072 --output-reserve 32768
```

These are chosen example values, not a model-capacity claim. Capacity must be
positive and reserve smaller than capacity; reserve must cover the active
OpenAI-compatible client's maximum output allocation, the greater of the initial
cap and 32,768. Run-wide usage/deadline/action budgets remain independent.

`--checkpoint-sources on` independently opts into JSON checkpoint entries
labelled fact, hypothesis or plan. Original user/tool IDs are mapped before
prompt folding loses them; injected old summaries receive no invented ID. Facts
with absent or unshown references become hypotheses; plans remain labelled. A
resolving ID is provenance, not proof that an interpretation is true. Malformed
checkpoints leave history intact and record refusal; summary usage is still
charged. Without the flag, the original summary format remains the control.

Harbor exposes `history_read`, `compaction`, `context_capacity`,
`output_reserve` and `checkpoint_sources` with equivalent validation. Its new
`workdir` option retains `/app` by default and supports Freight's public
`/workspace` requirement; CLI arguments, bootstrap Git commands and environment
cwd use the same value. The legacy eval CLI does not expose these controls.

Validation: 352 Rust tests passed across context, tools, loop and CLI packages;
10 adapter tests, plus scoped all-target Clippy, fmt and Ruff. The actual loop
restored an old observation after its live file was overwritten. Tests cover
unknown IDs, cross-WAL collisions, bounds, malformed logs, source mapping,
unshown references, capacity independent of trial allowance, summary refusal,
disabled defaults and metering. An existing cancellation test now waits for
provider admission rather than racing a fixed timer during snapshot setup. Logs:
`target/phase34-tests.log`, `target/phase34-clippy.log`.

## Bounded phase 5 authorization

The owner explicitly revised the old stop rule: “Allow bounded continuation;
preserve the reservation and $2.82 ceiling.” The original ledger remains
unchanged and its old unknown attempt remains incomplete. The new subledger is
capped at **$0.661849662**: $2.82 minus the existing **$2.158150338** upper bound,
which already includes the unknown **$0.324172800** reservation. Do not add that
reservation twice, reset the old ledger, infer its missing usage, or increase the
combined ceiling.

Authorization and immutable parent hash are recorded under
`/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261111-phases35/authorization.json`.
Synthetic reservation/settlement/new-unknown-stop checks passed without
inference. New paid requests use the existing serialized, receipt-backed proxy
with its cap scoped to this subledger; a new unknown response stops this campaign
too. Peak rates are used conservatively regardless of discount timing. Current
model alias/version, prices and 1M window were checked against the
[official pricing page](https://api-docs.deepseek.com/quick_start/pricing/). The
API alias is not an immutable checkpoint proof.

The official latest release was rechecked on 2026-11-11 through its API:
[Terminal-Bench v4.1.0](https://github.com/harbor-framework/terminal-bench/releases/tag/v4.1.0),
published 2026-11-03, pinned commit
`4070d6cef56a7d20cb5dc98d5e10ada88a6b9440`. The two development tasks are Cargo
Flight Dispatch and Freight Dispatch Shift; Coq Block Bound and VF2 Speedup
remain prospective holdouts. This is a convenience slice, not a full-suite score.
Full packages and native oracle/no-op controls are checked before inference; no
hidden grader feedback enters the agent or chooses a runtime action.

The first matched comparison changes only original `repo_lookup`, with enhanced
evidence/history/compaction disabled. Both arms use the same newly frozen
runtime, model/endpoint and declared budgets. Evidence enhancement is a later
separate comparison. Completion-feedback efficacy additionally requires an
equal-budget repair/resampling control. All attempts, token usage, cap failures
and incomplete pairs stay visible through the offline reporting helper. Nothing
is promoted by these implementation tests or a small development run.

## Phase 5 outcome: incomplete, stopped on a new unknown request

The [frozen protocol](experiments/2026-11-11-lookup-development-protocol.json)
expected two paired tasks/four native trials. Full pinned packages were fetched
and verified (152 files, 1,096,791 bytes). Both development native oracles scored
1.0 and both valid known-bad artifacts scored 0.0. Freight's first plain `nop`
failed artifact collection before grading; it is an invalid readiness run, not a
zero score. Its subsequent executable-exit-1 control graded normally as 0.0.
Holdout packages were fetched/hashed; no holdout outcomes were used.

The fresh runtime was built from clean commit `c522466`, SHA-256
`0e0e8721b73980c161b672a132a948fc455914894ff914b5808df4e0dca3f2cf`. The bundle
loader recognized the new controls before inference. Paid task/image layer IDs
and actual CPU/memory limits were recorded; Cargo's main and verifier containers
each had 2 CPU / 4 GiB limits. These readiness results are not coding-agent
capability scores.

Only Cargo's control ran: native artifact reward **0.0**, 311.08s, 26 forwarded
requests, 25 valid receipts. The next response returned HTTP 200 headers, then
`ConnectionResetError` during body read after 2.768356s; no usage receipt
arrived. The proxy retained its full reservation, blocked subsequent forwarding
and the runner stopped. The reset's original cause is not established. No
treatment or Freight agent attempt launched. There are zero completed task pairs
and zero new clean completed/accounted passes; no lookup efficacy conclusion
follows.

Known receipts report 186,706 input tokens (82,560 cached) and 4,264 output
(1,721 reasoning): **partial**, not whole-attempt totals. Their peak-rate cost is
$0.036855960; the new unknown reservation is $0.324172800, already included in
the new **$0.361028760** upper bound. Added to the immutable old upper bound,
aggregate spend/reservations are **$2.519179098**, below $2.82. The raw difference
is $0.300820902, but usable paid headroom under the new stop rule is **zero**. Do
not reset either ledger or treat either unknown as zero. These are conservative
accounting bounds, not an invoice.

The [campaign summary](experiments/2026-11-11-lookup-campaign-summary.json)
records hashes, accounting and the incomplete verdict. The
[normalized input](experiments/2026-11-11-lookup-normalized.json),
[paired report](experiments/2026-11-11-lookup-report.json) and
[audit](experiments/2026-11-11-lookup-audit.json) retain the three unrun cells
and both incomplete expected pairs. The report reproduces byte-for-byte with
`python3 tools/rof_compare.py --input docs/experiments/2026-11-11-lookup-normalized.json --output REPORT.json`.
The actor's failed RunEnd is structurally valid; incomplete accounting and the
transport-induced proxy denial prevent a completed comparison. The old pilot
results remain separate: different runtime, tasks and protocols cannot be pooled
into a suite score. Resume paid evaluation only after authoritative usage
resolves the new stop or an explicit owner policy change that still preserves
all reservations.
