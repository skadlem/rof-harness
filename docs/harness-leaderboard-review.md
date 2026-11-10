# Harness Leaderboard review

Checked 2026-11-10 against the public Space, pinned source, and `/api/index`.
This reference does not require a new paid benchmark run.

## What it measures

The Space compares coding-agent harnesses on a fixed model, not models against
each other. Its currently configured benchmarks are Terminal-Bench 2.1 and
DeepSWE v1.1. It does not configure Terminal-Bench 4.x, so its recent update
date does not make it a current TB4.x result. The Space README says runs are
launched privately by the owner on HF infrastructure; there is no public
submission command for adding a local ROF result.

At the pinned revision, scored sets are 87 TB2.1 tasks (89 upstream minus two
QEMU tasks unsupported on HF Sandbox) and 111 DeepSWE tasks. Each has a fixed
3-task smoke and 50-task pilot; full uses three attempts per task. DeepSWE
smoke IDs: `eicrud-keyset-pagination-cursor`, `httpx-streaming-json-iteration`,
`tengo-destructuring-bindings`.

For the current project target, official Terminal-Bench v4.1.0 was published
2026-11-03 20:39:05 UTC (tag commit `4070d6cef56a7d20cb5dc98d5e10ada88a6b9440`).
It modifies 66 tasks and publishes a 63-task CPU dataset; release notes name
`session-window-debug`. Pin runs with `harbor run -d terminal-bench/terminal-bench@4.1.0`, not the Space's TB2.1 set.

Full-stage scoring averages attempts per task, then scored tasks per benchmark;
the headline macro weights benchmarks equally. Infra failures are retried and
excluded. The Space reports 95% confidence intervals and per-trial costs. TB
keeps internet; DeepSWE is offline since 2026-10-23, but older non-looked-up
trials remain, so historical DeepSWE mixes network conditions.

## Published reference and limits

The live payload was generated `2026-11-10T00:32:52Z`, version `0.1.2`,
SHA-256 `9c8a9487146a436054e0be17dec107c2867e82c14cb1119f519de8c5a6b59f5a`;
cached at `/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261110/external-references/harness-leaderboard/api-index.json`.

The board's model ID is `deepseek-ai/DeepSeek-V4.1-Flash`, routed through
DeepInfra (reference) and equivalence-checked Novita, fp8. This does not prove
official DeepSeek API or exact `Flash-1017` checkpoint identity.

For one concrete gateway comparison, OpenCode 2.2.6 at low effort scored
`rate: 0.7663` on TB2.1 (95% CI `0.6673–0.8428`) at measured `$0.02583` per
trial, and `rate: 0.6697` on DeepSWE (95% CI `0.5778–0.7502`) at measured
`$0.1513` per trial. These are provider- and harness-specific observed costs,
not a forecast for ROF or a ceiling. The payload's `passed: 66.67` is an
average task count; use `rate` for the fractional resolve rate.

These results are context, not a direct ROF comparison: provider, harness,
task protocol, and benchmark version must match before attributing differences
to the agent. Do not spend the remaining budget rerunning old TB2.1 tasks.

## Cheapest useful route

Use the public leaderboard as the reference without competitor or model calls.
First reuse the completed ROF result if task IDs, model/checkpoint, effort, and
scoring are known. For new evidence, use the project's current benchmark and
compare only with results from the same version.

Fetch the public score payload (read-only, no model credentials):

```sh
curl -fsS https://joelniklaus-harness-leaderboard.hf.space/api/index \
  -o /tmp/harness-leaderboard-api-index.json
```

Inspect the locally installed Harbor job schema and command form (these do not
launch a trial):

```sh
harbor job schema
harbor run --help
```

The local ROF adapter is `tools.harbor_rof:RofAgent`; Harbor's `AgentConfig`
accepts it through `import_path`, with adapter settings under `kwargs`. A local
job config can use this project profile for a single current task:

```json
{
  "agents": [{
    "import_path": "tools.harbor_rof:RofAgent",
    "model_name": "openai/deepseek-flash",
    "kwargs": {
      "runtime_dir": "/absolute/path/to/runtime",
      "budget_tokens": 1200000,
      "budget_steps": 120,
      "budget_actions": 240,
      "budget_shape": "gross",
      "reasoning_effort": "high",
      "preserve_failed_checks": false,
      "workflow": "legacy"
    }
  }],
  "datasets": [{
    "name": "terminal-bench/terminal-bench",
    "version": "4.1.0",
    "task_names": ["session-window-debug"]
  }]
}
```

The adapter defaults remain 300,000 gross tokens, 60 steps, 120 actions,
8,000-token replies, legacy workflow, failed-check preservation off, and
steering off. Complete the config with the campaign environment, endpoint, and
credential injection, then preview it with `harbor run --config <job.json>
--print-config`. Running
`harbor run --config <job.json>` makes paid model calls; this review did not
run it. Prefer the project's existing guarded campaign driver for any paid
trial so the current spend ceiling and receipts remain authoritative.

## Source and snapshot provenance

- Space source revision: `c694e3286e24bb710316fd9e6509f0df96561a1b`.
- Terminal-Bench source: [v4.1.0 release](https://github.com/harbor-framework/terminal-bench/releases/tag/v4.1.0), tag commit `4070d6cef56a7d20cb5dc98d5e10ada88a6b9440`.
- Read-only source snapshot inspected under `/tmp/harness-leaderboard-c694e328`.
- Public result payload: cached path and SHA-256 above.
- Primary links: [Space README](https://huggingface.co/spaces/joelniklaus/harness-leaderboard/blob/c694e3286e24bb710316fd9e6509f0df96561a1b/README.md), [benchmark registry](https://huggingface.co/spaces/joelniklaus/harness-leaderboard/blob/c694e3286e24bb710316fd9e6509f0df96561a1b/hlb/data/benchmarks.json), [model registry](https://huggingface.co/spaces/joelniklaus/harness-leaderboard/blob/c694e3286e24bb710316fd9e6509f0df96561a1b/hlb/data/models.json), [scoring code](https://huggingface.co/spaces/joelniklaus/harness-leaderboard/blob/c694e3286e24bb710316fd9e6509f0df96561a1b/hlb/scoring.py), [live index](https://joelniklaus-harness-leaderboard.hf.space/api/index).
