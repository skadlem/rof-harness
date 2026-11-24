PREMISES:
  problem: A headless coding-agent harness driving an OpenAI-compatible model through a tool loop against a repository until the task's own verifier passes. It exists to answer one question with evidence: does this harness make a given model solve more pinned external coding tasks than rival harnesses do with the SAME model, without spending more than 1.3x the matched reference's tokens per solved task. "Solved" is the task-written native reward inside the task container, never a model's own claim. Users: one owner-operator running batch campaigns, not interactive use. "Solved" for the product means a repeatable solve-rate edge at matched model and matched spend.
  rationale: The design's argument is that a harness's contribution is not prompt cleverness but three properties most harnesses lack: (1) grading in the task's live container, so installed packages, running services and migrated state survive from agent phase to verifier (docker exec for exec/test, host-side file tools on a bind-mounted /app); (2) a durable, fail-closed record written before each effect, so the trace is authoritative rather than reconstructed from prompts; (3) budget accounting that meters real provider usage including failed retry attempts, so cost claims are measurable rather than estimated. What would show it is true: the in-container path lifting an env-mutation task family from a structural 0/8 under file-copy grading to 8/8 in-container at $0.11 for eight cells (already observed once). Everything else in the system is meant to be an ablation candidate gated off by default.
  peak_load: UNKNOWN as a service-rate figure and not applicable by design - there is no server, no QPS and no multi-tenant surface. The unit of load is one process: `rof run` (one model conversation, one workdir, one task container) or one container host at a time. A campaign is <=10 sequential instances on one host. The only concurrency observation is a 5-way parallel worker sweep that died 0/10 to provider request timeouts and forced sequential workers thereafter; whether that was rate limiting or client contention is UNKNOWN.
  data_volume: Per run: one JSONL WAL at <workdir>/.rof-events.jsonl, one optional --dump-events mirror, one git baseline commit per dispatch, and a final unbounded unified diff on stdout. Observed scale: 14-47 assistant messages per run before a cap trips; a corpus of 99 run dumps plus 97 workdirs fit on one host with ~3.6 GB free disk at the last campaign. Retention: indefinite and manual; the CLI archives (does not delete) the previous WAL at startup, and no growth policy exists. WAL bytes per run is UNKNOWN - not measured in this review.
  latency_budget: Three separate clocks, deliberately not conflated. Agent phase per task: task.toml [agent] timeout_sec (e.g. 1800s; 900s query-optimize, 3600s video-processing), cancelled through the Ctrl-C token so a RunEnd is still emitted and the workdir still graded. Verifier: task.toml [verifier] timeout_sec, separate. Internal pilot wall: 900s preset, lifted to 3600s the moment ANY explicit budget flag is passed (crates/rof/src/cli.rs::budget_for). No interactive latency target exists; the nearest thing to a p99 is "a full task attempt completes or is killed inside its task.toml allowance".
  topology: Single host per campaign, no regions, no replicas, no service mesh; one machine is the blast radius. Per instance: a task container (docker, cpus/memory capped 2/4096m) started from the task image, host workdir bind-mounted at /app, harbor CLI invoked as a subprocess for the job YAML and grading seam. Alternative default path: a host-side disposable workdir with no container. Model over HTTPS to an OpenAI-compatible endpoint (DeepSeek direct, or opencode-go with a required per-conversation session header).
  team: One owner-operator (writes code, runs campaigns, holds the spend ledger). No on-call rotation, no second code reviewer. Spend is a hard ledger: $2.82 total ceiling, $2.519179098 upper bound consumed as of the last recorded campaign.

DECISION: >
  Is this shape — one sequential tool loop, a six-tool gated registry, a
  git-overlay snapshot with per-batch rollback, a fail-closed WAL, and a
  cumulative-token budget cap — the right vehicle to close the measured
  capability gap, or is the gap structural to the shape, so that effort should
  move off the mechanism surface (compaction, review, repo-lookup, completion
  check, procedure files, bets A/B/C, retention knobs) and onto something the
  current design holds fixed?

