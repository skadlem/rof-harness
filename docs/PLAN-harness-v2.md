# rof harness v2 — implementation plan

**Status: stages 0, 1 and 2 built (2026-09-14, evidence in `docs/STATUS.md`); stage 3 is next.** Written
2026-09-14, repo at `967023c` before stage 0 began; the *Stage 0 as built*, *Stage 1 as built* and
*Stage 2 as built* sections below record where the implementation differs from the sketch.

Goal: evolve the harness toward Hermes-style practices (programmable state, skills as procedural
memory, recursive delegation, persistent sessions) while it stays **one local Rust crate**, the CLI
(`run`/`eval`/`config`) and existing suites keep working, the tool gate stays the only path to any
side effect, and the Context/Executor split stops being decorative (the cheap model gets real work).

## Goals as given (do not re-scope these)

1. **Programmable harness state** — instructions, constraints, memory, agent/spec configs as
   structured persistent state beyond `CtxState`; controlled APIs for agents to *propose* updates,
   gated by human approval or evaluators.
2. **Layered context + RAG + cheap Context LLM** — keep the three layers, make per-layer budgets and
   retrieval strategies explicit and configurable; the cheap Context LLM does real summarization and
   context optimization, not just exists; budgets enforced strictly; summarization first-class.
3. **Skill-style self-improvement** — agentskills.io `SKILL.md` pattern under `~/.rof/skills/`
   (directory + `SKILL.md` + optional support files), frontmatter describing when/how to use it;
   `skills_list`/`skill_view`/`skill_manage` tools behind the gate so all three agents can write and
   refine skills; progressive disclosure (names+descriptions in the prompt, bodies on demand);
   prompt nudges to write skills after non-trivial workflows.
4. **Persistent multi-agent orchestration** — keep Planner/Implementer/Reviewer as roles, allow
   recursive delegation (planner sub-plans, implementer subagents like tester/deployer, reviewer
   secondary reviewers), persistent agent sessions with ids and reattachment commands.
5. **Meta-harness evaluation loop** — suite of tasks from JSON/YAML, Planner→Implementer→Reviewer
   per task, traces, metrics (success, tool success, context relevance if possible, cost, latency,
   reliability), and harness-version comparison under the same LLM pair and tasks.

Constraints: single Rust crate, no workspace, no external agent runtime; preserve the CLI and the
existing suites (extend, don't rewrite); the tool policy gate stays the only path to side effects;
keep the two-LLM design and make the cheap one actually used.

## Stage 2 as built (2026-09-14)

`src/context/policy.rs` (`LayerKind`, `LayerStrategy`, `LayerPolicy`, `ContextPolicy`, `LayerReport`,
`SummaryStat`), `ContextBuilder::{plan, plan_summarized}` with a content-keyed summary cache, the
orchestrator's per-layer folding (`fold_layers` → `layer_summaries`/`layer_truncations` in
`ContextMetrics` and in the `--report` dump), `AppConfig.context`, and the env knobs
`ROF_BUDGET_LONG|MID|SHORT`, `ROF_SUMMARIZE_AT`. Offline: `tests/context_policy.rs` (9 tests) plus the
loop test that used to pin whole-prompt condensation, now pinning the per-layer path and its counters.
Live arms and their numbers are in `docs/STATUS.md`.

**Deviations from the sketch above, all deliberate:**

