# rof — a local agent harness with a meta-harness

A self-contained Rust agent runtime. It owns its orchestration, context handling, tools and
evaluation; only model inference goes to an external API. Two logical models are used:

- **Context LLM** — planning, context compaction (cheap tier)
- **Executor LLM** — tool-using implementation and review (strong tier)

Orchestration is `Planner -> (Implementer -> Reviewer)*`, bounded by rounds and a per-task token
ceiling, with every model call, tool call, verdict and budget abort written to a durable JSONL
trace that the eval layer folds into metrics.

## Build and run

```bash
cargo build --release                 # single binary: target/release/rof
./target/release/rof "add a doc comment to src/lib.rs"      # run one goal
./target/release/rof eval eval/suites/repo-tasks.json       # run a suite
./target/release/rof eval eval/suites/repo-tasks.json --limit 6 --jobs 2   # first 6, two at a time
./target/release/rof config > my.json                       # effective config (diffable)
./target/release/rof --config my.json eval suites/repo-tasks.json
```

Live models need credentials in the environment (`OR_TOKEN` for OpenRouter, or
`DEEPSEEK_API_KEY`/`ROF_TOKEN` with `ROF_CHAT_BASE` for a direct OpenAI-compatible endpoint).
Without them the harness runs against a deterministic stub, which is how the tests exercise the
full loop offline.

**Always point `ROF_WORKDIR` at a scratch copy** — the harness edits the working directory:

```bash
WD=$(mktemp -d); cp -r src tests eval Cargo.toml "$WD"/
ROF_WORKDIR="$WD" ROF_ALLOW_CMDS="cargo test" ROF_TRACE=/tmp/run.jsonl \
  ./target/release/rof eval eval/suites/repo-tasks.json
```

A suite run never touches `ROF_WORKDIR`: every task runs in its own copy
(see Fan-out), so an eval cannot edit the tree it is measuring.

## Environment knobs

| Variable | Meaning |
|---|---|
| `ROF_CONFIG` | config file path (same as `--config`) |
| `ROF_WORKDIR` | target tree, copied per task; the permission allowlist is anchored here |
| `ROF_CTX_MODEL` / `ROF_EXEC_MODEL` / `ROF_EXEC_FALLBACK` | model routing without recompiling |
| `ROF_ALLOW_CMDS` | comma-separated exact command allowlist (empty = deny all) |
| `ROF_ALLOW_HOSTS` | comma-separated host allowlist for `http.get` (empty = deny all) |
| `ROF_EXPECT_WRITES` | `yes`/`no` — run mode: must the goal change a file to pass (default yes) |
| `ROF_PLANNER` | `always` (default) or `skip` (treat every goal as one task) |
| `ROF_MAX_ROUNDS`, `ROF_MAX_TASK_TOKENS`, `ROF_COST_LAMBDA` | loop and scoring knobs |
| `ROF_TRACE` | JSONL trace path (append-only, one line per event) |
| `ROF_CHECK` | run-mode checks, comma-separated exact commands |
| `ROF_JOBS` / `ROF_MAX_PARALLEL_TASKS` | suite fan-out (`--jobs N` wins over both) |
| `ROF_TASK_ROOT` | where per-task workdir copies live (default: system temp dir) |
| `ROF_CLEAN_TASKS` | `yes` deletes a task's copy when the task finishes |

Precedence: defaults < config file < environment < CLI flags. `rof eval <suite> --report out.json`
writes the suite report (per-task verdicts + feedback, folded metrics) next to the trace, so a
baseline survives the terminal it was printed to.

## Fan-out

`rof eval <suite> --jobs N` (or `max_parallel_tasks` in config) runs up to N tasks at once,
and `--limit K` runs only the first K tasks — a baseline without a second suite file to drift.

Isolation is by copy: each task gets its own scratch copy of the workdir under
`task_root`, so two writers never share a tree and the source tree is never a target.
`target/` and `.git/` are excluded from the copy (rebuildable, and history the agents must
not see). Results are recorded in suite order regardless of finish order, and a copy that
fails to be made fails only its own task.

Disk is the price of that isolation: a task whose checks build owns its own `target/`
(~250 MB for `cargo check`, ~1 GB for `cargo test` on this repo). Point `ROF_TASK_ROOT` at
disk rather than a small tmpfs, and set `ROF_CLEAN_TASKS=yes` for long suites. Copies that
are kept are how a MISMATCH is inspected after the fact.

## Config files

`rof config` prints the effective configuration as canonical JSON; saving it produces a file that
loads back identically, so configs can be versioned, diffed and A/B'd like any other artifact.
Partial files are valid — every unstated field falls back to its default. Examples:
`configs/default.json` (full dump), `configs/cheap.json` (cheap tier, planner skipped),
`configs/strict.json` (deny-by-default allowlists).

## Suites

JSON, one file per suite: `{name, tasks:[{name, goal, checks[], expect_writes, expect_pass, max_tokens?}]}`.
Each task runs the full loop against an isolated trace fork; results are matched per task
(`passed == expect_pass`), never per verdict, so retries cannot inflate the count.
`eval/suites/repo-tasks.json` holds 20 real tickets against this repo.

## Security model

Deny-by-default, enforced inside `ToolRegistry::call` (not in agents):

- `(agent, tool)` grant matrix — planner reads only; reviewer reads and runs checks; implementer writes.
- Path containment under the workdir, component-wise after lexical `..` normalisation.
- `proc.run` executes exact allowlisted strings only, with no shell.
- `http.get` requires an exact host allowlist match, refuses non-http(s) schemes, and does not
  follow redirects (a redirect would bypass the allowlist).
- `fs.patch` refuses ambiguous or missing search strings rather than guessing.
- The reviewer cannot write; a pass with no writes is rejected by the harness itself, not only by
  the reviewer prompt.

## Layout

```
src/engine/   session, orchestrator (supervisor loop), router (Context vs Executor)
src/agents/   planner, implementer, reviewer — one file per role behind the Agent trait
src/context/  layered state (long/mid/short), budgeted builder, keyword retriever
src/tools/    registry + permission gate + fs.list/read/write/patch, proc.run, http.get
src/llm/      LlmClient trait, Context/Executor services, OpenAI-compatible client
src/eval/     suite loader, runner, trace-folded metrics
src/obs/      trace events + append-only JSONL sink
src/config/   budgets, routing, permissions, pricing as versioned structs
```

## Testing

```bash
cargo test && cargo clippy --all-targets && cargo fmt --check
```

Tests default to `std::env::temp_dir()` and clean up the task copies they make. Live runs leave
copies under `ROF_TASK_ROOT` (the temp dir by default) — they are disposable; delete them or set
`ROF_CLEAN_TASKS=yes`. See `docs/STATUS.md` for what is done, what is left, and how to verify each claim.
