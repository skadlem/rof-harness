# Architecture

Headless agent harness: one process wires the sibling crates; a model drives a
sequential tool loop. `rof run` uses a disposable host-side workdir, while the
native Harbor adapter runs ROF in the task's live `/app` container (current
benchmark path).

Experimental capability controls are in `docs/harness-experiments.md`. The
default registry exposes the original six tools; optional tools add
archived-output retrieval, shell commands, pipe sessions, literal repository
lookup and multi-file edits. Loop controls independently vary workflow,
observation retention, failed-check preservation and repeated-failure notices.
The WAL retention pilot produced one clean pass on one selected task; a React
retention artifact also received reward 1 during an interrupted attempt whose
provider accounting was incomplete. Neither establishes general capability
improvement, and the other experimental tool mechanisms remain unmeasured on
benchmark tasks and opt-in. Review/repair uses fresh contexts, read-only
reviewer tools, separate excluded WALs and the same cumulative budget/deadline.
Procedure files load from frozen, hashed bytes and are selected against the
original task; offline distillation and two-candidate helpers live in `tools/`.

The [phased capability plan](superpowers/plans/2026-11-11-capability-phases.md)
records the sequence. Phases 0–2 add offline paired reporting, Harbor controls,
opt-in repository evidence and an opt-in public completion predicate. Historical
observation restoration, explicit capacity controls and checkpoint provenance are
implemented, default off; their need and benchmark benefit remain unestablished.
The completion predicate uses the policy-checked `test` dispatch (shared budget,
one repair continuation/two checks), not the host proof executor or the hidden
native verifier, and cannot combine with the review stage.

## Process shape (`crates/rof/src/main.rs`)

1. Parse flags (`cli.rs`), validate `--workdir` (`workdir.rs`), fail fast on
   missing credentials before any spend (exit 4).
2. `snapshot::TreeService::ensure()` records the start HEAD and baselines.
3. `agent_loop::run(&mut LoopState, Run, Vec<Input>, &CancellationToken)`
   (`agent-loop/src/run.rs`) drives the loop; events go to the WAL (`agent-log`,
   append-only JSONL, fsync'd, fail-closed) and to the incremental
   `--dump-events` mirror (one run, one dump file at PATH).
4. Deliverable: the full, unbounded unified diff against the start HEAD
   (`snapshot::patch_since_start_full()`) on stdout. A `Done` run with no
   recorded start HEAD is a failure outcome, never exit 0 with an empty patch.
   The 8KiB/200-line evidence bound is internal to the loop, not the
   deliverable.
5. Exit code from the outcome (`exit.rs`); one-line event summary on stderr.

`rof chat` (`chat.rs`) is opt-in and interactive: one `agent_loop::run` per
stdin line over a single `LoopState` (`reset_for_input` clears per-run latches,
`persisted` keeps the log cursor), an emitter listener printing assistant text
and one line per tool call. `--resume LOG` (also on `rof run`) rebuilds history
via `LoopState::restore`, which synthesizes results for unanswered tool calls
and closes an open turn as interrupted.

`--stream on` wraps the provider in `StreamPrint` (`main.rs`): text deltas from
`LlmClient::complete_streaming` go to stdout (chat) or stderr (run); the loop,
its events and the returned `Response` are unchanged. `--mcp` calls
`tools_std::register_mcp_stdio` per server after `registry_for`; those tools
ride an `mcp__*` prefix grant in `GrantGate` and need `ExecPolicy::Any`.
Host-side `write`/`edit` replace a file they cannot open for writing when they
own its directory (`common::overwrite`), because in-container commands run as
root and leave root-owned outputs in the bind-mounted workdir.

`rof eval` and `crates/eval` (the legacy host/in-container evaluation driver)
were retired in favor of the native Harbor adapter (`tools/harbor_rof.py`),
which runs `rof run` directly inside each task container. Historical reports and
implementation remain in git history (through `41393ae`).

### Harbor adapter (`tools/harbor_rof.py`)

The current benchmark path.

- Uploads the frozen runtime bundle and runs `rof run` inside the task container
  with `/app` as workdir and the task instruction as `--goal`, verbatim: no
  `GOAL_TAIL`, no `/app` rewrite (eval-only). Always passes `--exec-policy any`,
  `--steer off`, `--streak-steer off`, `--keep-thinking`,
  `--allow-dirty-workdir`.
- Budget defaults 300,000 tokens, 60 steps, 120 actions, `--max-tokens 8000`;
  the TB 4.1 pilot overrode them to 1,200,000 / 120 / 240
  (`current-benchmark-pilot.md`), so a default adapter run does not reproduce
  the pilot. Any budget flag sets ROF's own wall to 3,600 s, below the task's
  native agent allowance.
- WAL at `/logs/agent/rof/.rof-events.jsonl`, outside the workdir; stdout (the
  patch) is discarded — Harbor grades container state.
- Only rof exit codes 0, 3 and 8 are graded trials; any other code (or a missing
  exit marker) raises a trial failure, so a panic (101) or kill (137) is infra.
- Token and cost totals are the last `TurnEnd.usage_totals`; after a
  `*-usage-unknown` error they are withheld and `metadata.usage_unknown` is set,
  matching `rof eval`. `cost_usd` uses the PEAK price book, an upper bound.
- `version()` is `runtime-bundle-<first 16 hex of the rof binary's SHA-256>`
  (`runtime-bundle` alone if the binary is unreadable).
  Opt-in `exec_output` / `rollback_scope` map to `--exec-output` /
  `--rollback-scope`.

### Agent tree (`crates/rof/src/agents.rs`, `--agents FILE`)

Stage 1 of a configurable topology, off by default and unmeasured. A JSON file
declares roles (tools, optional prompt, token/step budget), limits (`max_depth`,
`max_agents`, `child_budget_tokens`) and `spawn` edges.

- A role with an outgoing edge, below `max_depth`, gets `spawn {role, brief}`
  (returns `id=a<n>` at once) and `wait {ids}` (blocks, returns each child's
  final answer or failure).
- A child is one `agent_loop::run` on a fresh `LoopState`, with only its role's
  tools (same policy, workdir and exec wrap as the parent), a log at
  `<parent log>.<id>`, and a per-child stderr line.
- Children are futures polled next to the root's run (`with_children`):
  concurrent with each other and the root, borrowing the provider, dropped if
  still running when the root ends. The child pool is carved from
  `--budget-tokens` before the root starts; unused child tokens are not
  returned.
- `workspace: "worktree"` and `message` edges parse but are rejected at load;
  stage-1 children share the parent's workdir read-only by tool choice (no
  `write`/`edit`), not by a filesystem boundary.