1. `LayerStrategy` has two arms, not four. `Raw` and `HeadTail` have distinct behaviour; `Retrieval`
   and `Outline` have none yet (retrieval feeds the mid layer from the outside already, and `Outline`
   is the plan's own stage-6 item) — a variant with no distinct behaviour is dead schema, the same
   rule that kept the `TraceEvent` variants out of stage 0.
2. `summarize_at` is per layer and `0.0` means *never*, so the three layers can be armed
   independently. The defaults are asymmetric on purpose: mid (retrieval, the only layer whose size
   follows the repo) at `0.8`, long (the stable head — the cached prefix) and short (artifact +
   checks + refusals, the evidence a reviewer judges) at `0.0`. The plan's single `summarize_at: f32`
   per layer had no way to say "this layer is never paraphrased".
3. The summary cache lives in `ContextBuilder`, not in `CtxState::summaries[3]`. The orchestrator
   rebuilds `CtxState` from scratch every round, so a cache inside it would cache nothing between
   rounds — the exact case the cache exists for (a retry must not buy the same summary twice).
4. `plan_summarized` returns `(CtxView, Vec<LayerReport>)` where the plan's `LayerReport` had no
   fields for summarize tokens; `LayerReport.summarize: SummaryStat` carries call/cached/tokens/
   latency/cost/attempts, which is what lets the orchestrator emit the `ModelCall{agent:
   "summarizer"}` trace event without the builder owning a trace sink.
5. The orchestrator's whole-prompt condensation path is deleted, not kept as a second path: it
   summarized *after* an overflow and then cut anyway, and two summarization paths that disagree
   about the order would be two things to measure. `truncated_views` is still reported (as the sum of
   the per-layer cuts) because every pre-stage-2 report carries it.
6. `AppConfig.context` is `Option<ContextPolicy>`: a config file that only states `budgets` (every
   file written before this stage) keeps its exact meaning, and one that states `context` decides —
   budgets included. Without the `Option`, a file with `budgets` and no `context` would silently
   have its budgets ignored, which is the kind of hidden input the eval layer exists to catch.

## Stage 1 as built (done 2026-09-14)

`src/skills/mod.rs` (frontmatter parser, `SkillManager`, proposals), `src/tools/skills.rs` (the three
gated tools), the orchestrator's index/injection wiring, the implementer's `skills`/`skill_views`
artifact keys, the reviewer nudge, `TraceEvent::SkillOp` + `SkillMetrics`, and
`rof skills list|show|approve|reject`. Offline: create → propose → approve → reuse is covered end to
end in `tests/skills.rs` (8 tests), and `eval/suites/skill-tasks.json` (three tasks sharing the
unit-test procedure) is the live instrument. Live results, including the reuse question, are in
`docs/STATUS.md`.

**Five deviations from the sketch above, all deliberate:**

1. The skills root is **not** pushed into `policy.allowed_dirs`. The plan's shortcut ("resolve_under
   then covers it") would have handed the implementer's `fs.write`/`fs.patch` a way into
   `~/.rof/skills`, i.e. around the Propose policy that is the whole point. The skill tools address
   skills by *name* and enforce their own containment (name charset, `safe_child`), the gate checks
   the (agent, tool) grant, and `fs.*` stays out of the store. `tests/skills.rs` pins both halves.
2. The index is fetched through the gate **per agent** (`skills.list` executed as that agent, by the
   harness), so the grant matrix decides who sees it instead of a hardcoded list. Those reads emit
   `SkillOp{op:"list"}`, never `ToolCall`: folding prompt-construction reads into `tool_accuracy`
   would inflate it silently.
3. Models reach the tools through the artifact, not an observe-act loop (the crate has no tool-call
   loop by design): `skills: [{op, ...}]` feeds `skills.manage`, `skill_views: [...]` feeds
   `skills.view` in the same bounded extra turn `reads` already uses. The harness overwrites the
   `agent`/`rationale` fields, so a model cannot write its own audit line.
4. `reused` counts *injected* bodies (the task named the skill) and `viewed` counts *requested*
   ones: two different reuse paths, and the arm needs to tell them apart.
5. The planner is granted `skills.view` (the sketch gave it `list` only). The injection rule
   delivers a body to the planner when the goal names the skill, and a harness feature that depends
   on a grant must *have* that grant — otherwise the injection is dead code that still emits
   metrics. Measured live: before the grant, a three-task run reported 6 injections (implementer +
   reviewer) and the planner's path silently returned nothing; after, 11 (three agents, once per
   plan task). With the planner skipped, no body is fetched for it at all, so `reused` never counts
   a prompt that was never sent.

## Stage 0 as built (done 2026-09-14)

Built in this order — additive, offline-testable, no behaviour change. Acceptance evidence: a
worktree at `c9b4cbf` run with the old and the new binary produced identical per-task results and an
identical aggregate (the report JSONs are equal modulo the two added fields); 57 tests green
(5 new), clippy and fmt clean. Details and the compare output in `docs/STATUS.md`.

1. `src/eval/runner.rs`: `RunLabel { git_head, config_hash, suite_hash, ctx_model, exec_model,
   harness_version }`, embedded in `SuiteReport` as `label` (`#[serde(default)]`), plus
   `SuiteReport::load` so the CLI can read a dump back. `git_head` via
   `git -C <workdir> rev-parse --short HEAD` with an `"unknown"` fallback (never fails a run); both
   hashes are an in-house FNV-1a-64 over the canonical JSON dump — `config_hash` over the `rof config`
   output verbatim, `suite_hash` over the suite as it ran, so `--limit` labels the tasks executed.