OBJECTIVE:
  1. Native reward (solved tasks) on pinned external tasks. This is the only
     capability estimand.
  2. Dollars, as a guardrail, not a target: <=1.3x the matched reference's
     tokens per solved task; a hard $ ceiling for discovery.
  3. Reproducibility of a claim: every row carries binary sha, task snapshot
     digest, config and usage, so an arm is identified by its own report file.
  4. Loses: elegance, generality, feature coverage, latency, and any ability
     the repository cannot measure.

INVARIANTS:
  - The WAL append happens before the effect it records, and an append failure
    hard-fails the run. Reference (log is the truth, prompt is a view):
    crates/agent-loop, crates/agent-log.
  - A deliverable is the full unbounded diff against the run's START HEAD
    (`snapshot::patch_since_start_full()`), computed once at run end. A `Done`
    outcome with no recorded start HEAD is a failure outcome, never exit 0 with
    an empty patch.
  - Rollback restores Git-managed tree state for ONE batch only; earlier batches
    survive. It does not restore installed packages, running services, or
    ignored files.
  - Budget admission happens before provider spend, and the action cap is checked
    before each tool in a batch. In-flight provider usage may be unknown on
    timeout or cancellation (accepted leak).
  - A model's completion claim is checked against observed evidence; the harness
    does not accept a claim as proof.
  - The task's hidden tests are never mounted during the agent phase; they are
    copied in immediately before grading (`docker cp` then `bash /tests/test.sh`).
  - The verdict is the task-written reward file; the verifier exit code is only
    the no-artifact fallback, because every slice `test.sh` ends with
    `echo ... > reward.txt` and therefore always exits 0 (measured 2026-10-12;
    exit-code grading had reported 25/26 where the reward files said 6/26).
  - Production defaults do not change on unmeasured mechanisms; a mechanism that
    measured as a no-op stays flagged and off.
  - Every durable row is written and fsynced BEFORE its live event frame is
    emitted (`record` = append -> sync -> emit, run.rs:643-651), so a consumer of
    the live stream can never see an event the log does not hold.
  - The first terminal reason wins. `stick_turn_reason` never downgrades a later
    attempt to a milder cause (state.rs:542-547), and every terminal site routes
    through `terminate`.
  - Interruption makes cost UNKNOWN, not zero: any run with a request in flight
    sets `cost_usd = None` and emits an explicit `provider-usage-unknown` /
    `wallclock-usage-unknown` frame (run.rs:872-920).
  - The prompt head is byte-frozen per run: the file map and named-file pins are
    read once at run start and reused; a pin whose file changed is DROPPED rather
    than re-read. The only per-request bytes are the budget line, the hold and
    the streak row, and none of those are persisted (request.rs:184-337).
  - Reported totals are always gross, even when the cap counts
    `MissPlusOut`; the cap shape and the reported number are deliberately
    different quantities (state.rs:516-540).

COMPONENTS (crate -> one line of responsibility):
  - `crates/agent-loop` (19 files, 11.5k lines) — the loop: state, request
    building, step dispatch, verification nudge, proof gating, cancellation.
  - `crates/rof` (11 files, 7.6k lines) — the binary: CLI flags, budget presets,
    workdir validation, exit codes, the eval engine and the in-container path.
  - `crates/tools-std` (13 files, 6.1k lines) — the policy-gated tools:
    view/search/edit/write/exec/test, plus optional shell/pipe/archive/repo-lookup.
  - `crates/tool-core` — tool vocabulary, registry gate, `ToolOutcome`
    (serde-default false = fail-closed).
  - `crates/provider-openai` (7 files) — the OpenAI-compatible client: retry
    ladder, truncation escalation, pricing table, usage parse.
  - `crates/provider-core` — the `LlmClient` trait and usage types.
  - `crates/agent-budget` — step/action/token/spend caps, halts, preset configs.
  - `crates/snapshot` (5 files) — git-overlay tree: baselines, rollback,
    hunk restore, bounded evidence patch, unbounded deliverable patch.
  - `crates/context` (8 files) — compaction, file map, collapse/window assembly.
  - `crates/bets` (5 files) — the three capability bets, feature-flagged, default
    `NoBets`.
  - `crates/agent-log` — the durable JSONL WAL (append, fsync, torn-tail repair).
  - `crates/agent-event` — the live event vocabulary, 11 variants, pinned.
  - `crates/eval` (8 files) — slice loading, winnability gate, gate stats,
    paired bootstrap, report flags.
  - `crates/trace` — ordered event sink and live JSONL forward.

