# rof-harness

Agent harness: an OpenAI-compatible model drives a tool loop (`agent-loop`)
against a scratch `--workdir`, with budgets, a snapshot tree, and an
evidence-bounded stdout patch as the deliverable.

## Layout

- `crates/rof` — CLI driver. Runs the loop, prints the **full** patch to
  stdout, exit code reflects the outcome. `--dump-events PATH` appends one
  JSON line per event (fsync'd, kill-resilient); the WAL defaults to
  `<workdir>/.rof-events.jsonl` (`--log-path` overrides, `--no-log` opts out).
- `crates/agent-loop` — the loop: `run()` is the shipped sequential driver
  (`drive_tick` is a test harness over the same step helpers). State,
  verification nudge, request building, proof gating, cancellation.
- `crates/tool-core` — tool vocabulary, registry gate, `ToolOutcome`
  (`success` carries the exit status; serde default false = fail-closed).
- `crates/tools-std` — policy-gated `view/search/edit/write/exec/test` tools.
- `crates/agent-budget` — step/action/token/spend caps, halts, `with_steps()`.
- `crates/snapshot` — git-overlay tree: baselines, rollbacks, bounded
  evidence patch + unbounded `patch_since_start_full()` deliverable.
- `crates/provider-core` / `crates/provider-openai` — `LlmClient` trait +
  OpenAI-compatible client (8-attempt retry ladder, priced models).
- `crates/eval` — slice loading, official-container verdicts, gate stats.
- `crates/agent-event` / `agent-log` / `context` / `bets` / `trace` / `verify`
  — events, durable log, compaction/file-map, proof gating, tracing, matching.

## Build / test

```sh
cargo build -p rof
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Scoped form per crate, e.g. `cargo test -p agent-loop`.

## Run

```sh
export OPENAI_API_KEY=...
cargo run -p rof -- --goal "fix ..." --workdir /tmp/w --model <id> \
  --endpoint https://... --allow-cmd "cargo test" --budget-steps 20
```

Useful flags: `--budget-actions/--budget-tokens/--max-tokens`,
`--context-file`, `--proof-cmd`, `--incentives base|contract|full`,
`--bets`, `--compaction`, `--dump-events`, `--log-path` / `--no-log`.

## Docs

`research/` is read-only input (briefs); decisions live in
`research/DECISIONS.md`. `AGENTS.md` states the build rules (ponytail,
contract-first parallelism, evidence discipline, no shared crates).