2. Per-task `ContextMetrics` (`src/eval/metrics.rs`, on `TaskResult.context`) folded from what the
   trace already carries plus additive keys on the orchestrator's return JSON: `retrieved:
   [{path, chars}]` (from the retriever's snippets) and `summarize_calls` / `summarize_tokens` /
   `truncated_views` (counted in the round loop where the summarizer is already invoked, i.e. the
   implementer's and reviewer's views). `referenced_chars` = snippet chars whose path appears in the
   task's `file_state[].path`; `relevance_proxy` = referenced/retrieved, named a proxy everywhere,
   never "context relevance". The plan's `layer_truncations`/`layer_summaries` are **not** here: they
   have no emitter until stage 2's `LayerReport`, and a field fed by nothing reads as a measured zero
   (the same dead-schema rule that keeps the new `TraceEvent` variants out of stage 0).
3. `src/eval/compare.rs` + CLI `rof compare a.json b.json`: label diff (with the `git_head` /
   `config_hash` / `suite_hash` warnings that say whether the pair is an A/B at all), matched totals,
   per-task rows (gain / loss / only-in-one, rounds, context columns, the losing side's feedback) and
   metric deltas (cost, in/out tokens, cache hit rate, tool accuracy, retries, aborts, model errors,
   latency, retrieved/referenced chars, proxy, summarize tokens, truncated views). Tests in
   `tests/eval_suite.rs`, including one that loads the pre-stage-0 committed baseline and checks it
   reports itself as unlabeled rather than zero-filled.
4. `docs/STATUS.md` updated with this note and the new report fields; committed.

**Two deviations from the sketch above, deliberate**: the model ids in the label come from the
`ContextService`/`ExecutorService` the runner actually uses, not from `cfg.routing` (they agree in
`rof eval`; a test or embedder may wire them differently, and the label should name what was called),
and `git_head` describes the tree the suite ran against — for the standard "before arm" recipe
(`git worktree add ~/rof-armX <commit>` then run from there) that is exactly the revision under test.

**Trimmed from the plan's stage 0, deliberately**: the new `TraceEvent` variants (`SkillOp`,
`StateProposal`, `SubSession`, `SpawnOutcome`, `SessionResume`) are *not* added yet. An enum variant
with no emitter is dead schema that the metrics would fold as zeros; each one lands with its emitter
in its own stage (1, 3, 4, 5). Re-add them then, not before.

Then stage 1 (skills) as described below. Live arms only from stage 1 onward; stage 0 costs zero
tokens.

---

Each stage below is one increment with its own acceptance test and its own measured arm. Stages 0–1
cost zero live tokens (offline stub tests); Stage 2 is the first that changes prompt content and
therefore the first that needs ≥3 runs per arm.

Ground rules carried from what already works (see `docs/STATUS.md`, `agent-harness` skill):

- Nothing new bypasses `ToolRegistry::call()`.
- Stable-first prompt ordering stays; every new prompt block is either byte-stable or in the tail.
- One change per arm; a 6-task single run swings ±2 tasks.
- Default-off for anything that can spend tokens unboundedly (subagents, auto-approval).

---

## 1. Goals mapped onto existing modules

| Goal | Existing home | Change |
|---|---|---|
| 1. Programmable state (instructions, constraints, memory, specs) | `config/mod.rs`, `engine/session.rs` (`CtxState` only) | new `state/` module + `~/.rof/state.json`, `~/.rof/agents/*.json`; proposals + approval CLI |
| 1b. Controlled update APIs | — | `state.propose` tool + `rof state approve/reject`; evaluator hook for auto-approval |
| 2. Layered context, explicit budgets, first-class summarization | `context/{state,builder,retriever}.rs`, `llm::ContextService::summarize` | `context/policy.rs` (per-layer policy), `LayerReport`, summarization before truncation with per-layer cache |
| 2b. Cheap Context LLM actually used | `engine/router.rs` (`Role::Context`), `llm/mod.rs` | live scripts + config set ctx to a cheap model; summarizer traced and budgeted |
| 3. SKILL.md procedural memory | — | new `skills/` module + `skills.list/view/manage` tools + prompt index |
| 4. Recursive delegation | `engine/orchestrator.rs` | new `engine/spawn.rs` (bounded sub-sessions), agent specs |
| 4b. Persistent sessions, reattach | `engine/session.rs` (in-memory only) | new `engine/store.rs`, `~/.rof/sessions/<id>/`, `rof session` CLI |
| 5. Meta-harness comparison | `eval/{suite,runner,metrics}.rs`, `scripts/measure-arm.sh` | `RunLabel` (config/suite hashes), `rof compare`, context/skill metrics, relevance proxy |
| 5b. Trace | `obs/trace.rs` | new events: `SkillOp`, `StateProposal`, `SubSession`, `SpawnOutcome`, `SessionResume` |

Preserved deliberately: crate layout, `Planner → (Implementer → Reviewer)*`, per-task isolated copies
with bounded fan-out, checks-before-verdict, the harness write gate, `parse_lenient`, the JSON
suite format, `--limit/--jobs/--report`.

---

## 2. New modules and types

### 2.1 `src/skills/mod.rs` — SKILL.md as procedural memory

Root: `~/.rof/skills/<name>/SKILL.md` (+ optional `references/`, `scripts/`, `assets/`).
An optional second root `./skills/` in the workdir so a repo can ship skills with it (read-only).

```rust
pub struct SkillMeta {          // the index entry: ALL the prompt ever sees by default
    pub name: String,           // lowercase-hyphen, <=64
    pub description: String,    // <=60 chars, one sentence, self-contained (index truncates)
    pub version: Option<String>,
    pub tags: Vec<String>,
    pub path: PathBuf,          // ~/.rof/skills/<name>/SKILL.md
}
pub struct Skill { pub meta: SkillMeta, pub body: String }
pub struct SkillFile { pub rel: String, pub bytes: usize }   // listing only, content on request

pub enum SkillOp {              // everything the manage tool can do
    Create { name: String, description: String, body: String },
    Patch  { name: String, find: String, replace: String },
    WriteFile { name: String, rel: String, content: String },
    Delete { name: String },
}

pub enum SkillPolicy { ReadOnly, Propose, Direct }   // default Propose

pub struct SkillManager { root: PathBuf, extra_root: Option<PathBuf>, policy: SkillPolicy }
impl SkillManager {
    pub fn list(&self) -> Vec<SkillMeta>;
    pub fn view(&self, name: &str, file: Option<&str>) -> Result<SkillView, SkillError>;
    pub fn manage(&self, op: SkillOp) -> Result<SkillChange, SkillError>;  // Propose => writes a proposal
    pub fn index(&self, max: usize) -> String;                             // "- name: description" lines
}

pub fn parse_skill(md: &str) -> Result<(SkillMeta, String), SkillError>;
```

- Frontmatter: minimal YAML subset, no dependency — `---` at byte 0, `key: value`, `key: [a, b]`,
  folded `description: >` one level. Repo's stated zero-dep stance holds until frontmatter grows past
  that; `serde_yaml` is a one-line upgrade if it ever does.
- Progressive disclosure: `index()` (names + descriptions, ~40 chars/skill) goes into the **stable
  head** of planner/implementer/reviewer prompts; a skill whose name the goal or plan task mentions
  gets its body injected (same rule as a goal-named file); anything else costs a `skills.view` call.
- `Propose` mode writes to `~/.rof/proposals/skills/<id>.json`; `rof skills approve <id>` applies.
  Never write-through by default: an agent editing its own instructions unattended is the one
  failure mode this whole design must not have.

Tools (registered in `ToolRegistry::with_defaults`, granted per agent, same gate):
`skills.list`, `skills.view`, `skills.manage`. Grants: planner `list`; implementer `list/view/manage`;
reviewer `list/view`. `with_defaults()` additionally pushes the skills root into
`policy.allowed_dirs` (`resolve_under` then covers it with no change to the gate logic).

Prompt nudge (implementer + reviewer system prompts, one line each): *after a workflow that took
more than one round and worked, record the lesson as a skill — one rule per lesson, with the trigger
in the description.*

### 2.2 `src/context/policy.rs` + `builder.rs` — explicit budgets, first-class summarization

```rust
pub enum LayerKind { Long, Mid, Short }
pub enum LayerStrategy {
    Raw,                                   // pass through
    HeadTail,                              // current behaviour
    Retrieval { max_snippets: usize, named_file_cap: usize },
    Outline,                               // symbol outline for oversize files (no model call)
}
pub struct LayerPolicy { pub budget: usize, pub strategy: LayerStrategy, pub summarize_at: f32 }
pub struct ContextPolicy { pub long: LayerPolicy, pub mid: LayerPolicy, pub short: LayerPolicy }
pub struct LayerReport { pub layer: LayerKind, pub chars: usize, pub est_tokens: usize,
                         pub truncated: bool, pub summarized: bool }
impl From<TokenBudgets> for ContextPolicy     // old configs keep working
```

`ContextBuilder` gains two entry points and keeps `build()` for tests:

```rust
pub fn plan(&self, state: &CtxState) -> (CtxView, Vec<LayerReport>);            // sync, pure
pub async fn plan_summarized(&self, state: &CtxState, ctx: &ContextService)
    -> (CtxView, Vec<LayerReport>);                                             // first-class path
```

Rules made explicit:

1. Per layer: if `chars > summarize_at * budget` → summarize that layer **alone** with the cheap
   model, cache it in `CtxState::summaries[LayerKind]`, invalidate when the layer text changes.
   Truncation is the fallback, never the first move (today it is the trigger).
2. `CtxState` grows `summaries: [Option<String>; 3]` and `policy: ContextPolicy` — the summary cache
   is why a retry no longer re-summarizes the same text every round.
3. The reviewer gets a budgeted view too (today it gets an unbounded short-term).
4. Config: `context: ContextPolicy` with serde defaults; env knobs `ROF_BUDGET_LONG|MID|SHORT`.
5. `Role::Context` gets real traffic: planner + every summarization + (Stage 2) an optional
   "context optimizer" call that picks retrieval strategy per task from a fixed menu — traced like
   any model call so its cost is visible.

### 2.3 `src/state/mod.rs` — programmable harness state

```rust
pub struct Instruction { pub id: String, pub text: String, pub scope: Scope, pub source: String }
pub enum Scope { Global, Path(String), Suite(String) }
pub struct Constraint { pub id: String, pub text: String, pub kind: ConstraintKind }
pub enum ConstraintKind { Never, Always, Budget }      // Never/Always feed the prompt + a harness check
pub struct MemoryNote { pub id: String, pub text: String, pub tags: Vec<String>,
                        pub evidence: Vec<String> }     // trace event refs, so memory stays auditable
pub struct AgentSpec { pub name: String, pub role: Role, pub system: String, pub tools: Vec<String> }
pub struct HarnessState { pub version: u32, pub instructions: Vec<Instruction>,
                          pub constraints: Vec<Constraint>, pub memory: Vec<MemoryNote> }
pub enum StatePatch { AddInstruction(..), AddConstraint(..), AddMemory(..), RetireMemory(String) }
pub struct Proposal { pub id: String, pub patch: StatePatch, pub rationale: String,
                      pub evidence: Vec<String>, pub status: ProposalStatus }

pub trait StateStore { fn load(&self) -> Result<HarnessState>; fn save(&self, s: &HarnessState) -> Result<()>; }
pub struct FileStore { root: PathBuf }   // ~/.rof/state.json, ~/.rof/proposals/*.json
```

- Rendering: instructions/constraints go into the **long-term** layer (stable head), memory notes
  that match the task's paths go into mid-term. Budgeted like everything else.
- `state.propose` tool (all three agents; reviewer most useful) → proposal file, never applied
  inline. Approval: `rof state approve <id>` (human) or `HarnessConfig::auto_approve` with
  `{ scope: Global|Path, requires_check_pass: true }` — an evaluator-gated path, off by default.
- `rof state show|diff|apply <file>` for versioned state; the file is plain JSON so it can be
  reviewed in git like a config.

### 2.4 `src/engine/spawn.rs` + `orchestrator.rs` — bounded recursive delegation

```rust
pub struct SpawnRequest { pub spec: String, pub goal: String,
                          pub budget_tokens: u64, pub max_rounds: u32 }
pub struct SpawnPolicy { pub max_depth: u32,            // default 0 = feature off
                         pub max_children_per_task: u32,
                         pub child_budget_share: f32,   // of the parent task's remaining budget
                         pub allow: Vec<String> }       // spec names
pub enum SpawnOutcome { Done { summary: String, writes: Vec<String> },
                        Denied(String), BudgetExhausted, DepthExceeded }
```

- An artifact may carry `spawn: [SpawnRequest]`; the orchestrator runs each as a **sub-session**
  (`Session::child(parent_id, spec, goal)`) with its own forked trace sink, its grant list narrowed
  to its spec, and a hard slice of the parent's remaining token budget.
- Bounds are enforced in the harness, not the prompt: depth, children per task, shared budget
  (`Arc<AtomicU64>`), and `SpawnOutcome` injected into the parent's short-term as one line each.
- Default three specs: `implementer`, `reviewer` (existing prompts) plus `verifier` (tester: runs
  allowlisted checks and reports; no write tools). Custom specs load from `~/.rof/agents/<name>.json`.
- Trace: `SubSession { parent, child, spec, goal }` on start, `SpawnOutcome` on end; child events
  carry their `session_id` so `tokens_by_agent` gains a `spec` axis.

### 2.5 `src/engine/store.rs` — persistent sessions and reattachment

```rust
pub struct SessionSnapshot { pub id: String, pub parent: Option<String>, pub spec: String,
                             pub goal: String, pub status: RunStatus, pub task_index: usize,
                             pub round: u32, pub ctx: CtxState, pub workdir: PathBuf,
                             pub config_hash: String, pub created_at: u64, pub updated_at: u64 }
pub struct SessionStore { root: PathBuf }   // ~/.rof/sessions/<id>/{snapshot.json,trace.jsonl}
```

- Snapshot written atomically at each round boundary (temp + rename), append-only trace per session
  in the same directory. `config_hash` is recorded so a resume under a different policy is *refused*
  unless `--force`.
- CLI: `rof session list`, `rof session show <id>` (last N events + metrics), `rof session resume <id>`
  (re-copies the task dir, replays the snapshot, continues from the recorded round).
- Trace: `SessionResume { id, from_round }`; metrics gain `resumes`.

### 2.6 `src/eval/*` — meta-harness comparable across harness versions

```rust
pub struct RunLabel { pub git_head: String, pub config_hash: String, pub suite_hash: String,
                      pub ctx_model: String, pub exec_model: String, pub harness_version: String }
/// Shipped in stage 0; `layer_truncations`/`layer_summaries` arrive with the
/// stage-2 `LayerReport` (no emitter before that, and a zero from a field
/// nothing writes is not a measurement).
pub struct ContextMetrics { pub retrieved_files: usize, pub retrieved_chars: usize,
                            pub referenced_chars: usize, pub relevance_proxy: f32,
                            pub summarize_calls: u64, pub summarize_tokens: u64,
                            pub truncated_views: u32,
                            /* stage 2: */ pub layer_truncations: [u32; 3],
                            pub layer_summaries: [u32; 3] }
