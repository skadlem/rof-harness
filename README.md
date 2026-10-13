# rof

[![CI](https://github.com/skadlem/rof-harness/actions/workflows/ci.yml/badge.svg)](https://github.com/skadlem/rof-harness/actions/workflows/ci.yml)

A minimal, measurable coding-agent harness in Rust: an OpenAI-compatible
model drives a tool loop against a scratch workdir — capped by budgets,
guarded by a git-overlay snapshot tree, recorded to a write-ahead log —
and the deliverable is the **full, uncut diff** of everything the agent
changed, printed to stdout.

No planner scaffolding, no framework lock-in. One loop, six tools, and
a benchmark rig strict enough to publish against.

## Design principles

- **The log is the truth; the prompt is a view.** Every request is
  derived from an append-only write-ahead log written *before* each
  effect. Crash mid-run and you can replay exactly what the agent did.
- **Budgets are the kill switch.** Steps, actions, tokens, dollars, and
  wall-clock, checked at the top of every step. A confused model cannot
  loop forever on your dime.
- **Honest reporting is enforced in code.** Unknown costs are `null`,
  never `$0.00`; infra failures never score as capability; every rate
  ships with a Wilson confidence interval; flaky and saturated cells are
  flagged rather than averaged away.
- **Frozen benchmarks, digest-pinned.** Task slices are locked with
  SHA-256 digests — a re-frozen slice fails `cargo test` before any run.
- **Minimal by default.** Hand-rolled bootstrap and Wilson gates with
  zero dependencies; experimental features ship behind flags that stay
  off until measurements earn them a place.

## Quickstart

```sh
cargo build -p rof
export OPENAI_API_KEY=...

# Point an agent at a scratch repo and let it work:
./target/debug/rof run --goal "fix the failing test" --workdir /tmp/w \
  --model <id> --endpoint https://api.deepseek.com/v1 \
  --allow-cmd "cargo test" --budget-steps 20
```

The full unified diff of the workdir lands on stdout; the exit code says
what happened. `--dump-events PATH` writes the complete event log for
replay or post-mortem.

To benchmark an agent on the frozen Terminal-Bench slice:

```sh
./target/debug/rof eval --agent --in-container --tasks-dir <tb-tasks> \
  --ids <task> --model <id> --endpoint <url> \
  --allow-cmd python3 --report out.json
```

## Measured results

All numbers are reward-verified — graded by each task's own verifier
artifacts, not by exit codes (see the note below).

| Measurement | Result |
|---|---|
| 26-cell in-container arm, frozen TB slice, glm-5.3-flash (2026-10-12) | 6 Resolved / 17 Unresolved / 1 infra — **$0.76 total spend** |
| File-artifact widening, 18 tasks (2026-10-12) | 4/18 solved, partial credit on 4 more |
| Env-mutation task class (live servers, agent-installed packages) | solvable in-container; structurally impossible under file-copy grading |
| Cost per cell (flash-tier models) | $0.006–$0.13; frontier-tier harnesses report $6–$23 per task on the full 89-task suite |

Capability signal is early — the point of the rig is that the numbers
above can be trusted. Case in point: an audit after the widening found
that every task's test script ends with `echo ... > reward.txt` and so
*always exits 0*; the original exit-code grading read workless runs as
passes (a "25/26" that was really 6/26, including a corrected 2/8 on the
earlier banked cells). The grader now reads the task-written reward file,
and every published number on this page was re-derived from it. The
benchmark caught the harness's own bug — that is the feature.

## What a run looks like

One step of the loop:

1. Build the request from the log (system head frozen for prefix
   caching; old tool observations collapsed to one-line stubs; echoed
   reasoning trimmed — thinking was 64% of input tokens on heavy traces).
2. Ask the model.
3. If the reply carries tool calls, gate them, execute, and fold the
   results back in.
4. Check budgets, append to the WAL, repeat — until the model answers in
   plain text or a cap trips.

Six tools, policy-gated: `view` / `search` / `edit` / `write` are
workdir-anchored (symlink-safe, secrets-denied, bounded reads); `exec` /
`test` run only allowlisted commands (binary-prefix match, shell-syntax
guard, bounded output). Edits land in a git overlay: bad batches roll
back, the proven prefix survives, and the final deliverable is the
complete patch against the starting tree — a "done" run with an empty
diff is a failure, never exit 0.

The provider layer speaks the OpenAI chat protocol (DeepSeek, GLM, and
any compatible endpoint), retries through an 8-attempt ladder with hard
HTTP timeout ceilings, and meters exact token counts and dollars per
model — an unpriced model reports `null` cost, never zero.

## Benchmarks, graded honestly

- **Slices:** Terminal-Bench 2.1 frozen slice (21 tasks, IRT-selected,
  SHA-256-pinned), Multi-SWE-bench Rust slice (50 ids); SWE-bench
  Verified is calibration-only.
- **Taxonomy:** `Resolved | Unresolved | ErrorNoReport | EmptyPatch |
  InfraFailure | Ambiguous` — infra failures are never scored as
  capability, and broken instances are excluded loudly before any spend.
- **Statistics:** Wilson 95% intervals on every rate; paired-bootstrap
  comparisons seeded and reproducible without a `rand` dependency;
  `Flaky` / saturation flags instead of silent averaging.
- **Apply modes:** patch grading defaults to Strict (`git apply` only);
  `--lenient-apply` records Lenient provenance, and the two are never
  compared.
- **Two graded estimands, never mixed:** harbor-graded (file-copy) rows and
  in-container rows (verifier = the same live container the agent worked
  in; verdict from the task-written reward file) live in separate arms.
- **Self-describing reports:** every report carries a provenance
  block — binary git sha, the agent-leg budget recipe (token/step/action
  caps and the token-cap shape), apply mode, and the
  estimand label (`Ungraded` for plain eval, `HarborOracle` for
  `--agent`, `InContainerReward` for `--agent --in-container`) — so an
  arm is identified by its own file, not by driver-script convention.
- Missing credentials fail fast (exit 4) before a single token of spend.

## Repository layout

- `crates/rof` — CLI driver: wires the loop once, prints the patch,
  sets exit codes; `rof eval` runs the benchmark legs.
- `crates/agent-loop` — the loop: state, request building, verification
  nudge, proof gating, cancellation, termination order.
- `crates/tool-core` / `tools-std` — tool vocabulary + registry gate, and
  the six built-in tools with their containment policy.
- `crates/agent-budget` — step/action/token/spend/wall caps and halts.
- `crates/snapshot` — git-overlay baselines, rollback, the unbounded
  deliverable patch.
- `crates/provider-core` / `provider-openai` — `LlmClient` trait +
  OpenAI-compatible adapter with retry ladder and pricing.
- `crates/eval` — frozen slice loading, winnability, oracles, gate
  statistics, report honesty flags.
- `crates/agent-event` / `agent-log` / `trace` — event vocabulary,
  durable WAL, ordered sinks.
- `crates/context` — prompt selection: file map, pinned files,
  collapse, compaction checkpoints (off by default).
- `crates/bets` — three capability experiments, flagged off until
  measured.
- `docs/` — [`architecture.md`](docs/architecture.md),
  [`decisions.md`](docs/decisions.md) (decision log),
  [`roadmap.md`](docs/roadmap.md) (what's next),
  [`threat-model.md`](docs/threat-model.md).

## CLI reference

`rof run` — required: `--goal`, `--workdir`, `--model`. Common flags:

| Flag | Meaning |
|---|---|
| `--endpoint URL` | OpenAI-compatible base URL (env `OPENAI_BASE_URL`) |
| `--allow-cmd CMD` | allowlisted command (repeatable; binary-prefix match) |
| `--budget-steps/-actions/-tokens N` | caps; any explicit flag lifts the 900s wall to 3600s |
| `--max-tokens N` | per-request output ceiling |
| `--context-file PATH` | pinned file context (repeatable) |
| `--dump-events PATH` | full event log (one run, one dump) |
| `--log-path` / `--no-log` | WAL location (defaults to `<workdir>/.rof-events.jsonl`) |
| `--incentives base\|contract\|full` | prompt scaffold level |
| `--compaction FRAC` | context checkpointing (off by default, unvalidated) |
| `--keep-thinking`, `--thinking-keep N`, `--collapse-hysteresis N` | transcript-shape knobs |
| `--pass-env NAME`, `--header NAME:VALUE`, `--api-key-env NAME` | provider plumbing |

`rof eval` adds: `--tasks-dir DIR` (required), `--ids a,b`, `--out-dir`,
`--report PATH`, `--winnability-only`, `--lenient-apply`, `--agent`
(per-instance in-process runs), `--in-container` (agent inside the task
environment container). Without `--agent` it is a scaffold, not a
grader — it checks winnability and slice integrity only. Agent budgets
default to the frozen recipe: 300k tokens / 60 steps / 120 actions.

## Workdir contract

`rof run` mutates `--workdir` in place (`git init`, `git add -A`, tool
exec). The guard enforces exactly four refusals (exit 2); every other
directory — however valuable it looks — runs today under the operator's
responsibility:

- the filesystem root (`/`),
- `$HOME`,
- the harness checkout itself,
- a repo with uncommitted changes, unless `--allow-dirty-workdir` is
  passed (pre-existing dirt then appears in the reported patch).

## Threat model

- The path policy covers `view`/`edit`/`write` only: root-anchored,
  symlink-safe, secrets-denied file access.
- `exec` and `test` run allowlisted host commands, but allowlisting is
  not isolation: permitting `cargo test`, `sh`, or any test runner hands
  the agent arbitrary code execution on the host. Agent-spawned children
  run at the lowest scheduling priority (nice 19) so unattended sweeps
  yield to interactive work; memory stays uncapped.
- OS-level isolation (container, namespaces, no network, read-only
  mounts, memory caps) is the operator's job — the tools build no
  sandbox.

## Exit codes

0 done; 2 usage/parse; 3 run failure (halted, cancelled, provider
error); 4 missing credentials; 5 log failure; 6 snapshot failure;
7 input failure.

## Build / test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

CI runs all three on every push, plus a stale-reference grep over all
tracked files.

## License

Apache-2.0 — see [LICENSE](LICENSE).
