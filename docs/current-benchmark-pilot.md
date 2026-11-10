# Terminal-Bench 4.1 ROF pilot preregistration

**Purpose.** One paid ROF run on `session-window-debug` validates the current agent, proxy, accounting receipts, and task/verifier wiring. It is a development slice, not a Terminal-Bench score or a frontier claim.

## Frozen inputs and runner profile

- Dataset package: `terminal-bench/terminal-bench-cpu@4.1.0`; Terminal-Bench tag `v4.1.0`, commit `4070d6cef56a7d20cb5dc98d5e10ada88a6b9440`.
- Task: `terminal-bench/session-window-debug`, task version `2.3.0`, fetched artifact manifest SHA-256 `7f0633f3c71fe9824cce50628321760f49b29ed21f95c7264096ce6fde3f3953`.
- One attempt, one concurrent trial, no retries. Model API alias `deepseek-flash`; local task receives proxy URL and dummy API key. Real key remains in the driver process and is injected upstream by the in-process proxy.
- ROF profile: gross token budget 1,200,000; 120 steps; 240 actions. Legacy workflow; preserved failed checks off; reasoning-effort override unset; per-request max output 8,000. Exec policy `any`; steering and streak steering off; keep-thinking/default behavior; allow dirty workdir.
- Harbor honors the task's native 8-hour agent allowance, but ROF's internal run guard is 3,600 seconds. This is a shorter pilot allowance and is not identical to a run using the full task-native timeout.
- Same carried ledger; cap $2.82. At preregistration the parent reports $0.114431406 peak-accounted and $0.057215703 off-peak estimate, with zero unknown attempts. Never reset or seed lower. Unknown usage, proxy denial, failed/errored trial, or zero valid receipts ends the experiment; no automatic retry.

The fetched task metadata specifies 2 CPUs, 4,096 MB memory, 10,240 MB storage, a separate verifier environment, and a 300-second verifier timeout. Host snapshot had 12 CPUs, ~15.9 GB RAM, and 7.7 GB free on the Docker filesystem. The storage headroom is tight; confirm the oracle/build path and watch disk before a paid run.

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
global installed packages are unchanged. Oracle validation is required before
paid execution.

Measured validation: the unpatched free oracle reached grading but failed when
the host read the protected reward. With the ownership-order patch, the same
v4.1 task's oracle completed with reward **1.0**, zero errors and zero retries.
That result is under `work/newTB41/jobs/oracle-session-window-debug-v410-logfix/`.
The paid ROF row `tb41-control-r1-rof-session-window-debug` started afterward;
its agent result is pending. No paid comparator runs are part of this protocol.

## Why this version and how to read the result

Terminal-Bench v4.1.0 is the latest stable release checked on 2026-11-10. It changes all 66 tasks and publishes a 63-task CPU-only dataset. The old TB2.1 work is superseded for current benchmark claims. This one-task run only checks the first current-release case and cost profile; it cannot stand in for the 63-task suite or support variance estimates. First valid receipts determine whether a later slice can fit the remaining balance.

DeepSeek reports Flash-1017 at 42.3 on Terminal-Bench 4.0 using Claude Code. That is a vendor aggregate on a prior release, not a per-task or v4.1 reference; the public 4.1 Hub leaderboard did not show a Flash-1017 row when checked. Treat it as context only. The earlier TB2.1 row's cost is a measured anchor, not a forecast for this task.

## Sources

- [Official Terminal-Bench v4.1.0 release](https://github.com/harbor-framework/terminal-bench/releases/tag/v4.1.0), published 2026-11-03 20:39 UTC.
- [Official Harbor Hub package and leaderboard](https://hub.harborframework.com/datasets/terminal-bench/terminal-bench/5?tab=leaderboard), retrieved 2026-11-10.
- [DeepSeek API changelog](https://api-docs.deepseek.com/updates/), retrieved 2026-11-10; reports the 1017 score on TB4.0 and Claude Code harness.
- Local fetched task: `/home/madiyar/.local/share/rof-tb/datasets/tb41-session-window-debug-20261110-full/tasks/session-window-debug`; full source pin and hashes are recorded by `TB41-pilot.py` at execution.