RELATIONS:
  - `crates/rof` -> `agent_loop::run` : in-process async fn, called at most twice
    per process (primary run, then an optional review/repair run); each stage
    re-mints `LoopState` and carries only `budget`/`usage_totals`/`turn` across,
    so the durable `items` WAL does not cross stages (rof/src/main.rs:244-275).
  - `agent-loop` -> `provider-core::LlmClient` : in-process, STATIC dispatch
    (`P: LlmClient`, not a trait object), async. Guarantee: at-least-once per
    logical step — the adapter's own retry ladder runs inside one `complete`,
    and the loop adds `STEP_RETRY_BUDGET=2` in-step retries outside it
    (state.rs:23). There is no idempotency key on the wire, so a retried request
    can duplicate provider work; both attempts' usage is billed when reported.
  - `agent-loop` -> `tool-core::Registry` : in-process, sync, and the loop has NO
    dependency on `tools-std` (it sees only `tool-core` vocabulary + the
    `Arc<dyn Tool>` trait object). `Registry::prepare` is the one door: gate,
    resolve, strict-validate, prepare; deny, `Ask` (no approver exists, so it
    always denies), unknown tool and invalid args all become bounded, model
    visible error results and are never dispatched (tool-core/src/registry.rs:42-74).
  - `agent-loop` -> `Tool::execute` : `Arc<dyn Tool>` trait object, async,
    at-most-once per prepared `Invocation` (one call site, no retry). A
    cancellation drops the future mid-effect, so the effect's state after a
    cancel is UNKNOWN by construction (run.rs:1352-1385).
  - `tools-std` -> OS subprocess (exec/test) : subprocess, at-most-once, sync
    with a 300s deadline and `kill_on_drop` process-group SIGKILL. `exec` is the
    only path that can mutate the environment; under in-container eval the argv
    is wrapped `docker exec <container>` while the allowlist and shell-operator
    guard still see the unwrapped argv. A killed `docker exec` client does not
    guarantee the in-container command dies.
  - `agent-loop` -> `snapshot::TreeService` : sync calls dispatched on
    `spawn_blocking` and deliberately never raced against cancellation, so an
    in-flight git rollback completes even while the run is cancelling
    (run.rs:440-457). One git subprocess per operation, no retry, error
    propagated. `baseline()` commits the tree the next attempt starts from;
    `rollback()` restores tracked files and `clean -fdq`s untracked ones
    (git.rs:136-156); `patch_since_start_full()` diffs against the START HEAD,
    never the index, because a per-batch baseline commit makes the index-based
    diff read empty (git.rs:159-176).
  - `agent-loop` -> `agent-log` : file protocol, append then fsync per row, sync,
    ordered, stop-at-first-error. Guarantee: at-most-once per row (monotone
    `synced` cursor never re-appends). There is NO read-back edge: the loop never
    calls `read_log`.
  - `agent-loop` -> `agent-budget` : in-process, sync, infallible apart from a
    tripped cap. Admission (`may_step` + `record_step`) strictly precedes
    provider spend; usage is metered only after a settle or a metered failure.
  - `agent-loop` -> `bets` : `&dyn BetsHook` trait object, sync, exactly-once
    per step head and per dispatched batch, no error channel (a hook can only
    return `PhaseVerdict::Return` to end the run).
  - `crates/rof` -> docker : subprocess, at-most-once per operation, async.
    Container start, `cp` of the frozen tests, one verifier `exec` per grading
    attempt (no retry; failure is a static InfraFailure), best-effort removal.
  - `crates/rof` -> harbor CLI : subprocess `harbor run -c <job> -y -q`,
    at-most-once, async, bounded by verifier timeout + 300s. Spawn failure,
    grading timeout, workdir refusal or a missing RunEnd are infra failures,
    never capability.
  - `crates/eval` -> `crates/rof` : NOT a call. `crates/eval` never execs the
    binary; it parses the `--dump-events` JSONL (exactly-once read) and treats a
    typed-parse failure as a compile error, never a silent zero. The agent leg
    itself lives in `crates/rof/src/eval_agent.rs` and runs the loop in-process
    through the same wiring as `rof run`, so a graded row cannot drift from a
    shipped run.
  - REQ-CAPABILITY -> `crates/eval` : a solve is the task's own native reward,
    re-derived from the reward file; a model's `Done` is a claim, not an outcome.
  - REQ-COST -> `provider-openai::price_usd` : dollars from published PEAK rates
    with cache reads priced at the hit rate; an unknown model id returns `None`,
    never a fabricated price.
  - REQ-TRACE -> `crates/trace` : `--dump-events` JSONL, LF-only, one event per
    line, rewritten post-run if the line count came up short.