- `--agents` conflicts with `--review on` and is not wired into `rof eval`.

## Crate map

- `agent-loop` — the loop. `run()` is the shipped sequential driver. Owns state,
  verification nudge, request building, proof gating, cancellation. The
  verification nudge, half/late action directives and wrap-up nudge run only
  with `--steer on`; `steer` defaults off on `rof run` and `rof eval`, the
  Harbor adapter forces it off (only `RunConfig::default()`, used by library
  tests, has it on). Any failed call in a tool batch (denial, invalid args, tool
  error, non-zero exit) rolls the whole batch back and queues the rollback
  notice; calls after the failing one still run first. `--rollback-scope
  errors` (off by default, unmeasured) exempts calls that ran to completion and
  reported failure, and skips rollback and notice when the batch changed no
  file. `RunConfig` carries goal/model/context/max-tokens/incentives/proof-cmd,
  the fold knobs (`thinking_keep`, `collapse_hysteresis`) and the WAL path; the
  loop never reads process env. `--streak-steer` opts in to the unvalidated
  six-read directive, recording crossings for control and treatment.
  `--deliverable PATH` logs named outputs still missing at step 15 (shadow);
  `--deliverable-steer on` adds one unvalidated directive.
- `tools-std` — policy-gated `view/search/edit/write/exec/test`. `exec` splits
  with POSIX quoting (no shell) and appends `[exit N]` to a non-zero exit, since
  `success` is not shown to the model; the marker is added before the
  8,000-character head cut, so long output loses it, archived exec
  (`--archive-output on`) never adds it, and archived `test` output is not
  condensed. Children get a cleared environment (nine names plus `--pass-env`).
  `--exec-output head-tail` (off by default, unmeasured) puts `[exit N]` first,
  keeps head and tail around an omission marker within the 8,000 characters, and
  returns partial output on a 300 s timeout. Under `--exec-policy any` the exec
  and test descriptions no longer say "allowlisted". The path policy checks
  containment and requested/resolved paths against secrets globs, including
  recursive search descendants, and resolves a leading `/app/` to the tool root.