pub struct SkillMetrics  { pub listed: u64, pub viewed: u64, pub proposed: u64,
                           pub applied: u64, pub reused: u64 }
```

- `RunLabel` lands in every `--report` dump (stage 0, done); `config_hash` = FNV-1a-64 of the
  canonical `rof config` output (already byte-stable by design; the hash has to separate identical
  from different inputs, which is not a job that needs sha2 and a dependency).
- `rof compare a.json b.json` → per-task matched delta + metric delta table (formally replaces
  eyeballing two reports); `scripts/measure-arm.sh` keeps producing the raw arms.
- `relevance_proxy` (stated as a proxy, not truth): of the chars retrieval put in front of the
  implementer, the share whose source path appears in the artifact's `patches[]`/`writes[]`. Computed
  from the trace + per-task result, no extra model calls.
- Suites gain optional per-task `spec`, `max_rounds`, `checks_extra` (`#[serde(default)]`, so
  existing JSON is untouched). YAML stays out unless asked: the repo chose JSON for zero deps, and
  `serde_yaml` is a decision (and a measured dependency), not a freebie.

---

## 3. Order of work (each stage = one arm, one acceptance test)

**Stage 0 — observability first (no behaviour change). — DONE 2026-09-14**
Report labels (`RunLabel` + config/suite hashes), per-task `ContextMetrics` from existing trace data,
`rof compare a.json b.json`. Acceptance met: a worktree at `c9b4cbf` gave byte-identical per-task
results and aggregate under the old and the new binary; a report carries the label; `rof compare`
prints the delta between two report dumps, including a pre-stage-0 baseline (see *Stage 0 as built*).
The plan's `TraceEvent` variants are deliberately *not* part of stage 0 — see *Stage 0 as built* for
the trim and the reasoning. *Zero live tokens: offline stub tests.*

