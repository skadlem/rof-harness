# Terminal-Bench 4.1 ROF pilot record

**Purpose.** Eight paid rows cover three selected tasks: three
`session-window-debug` rows, three `wal-recovery-ordering` rows and two
`react-lead-form` rows. Every row scored native reward 0.0 except two: the WAL
think256 row (1.0, the only clean completed ROF pass) and the interrupted React
retention treatment (artifact 1.0, but its provider attempt and usage
accounting were incomplete). These are task-level development results, not a
Terminal-Bench suite score or a general performance claim.

The original ledger and unknown request below remain unchanged. A later owner
instruction authorized bounded continuation in a separate subledger under the
same total $2.82 ceiling; see the [phase 3–5 record](phase-3-5-implementation.md).
Its new runtime/results must not be pooled with this frozen historical pilot.

## Recipe shared by all eight rows

Frozen ROF runtime
`/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261110/runtime/rof`,
SHA-256
`c9a25f9041e54540e03feeb1079a63f9ed0a3dd08c88d841fc7116af52ba3aab`.
Gross token budget 1,200,000; 120 steps; 240 actions; baseline
observation/thinking keep 5/2; legacy workflow; preserve-failed-checks off;
reasoning-effort override unset; per-request output starts at 8,000 and retry
escalation can raise it through 16,000 and 32,000 to the 32,768 ceiling; exec
policy `any`; steering and streak steering off; keep-thinking on; dirty
workdirs allowed. Every row is one attempt, one concurrent trial, zero retries
under a 3,600-second internal ROF deadline, so the shorter guard bounds each
row even where Harbor honors a larger task-native agent allowance. Model API
alias `deepseek-flash`: the task sees a proxy URL and dummy API key; the real
key stays in the driver process, injected upstream by the in-process proxy.

The larger pilot token/step recipe is chosen to allow repository
investigation; it does not change ROF's defaults or establish an optimal
budget. Never reset or seed the ledger lower: unknown usage, proxy denial, a
failed/errored trial or zero valid receipts ends the experiment with no
automatic retry. Cost figures are peak-rate accounting bounds, not invoices.

Runner: Harbor 0.23.0 with a campaign-local ownership patch. Docker's mounted
reward file could be root-owned mode 0400 until environment cleanup, so the
earlier host-side reward read failed; the patch invokes Harbor's existing
`prepare_logs_for_host()` after grading and before reading rewards, changing
ownership only and preserving file bytes and mode. Original task and grader
unchanged. Both overlays are local, not changes to the globally installed
package, and vanilla installed Harbor does not supply them: the pilot adapter's
overlay setting and `PYTHONPATH` are part of the runner configuration.

## Completed Session Window slice

- Dataset package `terminal-bench/terminal-bench-cpu@4.1.0`; tag `v4.1.0`,
  commit `4070d6cef56a7d20cb5dc98d5e10ada88a6b9440`. Task
  `terminal-bench/session-window-debug` version `2.3.0`, fetch manifest
  SHA-256 `7f0633f3c71fe9824cce50628321760f49b29ed21f95c7264096ce6fde3f3953`.
  Fetched from the official release commit rather than the whole CPU package;
  all 29 source-file hashes were checked against the pinned fetch manifest
  without displaying verifier or solution code.
- Native limits 2 CPUs, 4,096 MB memory, 10,240 MB storage, separate verifier
  environment, 300-second verifier timeout, 8-hour agent allowance. Preflight
  host: 12 CPUs, ~15.9 GB RAM, 7.7 GB free on the Docker filesystem; storage
  headroom was tight.
- Ledger at preregistration: $0.114431406 peak-accounted, $0.057215703
  off-peak estimate, zero unknown attempts, against the carried $2.82 cap.
- Overlay
  `/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261110/harbor-logfix`,
  patch SHA-256
  `d6437f4034c021670d0cde777ea5d7487dbd3c25801510afc4f5b61752e657b0`.
- Oracle: unpatched, grading was reached but the host read of the protected
  reward failed; patched, the same v4.1 task's free oracle completed with
  reward **1.0**, zero errors and zero retries at
  `/home/madiyar/.local/share/rof-tb/work/newTB41/jobs/oracle-session-window-debug-v410-logfix/`.

## One-case ablation results

Each row is one raw reward on the same pinned TB4.1.0 `session-window-debug`
case; values are not pooled and do not estimate the 63-task CPU suite. B is the
baseline, A changes observation retention, C changes thinking retention
relative to A; other recipe fields stayed fixed.