MEASURED STATE (as of 2026-11-11; each number appears exactly once):
  - Slice A pilot, 10 high-information tasks, matched config, official container
    verdicts: 0/10 solved at 30 steps/60 actions; 2/10 at 60/120. Cost
    $0.03-0.08 per run. Two different limits are routinely confused and are
    distinct here: a $4 PER-INSTANCE discovery envelope (what one task attempt
    may spend) and a $2.82 TOTAL campaign ledger (all attempts). The runs above
    used neither as their binding cap - steps or tokens bound first.
  - Death mechanism migrates with the envelope: steps 9/10 of runs -> tokens 7/10.
  - OFF-MEASURE accounting recompute over 99 dumps (counter model validated
    90/99 exact): the frozen control's token cap uses
    `BudgetShape::Gross`, which counts CUMULATIVE input+output with cache reads
    at FULL weight (agent-loop/src/state.rs:517-522, default at run.rs:393).
    Token-death runs die at a median 38 turns having spent
    $0.075 = 1.9% of the approved $4 (p90 2.7%, max 4.0%). Cache reads are 46.5%
    of the counter at the kill. A $4-equivalent token cap is ~16.3M = 54x the
    current 300k. `BudgetShape::MissPlusOut` already exists on the same code path
    and is selectable via `--budget-shape miss-plus-out`; it is not the default.
  - Bets A/B (claim tripwire + proof-gated savepoints), 6 runs with the gates
    armed: ZERO interventions. Rollbacks 0/6; no per-hunk proof ever refuted a
    prefix; every failed batch was an empty-hunk exec failure already handled by
    the pre-gate rollback.
  - Incentive ablation, swd task, n=3/arm: declared-DONE 0/3 (Base) -> 1/3
    (+contract) -> 2/3 (+directives); tests 4.7/5.3/5.0 of 7 and edits 8.0/9.3/7.3
    are flat within noise.
  - K=2 echoed-reasoning trim A/B, n=3/arm: quality identical per rep; the
    promised ~68% echo saving never materializes end-to-end because both arms
    spend the full budget. Verdict: no-op, stays flagged, default unchanged.
  - n=2 paired gate (two tasks, 3 reps/cell, official verdicts): rof-hermes and
    rof-pi both -0.167 with CI [-0.33, +0.00], hermes-pi 0.000 CI [-0.33,+0.33].
    UNRESOLVED: CIs touch zero; each harness's single solve landed on a different
    task, so task-harness interaction is strong and n=2 cannot separate them.
  - Surface size: 55 fields on the parsed `Args` struct, 18 of them boolean
    toggles (`crates/rof/src/cli.rs:39-140`); 520 test fns; 0 `todo!()` in the
    workspace.

