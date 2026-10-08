# Architecture

Headless agent harness. One process wires the sibling crates together;
a model drives a sequential tool loop against a disposable scratch
directory.

## Process shape (`crates/rof/src/main.rs`)

1. Parse flags (`cli.rs`), validate `--workdir` (`workdir.rs`), fail fast
   on missing credentials before any spend (exit 4).
2. `snapshot::TreeService::ensure()` records the start HEAD and baselines.
3. `agent_loop::run(RunConfig, Input)` drives the loop; events go to the
   WAL (`agent-log`, append-only JSONL, fsync'd, fail-closed) and to the
   incremental `--dump-events` mirror (one run, one dump file at PATH).
4. Deliverable: the full, unbounded unified diff of the workdir against
   the start HEAD (`snapshot::patch_since_start_full()`) on stdout. A
   `Done` run with no recorded start HEAD is a failure outcome, never
   exit 0 with an empty patch. The 8KiB/200-line evidence bound is
   internal to the loop, not the deliverable.
5. Exit code from the outcome (`exit.rs`); a one-line event summary on
   stderr.

`rof eval --tasks-dir DIR` (`eval_cmd.rs`) grades a frozen task slice:
load instances, run the official-container verdict per task, aggregate
gate stats into a JSON report. Default patch-apply provenance is Strict;
`--lenient-apply` records Lenient instead. With `--agent` the per-instance
engine is the agent leg instead (`eval_agent.rs`): fresh seeded workdir,
the loop executed in-process through the same `execute()` wiring as
`rof run`, then the harbor CLI as the grading seam (job yaml +
`harbor run -c <job> -y -q`; spawn failure, grading timeout, workdir
refusal, and a run with no RunEnd in the events dump are infra failures,
never capability). The container default is unchanged.

With `--in-container` the per-instance shape changes (`eval_agent.rs`):
the task environment container (`Instance.image`, from task.toml
`docker_image`) is started before the agent run with the workdir
bind-mounted at `/app` and the task tests read-only at `/tests`
(cpus/memory capped at 2/4096m); the run's tool policy carries an exec
wrap (`docker exec <container>`) so exec/test tool calls land inside the
container while the allowlist and shell-op guard still see the unwrapped
argv, and file tools stay host-side on the bind-mounted workdir. The
verifier then execs the same live container (`/tests/test.sh`), so
installed packages and running services persist to grading; the verdict
is the verifier exit code — a new estimand, never mixed with the harbor
`result.json` reward. Every per-instance failure in this mode (missing
image, container start, verifier spawn/timeout) is infra, never
capability, and the container is removed best-effort on every path.

## Crate map

- `agent-loop` — the loop. `run()` is the shipped sequential driver;
  `drive_tick` is a test harness over the same step helpers. Owns state,
  verification nudge, request building, proof gating, cancellation.
  `RunConfig` carries goal/model/context/max-tokens/incentives/proof-cmd
  plus the fold knobs (`thinking_keep`, `collapse_hysteresis`) and the
  WAL path; the loop itself never reads process env.
- `tool-core` — tool vocabulary, registry gate, `ToolOutcome` (`success`
  carries the exit status; serde default false = fail-closed).
- `tools-std` — policy-gated `view/search/edit/write/exec/test` tools.
  The path policy (root-anchored, symlink-safe, secrets-denied) covers
  `view`/`edit`/`write` only.
- `agent-budget` — step/action/token/spend caps, halts, `with_steps()`
  (warn clamp + refund re-derive live there). `config_for` presets per
  capability; run budgets resolve in `cli::budget_for`.
- `snapshot` — git-overlay tree: baselines, rollbacks, bounded evidence
  patch, unbounded `patch_since_start_full()` deliverable.
- `provider-core` / `provider-openai` — `LlmClient` trait +
  OpenAI-compatible client (8-attempt retry ladder, priced models).
- `eval` — slice loading (`load_tb_slice`), official-container verdicts,
  gate stats.
- `agent-event` — the live event vocabulary (11 variants; pinned in
  `exit.rs::NAMES`). `agent-log` — the durable JSONL log. `context` —
  compaction/file-map. `bets` — three capability bets, feature-flagged.
  `trace` — ordered sink + live forward + JSONL append over the event
  vocabulary; token totals as passed-in counts only.

## Data flow

Model response -> parsed tool call -> registry gate -> tool exec (policy
checked) -> result folded back into context -> budget guard at the step
head -> WAL append (pre-effect, fsync) -> next request. The stdout patch
is computed once at the end from the snapshot tree.

## Budget defaults (`cli::budget_for`)

- Explicit `--budget-steps` / `--budget-actions` / `--budget-tokens` win.
- Any explicit budget flag also lifts the preset's 900s wall-clock cap to
  the LongTask 3600s allowance: the wall is the measured runaway guard for
  20-step default batches, never part of a flagged recipe (measured
  2026-10-08: v0 sweep cells ran 207–816s and all bound on the token cap;
  one 60-step/300k cell wall-halted at 24% of its token budget — three
  300s exec timeouts consume the 900s wall). Flag-less runs keep the
  preset wall.
- Pinned-context run (`--context-file`) with no token/step flags:
  200_000 token cap (floor justified in the code comment as
  chosen-to-validate; no measured anchor yet).
- Otherwise the UnattendedBatch preset (50k tokens, 20 steps, 30
  actions/trial).

## Not inferable from code

- Why the compaction trigger (`--compaction`) remains off-by-default and
  unvalidated, and when its experiment is planned.
- Why `FailureKind::Provider` keeps the historical exit code 3 instead of
  a dedicated one.
- The intended scope/timeline of Bet C (candidate racing); the code only
  records it as flagged and unbuilt.
- The roadmap for pricing new models in `provider-openai`.