| Row | Observation / thinking keep | Raw reward | Requests / elapsed | Prompt (cached) | Output (reasoning) | Peak upper bound |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| B | 5 / 2 | 0.0 | 107 / 589 s | 1,185,047 (897,152) | 17,736 (10,677) | $0.113034612 |
| A | 32 / 2 | 0.0 | 58 / 1,707 s | 1,029,619 (346,880) | 175,121 (158,341) | $0.417048180 |
| C | 32 / 256 | 0.0 | 34 / 838.149 s | 1,167,334 (738,550) | 75,385 (63,104) | $0.223528500 |

C's final-looking summary preceded ROF's post-accounting token-budget failure;
the failed-token outcome does not explain its native verifier reward of 0.0.
The artifact was retained, and a separate snapshot audit found the protected
files unchanged. C used fewer output tokens than A, but these three zero-reward
observations show no capability gain from full-history retention. Do not
promote a production default or spend more on Session Window tuning: retire
it. Its three paid rows are complete and all scored 0.0; they are the
development slice, not a held-out result.

Row results, configs and artifact manifests: per-trial files under
`/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261110/jobs/` for `tb41-control-r1-rof-session-window-debug`,
`tb41-obs32-r1-rof-session-window-debug` and
`tb41-obs32-think256-r1-rof-session-window-debug`. C's 34 receipts are slice
`[197:231]` of
`/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261110/receipts.jsonl`;
its result records no Harbor exception, ROF exit code 3, native reward 0.0. At
Session closure the ledger was $0.868042698 against the $2.82 cap, with zero
unknown attempts and $1.951957302 conservative remaining headroom. Historical
TB2.1 cost remains in the global financial ledger and is excluded from current
benchmark comparisons.

## WAL baseline and retention ablations

All three paid WAL rows closed with zero errors and native rewards 0.0, 0.0,
1.0 on the frozen control runtime. control→obs32 changed observation retention
from 5 to 32; obs32→think256 changed thinking retention from 2 to 256 while
observation retention stayed at 32.

| Row | Observation / thinking keep | Native reward | Requests / elapsed | Prompt tokens (cached) | Completion tokens | Peak cost bound |
|---|---:|---:|---:|---:|---:|---:|
| `tb41-wal-control-r1` | 5 / 2 | 0.0 | 81 / 531.504 s | 1,193,432 (974,080) | 13,614 | $0.087986880 |
| `tb41-wal-obs32-r1` | 32 / 2 | 0.0 | 54 / 1,189.307 s | 1,096,555 (348,800) | 109,686 | $0.358042500 |
| `tb41-wal-obs32-think256-r1` | 32 / 256 | 1.0 | 30 / 997.241 s | 926,061 (328,320) | 55,955 | $0.248438220 |

Prompt, cached and completion counts are sums of receipt usage fields, not
thinking estimates; receipt slices are `[231:312]`, `[312:366]` and `[366:396]`
for control, obs32 and think256 in
`/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261110/receipts.jsonl`.
The think256 slice totals 926,061 input tokens (328,320 cached) and 55,955
output tokens, including 39,012 reasoning tokens. Elapsed time, native reward
and row status are from each row's `manifest.json` under
`/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261110/rows/`,
in directories `tb41-wal-control-r1-rof-wal-recovery-ordering`,
`tb41-wal-obs32-r1-rof-wal-recovery-ordering` and
`tb41-wal-obs32-think256-r1-rof-wal-recovery-ordering`.

The think256 run passed, following two zero-reward controls. Each row has one
attempt; broader replication is needed before changing defaults, and a
tokens-per-solved-task ratio against the zero-reward controls is undefined. The
thinking-retention follow-up launched at approximately 06:54 UTC on 2026-11-10
and completed as `AgentCompleted/Passed`; it was the final paid WAL arm, so
further paid WAL tuning is retired. The optional tool-contract runtime was not
used; its score and any wording effect remain unmeasured. The free oracle below
checks task/verifier readiness only, not ROF capability.

