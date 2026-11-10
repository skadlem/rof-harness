# Terminal-Bench 4.1 ROF pilot record

**Purpose.** Three paid ROF runs on one `session-window-debug` case record the current agent, proxy, accounting receipts, and task/verifier wiring. This development slice is not a Terminal-Bench score or a frontier claim.

## Frozen inputs and runner profile

- Dataset package: `terminal-bench/terminal-bench-cpu@4.1.0`; Terminal-Bench tag `v4.1.0`, commit `4070d6cef56a7d20cb5dc98d5e10ada88a6b9440`.
- Task: `terminal-bench/session-window-debug`, task version `2.3.0`, fetched artifact manifest SHA-256 `7f0633f3c71fe9824cce50628321760f49b29ed21f95c7264096ce6fde3f3953`.
- One attempt, one concurrent trial, no retries. Model API alias `deepseek-flash`; local task receives proxy URL and dummy API key. Real key remains in the driver process and is injected upstream by the in-process proxy.
- ROF profile: gross token budget 1,200,000; 120 steps; 240 actions. Legacy workflow; preserved failed checks off; reasoning-effort override unset; per-request output starts at 8,000 and escalates up to 32,000. Exec policy `any`; steering and streak steering off; keep-thinking/default behavior; allow dirty workdir.
- Harbor honors the task's native 8-hour agent allowance, but ROF's internal run guard is 3,600 seconds. This is a shorter pilot allowance and is not identical to a run using the full task-native timeout.
- Same carried ledger; cap $2.82. At preregistration the parent reports $0.114431406 peak-accounted and $0.057215703 off-peak estimate, with zero unknown attempts. Never reset or seed lower. Unknown usage, proxy denial, failed/errored trial, or zero valid receipts ends the experiment; no automatic retry.

The fetched task metadata specifies 2 CPUs, 4,096 MB memory, 10,240 MB storage, a separate verifier environment, and a 300-second verifier timeout. The preflight host snapshot had 12 CPUs, ~15.9 GB RAM, and 7.7 GB free on the Docker filesystem; storage headroom was tight.

The selected task was fetched from the official release commit, rather than
downloading the entire CPU package. All 29 source-file hashes were checked
against the pinned fetch manifest without displaying verifier or solution code.
The larger pilot token/step recipe is chosen to allow repository investigation;
it does not change ROF's defaults or establish an optimal budget.

Runner: Harbor 0.23.0 with a campaign-local ownership patch. Docker's mounted
reward file could be root-owned with mode 0400 until environment cleanup,
causing the earlier host-side reward read to fail. The patch invokes Harbor's
existing `prepare_logs_for_host()` after grading and before reading rewards.
It changes ownership only, preserving file bytes and mode. The original task
and grader are unchanged. Patch and source hashes are recorded in each manifest;
global installed packages are unchanged. Oracle validation was completed before
the paid runs.

Measured validation: the unpatched free oracle reached grading but failed when
the host read the protected reward. With the ownership-order patch, the same
v4.1 task's oracle completed with reward **1.0**, zero errors and zero retries.
That result is under `work/newTB41/jobs/oracle-session-window-debug-v410-logfix/`.

## One-case ablation results

Each row is one raw reward on the same pinned TB4.1.0 `session-window-debug`
case; values are not pooled and do not estimate the 63-task CPU suite. B is the
baseline, A changes observation retention, and C changes thinking retention
relative to A. Other recipe fields stayed fixed.

| Row | Observation / thinking keep | Raw reward | Requests / elapsed | Prompt (cached) | Output (reasoning) | Peak upper bound |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| B | 5 / 2 | 0.0 | 107 / 589 s | 1,185,047 (897,152) | 17,736 (10,677) | $0.113034612 |
| A | 32 / 2 | 0.0 | 58 / 1,707 s | 1,029,619 (346,880) | 175,121 (158,341) | $0.417048180 |
| C | 32 / 256 | 0.0 | 34 / 838.149 s | 1,167,334 (738,550) | 75,385 (63,104) | $0.223528500 |

C's final-looking summary preceded ROF's post-accounting token-budget failure;
the failed-token outcome does not explain its native verifier reward of 0.0.
The task artifact was retained, and a separate snapshot audit found the
protected files unchanged. C used fewer output tokens than A, but these three
zero-reward observations show no capability gain from full-history retention.
Do not promote a production default or spend more on Session Window tuning.

Row result, config, and artifact manifest sources are the per-trial files under
`/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261110/jobs/` for
`tb41-control-r1-rof-session-window-debug`,
`tb41-obs32-r1-rof-session-window-debug`, and
`tb41-obs32-think256-r1-rof-session-window-debug`. C's 34 receipts are slice
`[197:231]` of
`/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261110/receipts.jsonl`.
Its result records no Harbor exception, ROF exit code 3, and native reward 0.0.
The carried ledger records $0.868042698 peak upper bound against the $2.82
cap, with zero unknown attempts; conservative remaining headroom is
$1.951957302. These are accounting bounds, not an invoice. Historical TB2.1
cost remains in the global financial ledger and is excluded from current
benchmark comparisons.

Retire further Session Window tuning. Broader held-out WAL work remains
pending a privilege-fix preflight before any paid run.

## Why this version and how to read the result

Terminal-Bench v4.1.0 is the latest stable release checked on 2026-11-10. It changes all 66 tasks and publishes a 63-task CPU-only dataset. This three-row, one-case slice cannot stand in for the 63-task suite or support variance estimates.

DeepSeek reports Flash-1017 at 42.3 on Terminal-Bench 4.0 using Claude Code. That is a vendor aggregate on a prior release, not a per-task or v4.1 reference; the public 4.1 Hub leaderboard did not show a Flash-1017 row when checked. Treat it as context only.

## Sources

- [Official Terminal-Bench v4.1.0 release](https://github.com/harbor-framework/terminal-bench/releases/tag/v4.1.0), published 2026-11-03 20:39 UTC.
- [Official Harbor Hub package and leaderboard](https://hub.harborframework.com/datasets/terminal-bench/terminal-bench/5?tab=leaderboard), retrieved 2026-11-10.
- [DeepSeek API changelog](https://api-docs.deepseek.com/updates/), retrieved 2026-11-10; reports the 1017 score on TB4.0 and Claude Code harness.
- Local fetched task: `/home/madiyar/.local/share/rof-tb/datasets/tb41-session-window-debug-20261110-full/tasks/session-window-debug`; full source pin and hashes are recorded by `TB41-pilot.py` at execution.
