# ROF

**A Rust coding-agent harness built to close the gap between Flash-class and frontier models.**

[![CI](https://github.com/skadlem/rof-harness/actions/workflows/ci.yml/badge.svg?branch=v2-core)](https://github.com/skadlem/rof-harness/actions/workflows/ci.yml)
![License](https://img.shields.io/badge/license-Apache--2.0-blue)
&nbsp; [Architecture](docs/architecture.md) · [Experiments](docs/harness-experiments.md) · [Roadmap](docs/research-next-steps.md)

## The harness

| Layer | What ROF controls |
|---|---|
| **Context** | Durable event log; recent observations and reasoning kept, older output collapsed, evidence retrieved on demand. |
| **Tools** | Inspect, edit, execute and test in the task workspace. Opt into repository evidence search, archived output retrieval, multi-file edits. |
| **Loop** | Tool calls recorded before they run, failed file batches rolled back, step/action/token/wall budgets enforced. Opt into public completion checks or bounded review. |
| **Evaluation** | Run in the task environment, grade with native verifiers, record runtime, configuration and provider usage per trial. |

## How context works

1. **Build from the event log.** Reconstruct messages in order, tool calls paired with results.
2. **Keep recent evidence.** By default the last **5** tool results are sent as recorded and reasoning from the last **2** assistant messages is kept. Older results become their byte count plus a **120-character** preview; older reasoning is trimmed.
3. **Keep the prefix stable.** Workflow and startup file map up front, pinned excerpts while they still match, live budget at the request tail.
4. **Recover detail on demand.** Re-open files; the development build can page archived command output or retrieve earlier observations by ID.

Optional compaction summarizes older turns in a separate model call; the event log retains the recorded history.

### Local measurements

Scripted local provider, development build `97075cf`, 40 scripted 8 KiB reads.

| Check | Result |
|---|---|
| **Request payload** | **72% fewer cumulative bytes**, **83% smaller final request**. Kept 5 results versus all 40. |
| **Output recovery** | **1 MiB recovered byte-for-byte** with archive retrieval. The collapsed result was **159 bytes**, including its handle. |
| **Batch rollback** | **16/16** injected failures restored the pre-batch files and preserved the earlier successful edit. |

[Method and exact counts](docs/experiments/2026-11-23-local-harness-checks.md)

## Results

| Experiment | Measured result |
|---|---|
| [Terminal-Bench 2.1 Slice A](docs/experiments/2026-11-11-slice-a-faithful.md) | **5/18 solved** with DeepSeek Flash at a 300k gross-token run budget; one rep per task. |
| [Reasoning retention · Terminal-Bench 4.1](docs/current-benchmark-pilot.md#wal-baseline-and-retention-ablations) | **Fail → pass** on WAL recovery with **19% fewer gross tokens**. One attempt per setting. |
| [Budget visibility · matched A/B](docs/experiments/2026-11-23-budget-line-ab.md) | **1/4 vs 0/4** across two Slice A tasks. The treatment pass did not repeat. |

**Benchmark target** · Terminal-Bench 4.1 CPU, same model and budget per matched configuration; track native solves and tokens per solve. [Evaluation protocol](docs/research-next-steps.md)

## Quickstart

Build with Rust and Cargo. Set `DEEPSEEK_API_KEY` first.

```sh
cargo build --release -p rof

workdir=$(mktemp -d)
./target/release/rof run \
  --goal "Write a short README for this scratch project" \
  --workdir "$workdir" \
  --model deepseek-flash --endpoint https://api.deepseek.com \
  --api-key-env DEEPSEEK_API_KEY --budget-steps 10
```

Use a disposable workspace. ROF edits it in place.

| Command | Use |
|---|---|
| `rof run` | Run an agent on a goal and emit its patch. |
| `rof chat` | Work interactively in a workspace. |
| `rof eval` | Run repeatable task evaluations. [Harbor adapter](tools/harbor_rof.py) |

[Full documentation index](docs/INDEX.md) · [License](LICENSE)