- **Pinned source:** Terminal-Bench CPU 4.1.0, tag `v4.1.0`, commit
  `4070d6cef56a7d20cb5dc98d5e10ada88a6b9440`; task
  [`wal-recovery-ordering`](https://github.com/harbor-framework/terminal-bench/tree/4070d6cef56a7d20cb5dc98d5e10ada88a6b9440/tasks/wal-recovery-ordering),
  task version `2.5.0`. Local task path:
  `/home/madiyar/.local/share/rof-tb/datasets/tb41-wal-recovery-ordering-20261110-full/tasks/wal-recovery-ordering`.
  Its 39-file, 345,246-byte fetch manifest is
  `/home/madiyar/.local/share/rof-tb/datasets/tb41-wal-recovery-ordering-20261110-full/FETCH_MANIFEST.json`,
  SHA-256 `7a3ef9d0c0dac8233cc1af5fd9b47a5fb1b58bf439e7e9cc27b0c34caf06087b`.
- **Native task limits:** agent timeout 28,800 seconds; separate verifier
  timeout 1,800 seconds; 2 CPUs, 4,096 MB memory and 10,240 MB storage.
- **Runner patches:** paid runs must use the recorded campaign-local Harbor
  0.23.0 overlay; the WAL overlay is
  `/home/madiyar/.local/share/rof-tb/work/campaign/harbor-rootlogfix`. It
  retains the post-grade host-read patch (`ownership-read-fix.patch`, SHA-256
  `d6437f4034c021670d0cde777ea5d7487dbd3c25801510afc4f5b61752e657b0`) and
  adds the pre-grade root-owner patch (`rootlog-fix.patch`, SHA-256
  `e2458d547e5e964443dfff8c5f7916d34c67f7dfe871a5f8e3ffd4eb308f063f`).
  Overlay trial/verifier source SHA-256 values are
  `93ad8c3b32f51352ae6257543db9c63a3815f7a93b94f9be27de0e870119e72c` and
  `a27254a226d56a9885e28ce59374a1696aed961f97009571570e3bcd753260c9`.
- **Oracle gate:** free oracle reward 1.0, one completed trial, zero errors,
  zero retries and no exceptions. Validation binding:
  `/home/madiyar/.local/share/rof-tb/work/campaign/harbor-rootlogfix/oracle-validation.json`;
  aggregate result:
  `/home/madiyar/.local/share/rof-tb/work/newTB41/jobs/oracle-wal-v410-rootlogfix/result.json`,
  SHA-256 `35794366735b9bb7abab33d686c94d3b2b8b601dc0e45e770e205b6f999ecf3b`.
  The matching task manifest and overlay hashes are bound in that validation
  file. This preflight establishes neither ROF success nor an external
  capability comparison.
- **Spend guard:** starting peak upper bound $0.868042698, zero unknown
  attempts, against the existing $2.82 cap. The control row moved it to
  $0.956029578; obs32 closed at $1.314072078, `denied=false`, headroom
  $1.505927922 at the think256 launch. The final row added $0.248438220 peak
  bound (counterfactual off-peak estimate $0.124219110), bringing the ledger to
  $1.562510298 with zero unknown attempts and `denied=false`; remaining
  conservative headroom is $1.257489702. Use peak pricing, not the
  counterfactual discount.

Keep these task-level comparisons separate from published full-suite aggregates.

## React lead-form transfer check (interrupted)

This two-row check transfers the higher-retention profile to `react-lead-form`
on the frozen runtime and shared recipe above. The adapter's `max_tokens` field
defaults to 8,000 (`108d6c3`) and both registered rows retain that value, so
output allocation is unchanged. The profiles change observation and thinking
retention together: a combined profile transfer, not an isolated ablation of
either setting.

| Row | Observation / thinking keep | Status |
|---|---:|---|
| `tb41-react-control-r1` | 5 / 2 | Complete; native reward 0 |
| `tb41-react-retention-r1` | 32 / 256 | Interrupted; native artifact reward 1, incomplete provider attempt |

The control launched at approximately 07:32 UTC on 2026-11-10 and closed with
native reward 0, zero errors and retries, and 69 provider requests (receipt
slice 396–465). Elapsed time was 796.4754858 seconds; receipts report 1,181,675
input tokens, 962,048 cached input tokens and 22,148 output tokens. The
conservative peak cost bound is $0.098237988. At control closure the ledger was
$1.660748286 against the existing $2.82 cap, with zero unknown attempts,
`denied=false` and $1.159251714 conservative headroom. ROF's 3,600-second
guard is shorter than the task's 28,800-second agent timeout; the task also has
a separate 900-second verifier, 2 CPUs and 4,096 MB memory.

- **Pinned task:** Terminal-Bench CPU 4.1.0, release commit
  `4070d6cef56a7d20cb5dc98d5e10ada88a6b9440`; task
  `terminal-bench/react-lead-form`, local path
  `/home/madiyar/.local/share/rof-tb/datasets/tb41-react-lead-form-20261110-full/tasks/react-lead-form`.
  Its 42-file, 513,556-byte tree fingerprint is
  `c75086b2f2647243cc9192bc92fb4afbc33975ca1ed16048fc852fa49adca717`; the
  fetched manifest SHA-256 is
  `efdba27645aff4db17fd0a52b8f73c4f560ecbb9de256a38077901e5c9480fb9`.
- **Runtime and plans:** both rows use the frozen runtime SHA-256 above.
  Preregistered plans are
  `/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261110/rows-react-control-preregistered.json`
  and
  `/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261110/rows-react-retention-preregistered.json`.
- **Free oracle gate:** reward 1.0, one completed trial, zero errors, retries
  or exceptions. Validation binding
  `/home/madiyar/.local/share/rof-tb/work/newTB41/oracle-react-lead-form-validation.json`,
  SHA-256 `abcf69ec2744e0836780359c9d767a9754f8f18e2913ffa6ae31eaa3861b0c89`;
  aggregate result
  `/home/madiyar/.local/share/rof-tb/work/newTB41/jobs/oracle-react-lead-form-v410-rootlogfix/result.json`,
  SHA-256 `5abcbe8bc33751ec5140b25b1c43747534ad350640bd01c3053f7f523cc4ac96`.
  It binds the task manifest, pinned release commit, result and campaign-local
  Harbor overlay; as with WAL, task readiness only, not ROF capability.

**Interrupted attempt:** the treatment launched at approximately 11:41 UTC on
2026-11-10 after the oracle, frozen runtime, configuration and budget checks
passed. Its artifact received native reward 1.0, with zero Harbor trial errors
or retries, in 872.816 seconds. The agent made 14 writes, 2 edits and 4 test
calls before an upstream request failed without a receipt; the guard then
blocked further calls. Keep the raw artifact reward, but treat the system
comparison and usage totals as incomplete.

The row forwarded 32 requests in total; receipt slice `465:496` covers 31
requests, reporting 1,006,387 input tokens, 692,992 cached input tokens and
62,544 output tokens, including 46,606 reasoning tokens. Known peak-rate cost
is $0.173229252; known off-peak cost is $0.086614626. The missing request's
full $0.3241728 reservation remains in the conservative bound, so the row upper
bound is $0.497402052. The ledger is $2.158150338 against the $2.82 cap, with
$0.661849662 conservative headroom. `unknown=1` and `denied=true`; paid calls
have stopped. The reservation is not evidence of the actual charge.

Row manifests and audited counters: the campaign's
`rows/tb41-react-control-r1-rof-react-lead-form/` and
`rows/tb41-react-retention-r1-rof-react-lead-form/` directories, each with a
`compactsummary.json`. Both audits use the same 17 public fixture files: the
baseline retained all 17 unchanged; the treatment changed 6 and added 5.

WAL remains the clean completed passing row. The optional 5/256 plan was not
launched: the unknown request and spending denial fail its execution
conditions, and no further paid calls are planned in this campaign.

After the incident the campaign proxy gained sanitized exception type,
open/read stage, elapsed time and observed upstream status diagnostics. Eight
local fake-upstream tests passed, including reservation retention after
transport failures and settlement before a downstream disconnect, and the live
ledger was unchanged across verification. The exact historical proxy is saved
as `proxy-before-transport-diagnostics.py`; source hashes and test evidence are
in the campaign's `verification/transport-diagnostics-provenance.json` and
`verification/transport-diagnostics-tests.log`. This change adds no paid retry
and does not recover the missing receipt.

## Why this version and how to read the result

Terminal-Bench v4.1.0 is the latest stable release checked on 2026-11-10. It
changes all 66 tasks and publishes a 63-task CPU-only dataset. These selected
task-level outcomes cannot stand in for the full suite or support variance
estimates.

DeepSeek reports Flash-1017 at 42.3 on Terminal-Bench 4.0 using Claude Code.
That is a vendor aggregate on a prior release, not a per-task or v4.1
reference. The public Terminal-Bench 4.1 leaderboard reports aggregate rows and
exposes no task-level outcomes or trial IDs, so it cannot supply a matched
comparator for Session Window or WAL Recovery; the lookup and limits are
recorded in
`/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261110/external-references/tb41-public-outcomes/2026-11-10.md`,
and per-task results must not be inferred from aggregate scores.

## Sources

- [Official Terminal-Bench v4.1.0 release](https://github.com/harbor-framework/terminal-bench/releases/tag/v4.1.0), published 2026-11-03 20:39 UTC.
- [Official Terminal-Bench 4.1 leaderboard](https://hub.harborframework.com/datasets/terminal-bench/terminal-bench/5?tab=leaderboard&leaderboard=4-1-0), retrieved 2026-11-10.
- [DeepSeek API changelog](https://api-docs.deepseek.com/updates/), retrieved 2026-11-10; reports the 1017 score on TB4.0 and Claude Code harness.
- Local fetched task: `/home/madiyar/.local/share/rof-tb/datasets/tb41-session-window-debug-20261110-full/tasks/session-window-debug`; full source pin and hashes are recorded by `TB41-pilot.py` at execution.