**Stage 1 — skills core. — DONE 2026-09-14**
`skills/` module, frontmatter parser + tests, three tools behind the gate, prompt index, nudges,
`Propose` default, `rof skills list|show|approve|reject`. Acceptance met offline: `tests/skills.rs`
drives stub agents through create → propose → approve → a later task's prompt carrying the body
(injected and on request); `eval/suites/skill-tasks.json` runs clean live (3/3, in `docs/STATUS.md`).
*First live arm answered: agents wrote no skill on a suite whose tasks all pass in one round, and the
nudge's trigger is a multi-round workflow — see `docs/STATUS.md`.*

**Stage 2 — context policy + cheap Context LLM. — BUILT 2026-09-14 (live arm in `docs/STATUS.md`)**
`LayerPolicy`, `plan`/`plan_summarized`, per-layer summary cache, budgeted reviewer, config + env
knobs; the cheap model is the one already wired as `ContextService` (`ROF_CTX_MODEL`). Acceptance:
3 runs per arm on `repo-tasks --limit 6` with **matched non-inferior** and cost/tokens down, and
summarize calls visible in the trace with their tokens — see `docs/STATUS.md` for the measured arm
and for what the default threshold does (and does not) fire on at this suite's context sizes.

**Stage 3 — programmable state.** `state/` module, `~/.rof/state.json`, `state.propose` tool,
approval CLI, evaluator auto-approve hook, instructions/constraints rendering. Acceptance: a suite
task whose goal violates a `Never` constraint is refused by the harness (not just the prompt), and a
proposal round-trips through approve/reject.