REJECTED:
  - Durable task phase machine / crash resume (~3.1k lines in the reference
    harness) — rejected because the CLI archives the previous WAL and starts
    fresh; resume needs an interactive or multi-session property this product
    does not have. `read_log` has no production caller.
  - Fork/branch + SQLite state store — rejected: interactive/multi-session
    property, no consumer here. JSONL chosen for inspectability at ~1/50 the code.
  - Operation-actor model (4,999 non-test lines in the reference vs a 978-line
    loop) — rejected on size; its overlap with this design needs a batch that
    does not require all results, which the sequential loop never produces.
  - MCP host in core (7,167 lines outside the reference's own core) — rejected:
    six tools cover the measured workload, and the reference paying 7.6k lines
    outside its core reinforces the exclusion.
  - Parallel tool batches — rejected: wall-clock only, no token/step/dollar
    effect on this workload.
  - Grammar/schema-constrained decoding — rejected: measured to suppress tool
    calls on open-weight models; no measured bottleneck here.
  - Planner / multi-agent / MAS scaffolding — rejected on scope: no task
    decomposition result, and the only matched concurrency evidence is
    horizon-dependent (+14.3pp on one benchmark, -24.0pp on another).
  - Automatic harness-search optimizer — deferred, not built: harness choice
    studies find model choice dominates; no ROF selection result exists.
  - Every opt-in mechanism turned on by default — rejected: none has a measured
    capability gain; several measured as no-ops (bets A/B, K=2 trim).

KNOWN DEBT:
  - `read_log` has no production caller: durable recording exists, crash resume
    does not. Accepted, documented.
  - In-flight provider usage is unknown on timeout/cancellation; the budget
    guardrail can under-count by one request.
  - Blocking git operations can extend the wall-clock deadline.
  - Token accounting defaults to `BudgetShape::Gross`, which counts cache reads
    at full weight while dollars price them at the hit rate — so the token cap
    and the dollar cap disagree by up to ~50x on the same run. Measured and
    documented; the alternate shape exists but is not the default.
  - ~~Compaction's capacity check passes the run-wide token allowance where a
    context-window capacity belongs.~~ CORRECTED 2026-11-11: already fixed —
    `request.rs:484-488` passes `context_capacity_tokens - output_reserve_tokens`
    into `compaction_due`, and `--compaction` requires both flags. The stale
    claim survives in `docs/research-next-steps.md`; that doc is corrected too.
  - `--exec-policy any` removes the allowlist inside the container because the
    process boundary is elsewhere; the choice is recorded so rows never pool.
  - The registry's "strict" schema validation enforces `required` and
    `additionalProperties:false` but IGNORES `maxLength`, `minimum`, `maximum`,
    `minItems`, `maxItems`, `enum`, `minLength` (tool-core/src/registry.rs:78-135).
    Those bounds are only enforced inside each tool's own serde struct, so a new
    tool that forgets them is silently unbounded.
  - An unpriced model id makes the dollar cap INERT: `price_usd` returns `None`,
    `record_spend_cents` is skipped, and only token/step caps guard. The CLI
    prints a one-shot "the $N spend cap is inert" warning but does not fail
    closed (cli.rs:980-990). Model-id spelling decides this: the prefix table
    prices `deepseek-v4.1-flash*` but not `deepseek-ai/DeepSeek-V4.1-Flash`
    (pricing.rs:29-44, asserted at pricing.rs:85).
  - A blocking worker panic panics the run (`expect("blocking worker panicked")`);
    a bets-hook panic unwinds the run future with no `catch_unwind`. Neither is
    caught and converted to an outcome.
  - `context` silently drops an unreadable pinned file (`named_file_contents`
    Err -> pin not applied) and silently skips unreadable dirs in the file map.
    This is the only edge in the system whose failure is invisible to the agent.
  - A torn WAL tail is silently truncated at open and independently dropped by
    the reader; a corrupt mid-file line is fatal with no recovery. Both are
    deliberate; neither is counted or surfaced.

UNKNOWN:
  - Whether the 2/10 -> more solves trend continues with a bigger envelope, or
    whether 2/10 is the ceiling for this model class on this slice. The step-cap
    A/B was run at two points only.
  - Whether any of the default-off mechanisms (compaction, review, repo-lookup,
    completion check, procedure files, history read) changes native reward. All
    are implemented and locally verified; none has a benchmark result.
  - Whether the capability gap is model-class (flash-tier on tasks with frontier
    historical pass rates 0.2-0.8) or harness-shaped. The repo asserts the former
    is expected and has not tested a frontier model end-to-end.
  - Whether the harness contributes at all versus a rival at n>2 tasks: the
    paired CI needs task count. Two tasks are currently shared across harnesses
    (swd and layout-config-recreation2, 3 reps/cell); the earlier n=1 gate's
    "only one task shared" is superseded and must not be requoted.
  - Real WAL bytes per run, real container-start overhead, and the true cause of
    the 5-way parallel provider timeout.
  - The accounting recompute corpus (99 dumps) predates the current frozen
    control recipe (1,200,000 gross tokens / 120 steps / 240 actions). The 300k
    figure above is the recipe those runs actually used, not the current frozen
    control; whether the mispricing matters at 1.2M is not measured, only implied
    by the same counter code. No solve result exists at 120/240.
