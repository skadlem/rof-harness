# Harness Leaderboard review

Checked 2026-11-10 against the public Space, pinned source and `/api/index`. No new paid run needed.

## What it measures

- Compares harnesses on a fixed model, not models against each other. Configured benchmarks: Terminal-Bench 2.1 and DeepSWE v1.1; no TB4.x, so a recent update date is not a current TB4.x result.
- Runs launch privately on HF; no public submission path for a local ROF result.
- Pinned revision: 87 TB2.1 tasks (89 upstream minus two QEMU tasks unsupported on HF Sandbox), 111 DeepSWE tasks; fixed 3-task smoke and 50-task pilot each, three attempts per task at full. DeepSWE smoke IDs: `eicrud-keyset-pagination-cursor`, `httpx-streaming-json-iteration`, `tengo-destructuring-bindings`.
- Scoring: attempts averaged per task, then scored tasks per benchmark; the headline macro weights benchmarks equally. Infra failures retried and excluded. 95% confidence intervals and per-trial costs reported.
- TB keeps internet; DeepSWE offline since 2026-10-23, older non-looked-up trials remain, so historical DeepSWE mixes network conditions.
- Project target: official Terminal-Bench v4.1.0, published 2026-11-03 20:39:05 UTC, tag commit `4070d6cef56a7d20cb5dc98d5e10ada88a6b9440`; modifies 66 tasks, publishes a 63-task CPU dataset, release notes name `session-window-debug`. Pin with `harbor run -d terminal-bench/terminal-bench@4.1.0`, not the Space's TB2.1 set.

## Published reference and limits

### Official Terminal-Bench 4.1 aggregate snapshot

Board `terminal-bench/terminal-bench/4-1-0` fetched 2026-11-10 06:10:07 UTC; raw 35-row response SHA-256 `966e176efb12e88da24ec81bfdbe90affcad628fad331f822d586f24c9e1538e`.

| Rank | Published agent | Published model | Effort | Accuracy | Trials |
|---:|---|---|---|---:|---:|
| 1 | Claude Code | Opus 5.5 | max | 65.76% | 330 |
| 2 | Claude Code | Sonnet 5.5 | max | 63.94% | 330 |
| 3 | Codex | GPT-6.1 Sol | max | 60.91% | 330 |

- `accuracy` is already a percentage; do not multiply by 100.
- Board aggregates only: no per-task outcomes, trial IDs or CPU-only score. The local target is the 63-task CPU slice; infer neither a local task score nor a matched ROF comparison.
- Names are display aliases, not guaranteed checkpoint IDs; no row uses `DeepSeek Flash`.
- `Release Date` reads `metadata.date`, not a trial run date; the meaning is undocumented, so do not date experiments by it.
- The three Session Window ROF rows and any single-task WAL result are not a full-suite score or a same-model delta.

Primary source: [official Terminal-Bench 4.1 leaderboard](https://hub.harborframework.com/datasets/terminal-bench/terminal-bench/5?tab=leaderboard&leaderboard=4-1-0). Raw response, fetch record and summary are under `/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261110/external-references/tb41-public-outcomes/`: `leaderboard-2026-11-10T061007Z.json`, `fetch-2026-11-10T061007Z.txt`, `summary-2026-11-10.md`.

### HF Space payload (TB2.1 and DeepSWE)

Live payload generated `2026-11-10T00:32:52Z`, version `0.1.2`, SHA-256 `9c8a9487146a436054e0be17dec107c2867e82c14cb1119f519de8c5a6b59f5a`; cached at `/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261110/external-references/harness-leaderboard/api-index.json`.

Model ID `deepseek-ai/DeepSeek-V4.1-Flash`, via DeepInfra (reference) and equivalence-checked Novita, fp8. Not proof of official DeepSeek API or exact `Flash-1017` checkpoint identity.

OpenCode 2.2.6 at low effort: TB2.1 `rate: 0.7663` (95% CI `0.6673–0.8428`) at measured `$0.02583` per trial; DeepSWE `rate: 0.6697` (95% CI `0.5778–0.7502`) at measured `$0.1513` per trial. These are provider- and harness-specific observed costs, not a forecast for ROF or a ceiling. The payload's `passed: 66.67` is an average task count; use `rate` for the fractional resolve rate.

Provider, harness, task protocol and benchmark version must match before attributing differences to the agent. Do not spend the remaining budget rerunning old TB2.1 tasks.

## Cheapest useful route

Use the public leaderboard as reference without competitor or model calls. Reuse the completed ROF result first if task IDs, model/checkpoint, effort and scoring are known; otherwise use the project's current benchmark and compare only same-version results.

Fetch the public score payload (read-only, no model credentials):

```sh
curl -fsS https://joelniklaus-harness-leaderboard.hf.space/api/index \
  -o /tmp/harness-leaderboard-api-index.json
```

Inspect the locally installed Harbor job schema and command form (these do not launch a trial):

```sh
harbor job schema
harbor run --help
```

The local ROF adapter is `tools.harbor_rof:RofAgent`; Harbor's `AgentConfig` accepts it through `import_path`, with adapter settings under `kwargs`. For one current task:

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

Adapter defaults: 300,000 gross tokens, 60 steps, 120 actions, 8,000-token replies, legacy workflow, failed-check preservation off, steering off. Complete the config with the campaign environment, endpoint and credential injection, then preview with `harbor run --config <job.json> --print-config`. Running `harbor run --config <job.json>` makes paid model calls; this review did not run it. Prefer the project's guarded campaign driver for paid trials so the spend ceiling and receipts remain authoritative.

## Source and snapshot provenance

- Space source revision: `c694e3286e24bb710316fd9e6509f0df96561a1b`.
- Terminal-Bench source: [v4.1.0 release](https://github.com/harbor-framework/terminal-bench/releases/tag/v4.1.0), tag commit `4070d6cef56a7d20cb5dc98d5e10ada88a6b9440`.
- Read-only source snapshot inspected under `/tmp/harness-leaderboard-c694e328`; public result payload cached at the path and SHA-256 above.
- Primary links: [Space README](https://huggingface.co/spaces/joelniklaus/harness-leaderboard/blob/c694e3286e24bb710316fd9e6509f0df96561a1b/README.md), [benchmark registry](https://huggingface.co/spaces/joelniklaus/harness-leaderboard/blob/c694e3286e24bb710316fd9e6509f0df96561a1b/hlb/data/benchmarks.json), [model registry](https://huggingface.co/spaces/joelniklaus/harness-leaderboard/blob/c694e3286e24bb710316fd9e6509f0df96561a1b/hlb/data/models.json), [scoring code](https://huggingface.co/spaces/joelniklaus/harness-leaderboard/blob/c694e3286e24bb710316fd9e6509f0df96561a1b/hlb/scoring.py), [live index](https://joelniklaus-harness-leaderboard.hf.space/api/index).