**Stage 4 — subagents.** `spawn.rs`, specs, depth/children/budget bounds, `SubSession` tracing, CLI
`rof run --spec`. Acceptance: with `max_depth=0` behaviour and metrics are unchanged (regression
check); with depth 1 a multi-file task (`eval/suites/decompose.json`) shows child sessions in the
trace and no budget overrun.

**Stage 5 — persistent sessions.** `store.rs`, snapshot at round boundaries, `rof session
list|show|resume`. Acceptance: kill a run mid-task, resume it, and finish with the same matched
result as an uninterrupted run; a resume under a changed `config_hash` is refused.

**Stage 6 (optional, only if earned).** YAML suites, `Outline` layer strategy, in-repo `./skills/`
root, skill-relevance scoring without embeddings, `rof compare --n-runs`.

### Start here

Stage 3 (programmable state) is the next stage the plan names, but finishing stage 2's acceptance arm — the fix, the two commands and the trap that makes ordering matter
(binary rebuilt per run ⇒ freeze the tree) are in `docs/STATUS.md` § *Stage 2*. Stages 0–2 are built;
every arm from stage 1 onward reports through the stage-0 instrument (`rof compare` + labels + context
metrics). Stages 2 and onward change prompt content and therefore need ≥3 runs per arm.

---