| crate | contents |
|---|---|
| `tool-core` | tool vocabulary, registry gate, `ToolOutcome` (`success` carries the exit status; serde default false = fail-closed). |
| `agent-budget` | step/action/token/spend caps, halts, `with_steps()` (warn clamp + refund re-derive); `config_for` presets per capability, run budgets resolve in `cli::budget_for`. |
| `snapshot` | git-overlay tree: baselines, rollbacks, bounded evidence patch, unbounded `patch_since_start_full()` deliverable. |
| `provider-core` / `provider-openai` | `LlmClient` trait + OpenAI-compatible client. Separate retry bounds: cold-start 503 retries use eight attempts, ordinary retryable errors five, and the truncation output-token ladder its own bounded escalation (`provider-openai/src/retry.rs`). |
| `agent-event` / `agent-log` | live event vocabulary (11 variants; pinned in `exit.rs::NAMES`); durable JSONL log. |
| `context` | compaction/file-map. |
| `bets` | capability bets A/B (claim + proof-gated commits), feature-flagged. |

## Data flow

Model response -> WAL records assistant and tool calls (fsync) -> registry gate
-> tool exec (policy checked) -> WAL tool result (fsync) -> next request's
derived context. The budget guard admits provider steps and checks the action
cap before each tool in a batch. The stdout patch is computed once at the end
from the snapshot tree.

## Guarantee limits and runner scope

The host/file-copy and `--in-container` limits below describe the legacy
evaluation rig, with their original measurement dates. In the native Harbor
adapter path the whole ROF process, including file tools and snapshots, runs
inside the task container.

- By default the CLI archives the previous WAL before startup and constructs
  fresh state. `--resume LOG` (run and chat) reads the log with `read_log` and
  rebuilds history through `LoopState::restore`; `history_read` also reads the
  log. Automatic crash resume is not built.
- Spend accumulates before conversion to cents. Provider calls, tool execution
  and proof commands observe the run wall clock. In-flight provider usage may be
  unknown on timeout or cancellation; blocking Git operations can extend the
  deadline.
- Rollback restores Git-managed batch state, not installed packages, services or
  ignored files. File tools and snapshots run on the host; in-container
  exec/test run in the container; optional proof commands run on the host.
- Incremental proof rejects a truncated evidence patch before restoring any
  prefix. Exec timeout and cancellation cover pipe draining; in-container
  descendants may outlive a killed docker-exec client.
- Verifier artifacts are cleared before grading; verifier infrastructure
  failures take precedence over a reward. `/logs` stays writable during the
  agent phase, and tests are copied in immediately before grading.
- Reports record nonsecret model, endpoint, command allowlist, output-token limit
  and thinking setting. Each row records a SHA-256 task snapshot digest and
  live-container image ID when available. Symlinks and special inputs leave task
  identity unknown; Harbor-owned image identity is unknown.

## Budget defaults (`cli::budget_for`)

- Explicit `--budget-steps` / `--budget-actions` / `--budget-tokens` / `--budget-spend-cents` win.
  `--budget-tokens 0` uncaps cumulative tokens (the budget line omits the token segment);
  `--budget-spend-cents 0` disables the spend cap (`--max-tokens 0|omit` omits `max_tokens` on the
  wire so the endpoint uses its native default; the overflow retry ladder raises up to
  `MAX_TOKENS_CEILING = 393_216`).
- Any explicit budget flag lifts the preset's 900s wall-clock cap to the
  LongTask 3600s allowance: the wall is the measured runaway guard for 20-step
  default batches, never part of a flagged recipe (measured 2026-10-08: v0 sweep
  cells ran 207–816s and all bound on the token cap; one 60-step/300k cell
  wall-halted at 24% of its token budget — three 300s exec timeouts consume the
  900s wall). Flag-less runs keep the preset wall.
- Pinned-context run (`--context-file`) with no token/step flags: 200_000 token
  cap (chosen-to-validate in the code comment; no measured anchor yet).
- Otherwise the UnattendedBatch preset (50k tokens, 20 steps, 30
  actions/trial).
- An armed `$` cap whose model has no price-book entry is a *configuration
  error*: `cli::budget_for` returns `Err` and the run exits 7 before any
  provider request, because an unpriced model reports no cost and the cap could
  never trip. `--allow-inert-spend-cap` (run and eval, default off) opts out and
  names the inert cap once; the choice lands in the agent leg's eval report
  `run_config`.

## Not inferable from code

- Whether compaction improves coding capability: its trigger uses explicit
  context capacity/output reserve and optional source-labelled summaries, but
  efficacy is unmeasured. Observed prompt pressure is still required before
  scheduling its benchmark experiment; implementation tests do not establish it.
- Why `FailureKind::Provider` keeps the historical exit code 3 instead of a
  dedicated one.
- The intended scope/timeline of Bet C (candidate racing); the code records only
  automatic runtime racing, flagged and unbuilt. The explicit
  `tools/rof_candidates.py` helper exists and is a different mechanism.
- The roadmap for pricing new models in `provider-openai`.
