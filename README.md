# rof-harness

Agent harness: an OpenAI-compatible model drives a tool loop (`agent-loop`)
against a scratch `--workdir`, with budgets, a snapshot tree, and the full
(uncut) run patch on stdout as the deliverable.

## Layout

- `crates/rof` — CLI driver. Runs the loop, prints the **full** patch to
  stdout, exit code reflects the outcome. `--dump-events PATH` writes one
  JSON line per event (one run, one dump file); the WAL defaults to
  `<workdir>/.rof-events.jsonl` (`--log-path` overrides, `--no-log` opts out).
  `rof eval` grades a frozen task slice into a JSON report; with `--agent`
  each instance instead runs the agent leg: a fresh workdir
  `<out-dir>/<id>/work` seeded from `<tasks-dir>/<id>/environment`
  (or left empty for can_create tasks), the goal built from the instruction
  (`/app/` stripped, bare `/app` → "the workdir") plus the frozen pilot
  rules tail, the run executed in-process with the same wiring as
  `rof run`, then graded via the harbor CLI (`harbor run -c <job> -y -q`):
  reward 1.0 → Resolved, 0 → Unresolved, missing → ErrorNoReport. Agent
  infra failures (workdir refusal, missing harbor, grading timeout, a run
  with no RunEnd in the events dump) never score as capability;
  packaged files exclude top-level `.git`/`__pycache__`/`.pytest_cache`,
  any-depth `*.pyc`, and the WAL sidecar (driver parity: first component
  only, so a nested repo a task builds still reaches the container). With
  `--in-container` the agent works inside the task environment container
  (workdir bind-mounted at `/app`) and the verifier execs the same live
  container instead of grading a fresh copy via harbor.
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
  Strict patch-apply grading by default; `--lenient-apply` records Lenient
  provenance instead (Strict grades are not comparable with earlier
  fuzz-lenient numbers).
- `crates/agent-event` / `agent-log` / `context` / `bets` / `trace`
  — events, durable log, compaction/file-map, proof gating, tracing.

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
`--bets`, `--compaction`, `--dump-events`, `--log-path` / `--no-log`,
`--thinking-keep` / `--collapse-hysteresis` (env fallback `THINKING_KEEP` /
`COLLAPSE_HYSTERESIS`, flag wins), repeatable `--pass-env NAME`,
`--keep-thinking` (skip the DeepSeek-specific thinking-off request body),
`--allow-dirty-workdir`. `rof eval --tasks-dir DIR [--lenient-apply]`
grades the slice; `--lenient-apply` records Lenient patch-apply provenance
in the report (default Strict).

`rof eval --agent` runs the agent leg instead of the oracle-container leg.
Required: `--model ID`, `--endpoint URL` (or `OPENAI_BASE_URL`; flag wins).
Optional: `--api-key-env NAME` (default `OPENAI_API_KEY`), repeatable
`--header NAME:VALUE` (a colon-less value is a parse error), repeatable
`--allow-cmd CMD` (same binary-prefix semantics as `rof run`),
`--budget-steps/--budget-actions/--budget-tokens/--max-tokens` (agent
budgets default to the frozen pilot recipe: 300000 tokens / 60 steps /
120 actions), `--keep-thinking` (skip the thinking-off request body),
`--in-container` (run the agent inside the task environment container).
Mutually exclusive with `--winnability-only`.

With `--in-container`, the task environment container is started before
the agent run (workdir bind-mounted at `/app`, cpus/memory capped at
2 / 4096m); agent exec/test calls run inside it via `docker exec`, and
the verifier execs the same live container — so installed packages and
running services persist to grading, and the env-mutation task class
(installed-by-the-agent imports, a live server) becomes winnable.
Verdict provenance in this mode is the verifier exit code — a NEW
estimand, never mixed with harbor-graded rows (the harbor path reads
the `result.json` reward). Missing image, container-start failure, or
verifier spawn failure/timeout are infra failures, never capability.

Missing credentials fail fast before any spend: exit 4, naming the env var
(`--api-key-env NAME` selects which var holds the key).

## Workdir contract

`rof run` mutates `--workdir` in place (`git init`, `git add -A`, tool
exec). The guard enforces exactly the four refusals below (exit 2); every
other directory — however valuable it looks — is run today under the
operator's responsibility (see Threat model: the tools build no sandbox).
Refused with exit 2:

- the filesystem root (`/`),
- `$HOME`,
- the harness checkout itself,
- a repo with uncommitted changes, unless `--allow-dirty-workdir` is passed
  (pre-existing dirt then appears in the reported patch).

## Threat model

- The path policy covers `view`/`edit`/`write` only: root-anchored,
  symlink-safe, secrets-denied file access.
- `exec` and `test` run allowlisted host commands, but allowlisting is not
  isolation: permitting `cargo test`, `sh`, or any test runner hands the
  agent arbitrary code execution on the host. Agent-spawned host children
  run at the lowest scheduling priority (nice 19): they yield to
  interactive work but still get the full idle CPU, so unattended sweeps
  do not make the desktop unusable during builds. Memory stays uncapped.
- OS-level isolation (container/namespace, no network, read-only mounts,
  memory caps) is the operator's job; the tools build no sandbox.

## Exit codes

0 done; 2 usage/parse; 3 run failure (halted, cancelled, provider error);
4 missing credentials; 5 log failure; 6 snapshot failure; 7 input failure.