## 4. Where metrics, logging, and config hooks go

| Subsystem | Trace event | Metric | Config/env |
|---|---|---|---|
| Skills | `SkillOp { op, name, ok, bytes }` | `SkillMetrics::{listed,viewed,proposed,applied,reused}` | `skills: { root, policy }`, `ROF_SKILLS_ROOT` |
| Context | existing `ModelCall{agent:"summarizer"}` + `LayerReport` folded as `ContextMetrics` | `layer_truncations/summaries`, `summarize_tokens`, `relevance_proxy` | `context: ContextPolicy`, `ROF_BUDGET_*`, `ROF_CTX_MODEL` |
| State | `StateProposal { id, kind, status }` | proposals proposed/applied/rejected | `harness.auto_approve`, `ROF_STATE` |
| Subagents | `SubSession`, `SpawnOutcome` | `subsessions`, `spawn_denials`, tokens per spec | `spawn: SpawnPolicy`, `ROF_SPAWN_DEPTH`, `~/.rof/agents/*.json` |
| Sessions | `SessionResume { id, from_round }` | `resumes`, per-session duration | `→ ~/.rof/sessions/`, `ROF_SESSIONS_DIR` |
| Eval | `RunLabel` in the report (built) | per-task matched deltas via `rof compare` | suite task fields; `--label` on `eval` (planned) |

Everything above is additive to `AppConfig` with `#[serde(default)]`, so `configs/*.json`,
`eval/suites/*.json` and committed baselines keep parsing unchanged.
