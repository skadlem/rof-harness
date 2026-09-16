# ROF Harness — Architecture v3

**Status:** draft for review · **Supersedes:** `ARCHITECTURE.md` (v2) · **Date:** 2026-09-16

## 0. Purpose

ROF is a local Rust harness that runs bounded coding agents against a working tree and measures task outcomes independently of model prose.

v3 changes the *means*, not the purpose: **make the harness adaptive** — pull in only the context a task needs, and pay for frontier-class coding behavior at flash-class cost.

> **The objective is task completion, with quality. Cost is a per-task *constraint* — not the objective.** Cheapness comes from constraint satisfaction (stay in budget, ride the 50× cache), never from trading quality for tokens.

The target is quantified, not aspirational (official docs unless noted):

| | DeepSeek-V4.1-Flash | GPT-6 Astra |
|---|---|---|
| Coding (Deep SWE) | **74.2** | ~74 (tied; same cluster as Gemini-3.8-Flash, Opus-5) |
| Cost per task | **$0.27** | $8.75 / $3.26 (peers) |
| Input (cache hit / miss) | **$0.003 / $0.15** per Mtok | $1.00 / $10.00 |
| Cache discount | **50×** | 10× |

Flash-class models *tie* frontier models on rubric-checkable coding and break on state tracking and novel multi-step logic ([MindStudio, 2026-09-12](https://www.mindstudio.ai/blog/deepseek-v4-1-flash-benchmarks), third-party). That gap is the harness's job to close — deterministically, measurably.

**Scope of this document.** v3 was reviewed with a YAGNI discipline and cut to the **minimal core in §4**: six components, each justified by a measurement or a correctness bug already in the code. Everything speculative — including the per-task budget dial and graph retrieval, the two most ambitious ideas — moved to the **triggered backlog in §8**: deferred, not deleted, each with the measurement that would justify building it.

## 1. Eight principles

Each principle below is *demonstrated by a component in §4*, not asserted.

1. **Selection over compression.** Context is minimized by choosing what enters, never by summarizing what is inside the edit surface. (Measured: summarizing the mid layer cost 8/12 → 2/12, p=0.036.)
2. **Lossless on the edit surface.** Bytes a patch is written against are never summarized or head+tail cut. If they don't fit the budget, that is a **selection failure** → re-select, never truncate.
3. **Stability is worth 50× more than size.** DeepSeek's disk cache requires a *full prefix match*. Volatility is confined to the smallest possible tail; a smaller-but-perturbed prefix pays 50× on every round.
4. **The harness decides; the model proposes.** Pass/fail comes from checks and tree state, never from model prose or model self-report. (Kept from v2 — the strongest part.)
5. **Root cause, not symptom.** v2 compensates for stale tree state with `file_state` windowing and last-read-wins heuristics; v3 removes the root cause (§4.2). The lazy fix *is* the root-cause fix.
6. **Determinism first.** Dedupe, windowing, and metric collection are deterministic and offline-testable. No model call for anything a rule can decide.
7. **Comparability is sacred.** The model pair is frozen per run; escalation may be *suggested*, never executed. Matches Meta-Harness, where "the model M varies by domain and is always frozen" ([arXiv:2603.28052](https://arxiv.org/abs/2603.28052)).
8. **Isolation by copy.** Per-task trees; parallel writers never share state; the source tree is never a target.

## 2. What v3 changes (vs v2)

Scoped to the minimal core:

| v2 | v3 |
|---|---|
| 3 layer budgets + agent-appended sections outside any budget | **one** `ContextAssembler` owning the whole prompt |
| Same file's bytes in the prompt up to 3× (retrieval, `file_state`, `[VERIFIED FILES]`) | dedupe by `ItemKey` across layers and rounds |
| Reviewer evidence raw/unwindowed (≤262 KB × 5), then head+tail cut by the 24k-char budget | every file view windowed on its anchor, budgeted uniformly |
| `writes_made` = count of the model's self-reported `artifact["writes"]` (`orchestrator.rs:306–310`, `559–563`) | **`git diff --stat`** — ground truth, mode-independent |
| Linear retry loop, stale tree state, duplicate-edit bug class | **attempt rollback** via per-task git; clean tree per attempt |
| Direct-mode verdict = `checks_log.contains("STATUS: FAILED")` (`orchestrator.rs:568`) | structured `Vec<CheckResult>` in `TaskOutcome` and report |
| Direct mode silently lacks skills / goal-quality / auto-poke (drift: all three live only in `run_loop`, `orchestrator.rs:81–93, 102, 407–419`; `run_direct_loop` starts at line 481) | extracted into `RoundServices`; no mode can lack them |
| Budget = O(n²) trace rescan per round | per-task `Budget` counter incremented on each call |
| Reviewer = same model as implementer, no way to test otherwise | third `verify_model` slot (defaults to executor) |
| No notion of retrieval recall | **recall metric** — the instrument for every future retrieval idea |
| Lexical containment only | symlink-aware containment before writes |

## 3. System shape

```
                     ┌────────────────────────────────────────────┐
                     │                   CLI                      │
                     │     run | eval | compare | config          │
                     └───────────────┬────────────────────────────┘
                                     │ AppConfig (defaults<file<env<cli)
   ┌─────────────────────────────────┼───────────────────────────────────┐
   │                          EvaluationRunner                          │
   │   per-task worktree copy (incl. .git, harness-side only)            │
   │   trace fork          isolated result, suite order preserved        │
   └─────────────────────────────────┬───────────────────────────────────┘
                                     │
                        ┌────────────┴────────────┐
                        │     Orchestrator        │  execution: pipeline|direct
                        │     (config switch)     │  ← orchestration is already
                        └────────────┬────────────┘     a measured variable
                                     │
                     ┌───────────────┴───────────────┐
                     │        RoundServices          │  (shared → no mode drift)
                     ├───────────────┬───────────────┤
                     │ ContextAssembler │  one budget, dedupe, windowed views
                     │ CheckRunner      │  → Vec<CheckResult>
                     │ TreeService      │  snapshot / rollback / diff
                     │ Evidence         │  windowed read-back
                     │ SkillIndex       │  progressive disclosure (all modes)
                     │ Budget           │  counter, not a trace scan
                     └───────┬───────┴───────────────┘
                             │
                  ┌──────────┴──────────┐
                  │   ToolRegistry      │  ← the ONLY path to a side effect
                  │  deny-by-default    │  grant matrix + path/host/cmd policy
                  └──────────┬──────────┘
                             │
        fs.*  proc.run  http.get  skills.*
                             │
                  ┌──────────┴──────────┐
                  │   TraceSink (JSONL) │ → metrics → report → compare
                  └─────────────────────┘
```

## 4. Components — the minimal core

Six components. Each is justified by a measurement already in hand or a bug already in the code.

### 4.1 `context/assembler.rs` — the one budget owner *(new)*

**Trigger (findings 1 & 2).** `ContextBuilder` budgets three layers; `ImplementerAgent` then appends the file map, up to 3 whole requested files, and 2 skill bodies *outside any budget* — the largest and least-governed token consumers in the system. Meanwhile the same file's bytes appear up to 3× per round (mid-term retrieval, `short_term` `file_state`, the reviewer's `[VERIFIED FILES]`), and the reviewer's unwindowed evidence (≤262 KB × 5) gets head+tail cut by the 24k-char budget — so the reviewer may never see the region it is judging.

**Design.** One assembler owns the whole prompt for every role.

```rust
enum Fidelity { Exact, Windowed(anchor), Summary, Drop }

struct ContextItem {
    key: ItemKey,          // (path, region, role) — the dedupe key
    fidelity: Fidelity,
    must_include: bool,    // edit surface and evidence: never dropped
    est_chars: usize,
}

struct ContextAssembler {
    budget: usize,                              // ONE prompt budget, in chars
    seen: HashMap<ItemKey, Placement>,          // dedupe: across layers and rounds
}

enum Assembly { Ok(PromptParts), SelectionFailure { excess: Vec<ContextItem> } }
```

Three rules, kept deliberately small:

- **Fill, don't optimize.** All `must_include` items fit or the assembler returns `SelectionFailure` — a first-class result the orchestrator acts on (narrow retrieval, drop a requested file, tighten a window). Optional items fill the remainder and are dropped when full. This is a `retain` loop, not a knapsack.
- **Dedupe by `ItemKey`.** A named file appears once per role, whatever path it arrived by. Metric: `repeated_chars_eliminated`.
- **Order for the cache, don't type it.** Volatile parts (task text, artifact, checks, evidence) are appended *last*, so the stable prefix rides the 50× disk cache. Same effect as a typed `head`/`tail` partition, without the abstraction.

`ContextBuilder` is deprecated in favor of the assembler; every file view, including the reviewer's evidence, goes through the same windowing and budget.

### 4.2 `engine/tree.rs` — attempt rollback + diff-based write gate *(new)*

**Trigger.** The duplicate-edit bug class (E0592/E0428 — item already defined) and a write gate that counts the model's *self-reported* `artifact["writes"]` array, filtering entries starting with `FAILED`. The model is grading its own homework.

**Design.** Each implementer attempt starts from a clean tree:

Git becomes the tree-state substrate of the task copy — present always, harness-side only:

- `copy_tree` **includes `.git`** (it currently excludes `.git` and `target`, `runner.rs:384`). `target` stays excluded — it is by far the largest subtree (`runner.rs:376`); history is not.
- When the source history is large it is copied **shallow** (`--depth 1`): rollback and diff need tracking of the working tree, not ancestry, so copy cost comes off the history axis:

```rust
// ponytail: .git in task copy — shallow (--depth 1) keeps copy cost off the
// history axis; escalate to full history only if a check turns out to need ancestry.
```

- When the source workdir is not a git repo at all, the copy gets `git init` plus an initial commit — rollback must not silently degrade for non-repo inputs.
- Before an attempt: baseline commit. On failure: `git checkout -- . && git clean -fdq` — restores tracked files *and* removes untracked files the attempt created.
- The write gate reads `git diff --stat` / `--name-only` — "did the tree change, and what?", replacing the self-reported count entirely. The same `--name-only` output feeds the recall metric (§4.4), so one substrate serves both.

`git` is never added to `ROF_ALLOW_CMDS` (the agent cannot run it), and `.git` stays excluded from retrieval and from every path the file tools resolve — the model never sees it.

**Implementation (landed).** The concrete choices, so the code and this section cannot drift:

- `engine/tree.rs` owns the substrate as `TreeService` (`ensure` / `baseline` / `rollback` / `diff`), plus `copy_git_state` for `copy_tree`. A size threshold (`SHALLOW_GIT_BYTES`, 8 MiB) picks shallow clone over verbatim copy; a repo that cannot be cloned shallow falls back to a full `.git` copy, and an unborn HEAD gets the commit the source never made — `git checkout` refuses an unborn HEAD, so without it rollback would fail on the degenerate input.
- The change set comes from `git status --porcelain`, not `git diff --name-only` alone: plain diff misses files the attempt *created*, and a new file must count as a write. `diff --stat` still supplies the evidence line, with new files appended (it cannot see them either). Paths are unquoted — git C-quotes names with spaces or high bytes, and recall needs the exact name.
- Harness commits carry a fixed identity and run with hooks disabled: a source's pre-commit contract is not the harness's, and a failing one would block a baseline a round depends on.
- A failed attempt is rolled back only when a retry follows, so the final tree of a task stays in the copy for reading.
- Consequence for the retry's evidence: a change that *landed* is reported as rolled back, without its post-attempt text — that text describes a tree the harness restored, and a retry that trusted it would skip a change that is gone. A *refused* patch still hands over its text: the attempt changed nothing there, so the read survived the rollback. Both are in `file_state_evidence`.

**Why not snapshots of touched files.** That alternative was weighed and rejected on quality grounds: a snapshot-restore gate restores only files it knew to snapshot, missing untracked files the attempt created and files it deleted, so rollback is partial; to be complete it must enumerate the change set, which means reimplementing what `git status` already gives. It would also degrade the recall metric, which depends on exact change detection. For a harness whose verdicts turn on tree state, partial rollback is worse than no rollback. This is the branching-DAG pattern adapted correctly: **ROF has no message-history pollution — it rebuilds context every round. Its pollution is tree-state, so branch the tree, not the transcript.** Including `.git` is label-consistent: `RunLabel` already records `git_head` (`runner.rs:54`), so runs stay comparable.

### 4.3 `engine/session.rs` — structured outcomes + `RoundServices` *(rewritten)*

**Trigger.** Two correctness bugs: the direct-mode verdict is a substring match on a log (`checks_log.contains("STATUS: FAILED")`), and direct mode silently lacks the skill index, the goal-quality note, and auto-poke because all three are written into `run_loop` only.

**Design.** Extract the shared concerns into one struct both modes hold — `RoundServices`: tools, ctx/exec/verify services, `Budget` (a per-task counter, replacing the O(n²) per-round trace rescan), `CheckRunner`, `TreeService`, `Evidence`, `ContextAssembler`, `SkillIndex`. The two modes keep selecting via the existing `execution: pipeline|direct` config switch; the extraction is what removes the drift, and a shared `loop_one_round()` consolidates the duplicated loop scaffolding.

The outcome becomes structured:

```rust
struct CheckResult { name: String, passed: bool, output: String }
struct TaskOutcome {
    checks: Vec<CheckResult>,      // no more log-string matching
    writes: WriteSummary,          // from git diff, not self-report
    verdict: Verdict,
    feedback: String,
    evidence: Vec<EvidenceRef>,
}
```

`compare` gains per-check resolution: "task X moved because check Y flipped." A `Vec<CheckResult>` does not exist anywhere in `src/` today — checks are a `Vec<String>` that becomes a log line and reach the report only as prose inside `feedback`.

**Why not a strategy trait.** A trait with three strategies was considered and deferred: the config switch *already* makes orchestration a measured variable, and the third strategy (`PlanDirectVerify`) has no arm. See §8.

**Implementation (landed).** `RoundServices` is a borrow-only struct holding the services both modes must use identically — `cfg`, `trace`, `context`, `executor`, `verify`, `tools` — with the methods that used to live in `run_loop` alone: `run_checks` (returns `Vec<CheckResult>`), `skill_index`, `skill_bodies`, `reviewer_file_evidence`, `goal_note`, `budget`, and the pure `head_with_index`. The two loops construct one each and call the same methods, so "which prompt part does a mode forget" became a compile error against the struct rather than a silent drift. A mode that wants the skill index must call the shared method; there is exactly one of each.

Deliberate scope cuts (ponytail):

- **No `TaskOutcome` struct.** The report is already a `serde_json::Value` and both the CLI and `compare` read it; a typed outcome would have rewritten the whole output path for no measured bug. What the outcome *needed* — a verdict that does not parse prose, and a check list `compare` can diff — is delivered by `CheckResult` alone, carried in the report as `check_results` next to the rendered `checks` log the prompts still show.
- **No `WriteSummary`/`EvidenceRef`.** The write gate's numbers and the reviewer's evidence are already structured (`TreeDiff`, the artifact's `file_state`); new types would wrap existing ones.
- **No shared `loop_one_round()`.** The loops differ in *policy* (who gets a model call, who decides pass), and collapsing them would re-introduce a mode flag inside one body. `RoundServices` removes the drift they shared; the policy difference stays explicit. `fold_layers` stays on the orchestrator — it is run-wide accounting, not a per-round service.

The direct verdict is now `checks_pass(&results)` — a field read. The substring bug was not "the string was wrong" but that the verdict and the log were the same object, so any quoted failure text inside a passing body flipped the task; `render_checks` is now a pure function of the results, and the verdict never touches it.

`Budget` is O(1): `TraceSink` totals `input+output` on every `ModelCall` at the `emit()` choke point (`total_tokens()`), `fork()` starts a child at zero, and `Budget::exceeded()` is a subtraction. The old `tokens_since` rescanned the event stream from a trace index once per round per task — O(rounds²) in the stream length. Direct mode also gained the three services it was missing, plus the bounded auto-poke (at most one extra round), which is off by default and now traced identically in both modes.

`compare` reads `TaskResult.checks` and reports `CheckFlip` — same check name, different outcome — rendered as `check 'cargo test' flipped: fail -> pass`. A task that moved with **no** flipped check leaves the list empty, and that emptiness is the signal: the acceptance gate held, so the move was a verdict effect, not a gate effect. The two have different fixes, which is the point of naming them apart. Reports written before the field existed load with `#[serde(default)]` and compare as empty lists.

### 4.4 `eval/metrics.rs` — retrieval recall *(new metric)*

**Trigger.** There is no measurement that can justify any retrieval improvement. `relevance_proxy` measures precision-ish value; nothing measures whether retrieval *found the right files*.

**Design.** **Recall = (files a task's patches touched) ∩ (files in the retrieved set) / (files the patches touched).** Computed from the git diff (free, given §4.2) against the retrieval log. Ships *before* any retrieval feature — including the graph in §8 — because a recall baseline is the only thing that can say whether graph expansion is worth building at all.

**Implementation (landed).** `ContextMetrics` carries `changed_files` / `recalled_files` counts and a derived `recall()`; `from_run` takes the change set from §4.2's per-task `changed_files` (ground truth — *not* the artifact's `file_state`, which is self-reported and counts refused patches), and intersects it with the run's `retrieved[]`. It prints per task (`recall=recalled/changed`) and aggregates in `rof compare` as a true ratio, not a mean of per-task ratios, so a task touching 1/1 cannot average away one touching 0/100. `retrieved` is run-wide, so a multi-task plan measures each task against the single retrieval that fed the plan — the signal this run supports; per-task retrieval, if it ever exists, is what would sharpen it.

### 4.5 `engine/router.rs` — `verify_model` slot *(new, config only)*

**Trigger.** "Independent verification" is currently untestable: the reviewer is always the executor.

**Design.** One new routing slot, defaulting to the executor — zero behavior change until set; `ROF_VERIFY_MODEL` selects it. This is the decision gate for the reviewer's future (arm #4), not a feature. Escalation, if it ever exists, is a logged *suggestion*, never an executed swap (principle 7).

**Implementation (landed).** `RoutingConfig.verify_model: Option<String>` (default `None` = self-review), `Role::Verify` in `ModelRouter` resolving to the executor model plus its fallback chain when unset, and `ROF_VERIFY_MODEL` in `apply_env`. `Orchestrator` holds a separate `verify: ExecutorService` and `ReviewerAgent::new(&self.verify)` runs on it, so a live A/B is `ROF_VERIFY_MODEL=x` with nothing else changed and no code path differing between the arms. The eval runner and the test helper pass the executor clone until a config sets the slot.

### 4.6 Security — symlink-aware containment *(correctness, not an arm)*

**Trigger.** v2 documents the hole: `under()` is lexical, so a symlink inside an allowed dir can escape it. Until this lands, "deny-by-default" is real for paths and performative for symlinks.

**Design.** Canonicalize the parent directory before any write; refuse a symlink escape. Prerequisite for any trusted write-enabled run against an untrusted tree. Ponytail's rule holds: never simplify away a trust boundary.

**Implementation (landed).** `tools::symlink_safe` runs on every read, list and write: it resolves the containing directory against the canonical root (the root itself is the trust anchor and is exempt, so `path: "."` still works), and separately resolves a symlinked final name — a verified directory is not enough, since writing through `/root/link -> /etc/x` still lands in `/etc`. Chains resolve fully when the target exists; a *dangling* link is checked by name rather than trusted, because the write would create its target. A link that resolves back inside the root still reads, so the fix costs nothing legitimate. `copy_tree` already skipped symlinks per entry, so no task copy carries one in.

## 5. Invariants that must survive v3

1. **One policy gate.** Every side effect goes through `ToolRegistry::call`, deny-by-default.
2. **The harness decides pass.** Configured checks pass before any verdict; `expect_writes` is enforced by tree diff, not by self-report. Quality = checks pass + goal-quality note; reviewer approval does *not* gate the verdict.
3. **Lossless edit surface.** `Exact`/`Windowed` items are never summarized or truncated; overflow is a `SelectionFailure`, acted on, never a cut.
4. **Trace is the substrate.** Every model/tool/verdict/budget/selection event is append-only JSONL; reports fold from it; `RunLabel` makes runs comparable; `compare` refuses to imply an effect across differing inputs.
5. **Determinism first.** No model call for anything a rule can decide.
6. **Isolation by copy.** Per-task trees; the source tree is never a target.
7. **Comparability is sacred.** Model pair frozen per run; escalation suggested, never executed.

## 6. Layout delta

```
src/
  engine/      session.rs (+RoundServices, +TaskOutcome/CheckResult),
               router.rs (+verify slot), tree.rs (NEW),
               orchestrator.rs (thins: two modes over shared services)
  context/     assembler.rs (NEW), policy.rs, state.rs,
               builder.rs (deprecated → assembler)
  eval/        metrics.rs (+recall), runner.rs (copy_tree: .git, shallow)
  tools/       surface unchanged; containment symlink-aware
  config/      + verify_model
```

No new dependencies. No new traits. No new subprocess capability exposed to the agent.

## 7. Measurement plan — every change is an arm

Frozen tree, same suite and models, ≥3 repetitions, matched task outcomes, `config_hash`-diffed. **Primary metric: task completion with quality. Cost is the constraint, reported alongside.**

| # | Change | Knob | Success criterion |
|---|---|---|---|
| 1 | Assembler + dedupe + uniform windowing | budget, dedupe on/off | tokens/run ↓, `matched` ≥ |
| 2 | Recall baseline | — | establishes the number; no feature gated on a target yet |
| 3 | Rollback + diff gate | rollback on/off | duplicate-edit class gone; rounds ↓ on hard tasks; self-reported writes retired |
| 4 | Verifier slot | `ROF_VERIFY_MODEL` ≠ vs == exec | `matched` ↑ → keep verifier; flat → retire reviewer from default |

Order is deliberate: 1 is the prerequisite for every future context idea; 2's instrument ships before the feature it would justify; 3 removes a bug class the others would otherwise mask; 4 is a decision gate, not a feature.

## 8. Triggered backlog — deferred, not deleted

Each item ships only when its trigger fires. The two most ambitious ideas in the original draft are here, first — the per-task budget dial and graph retrieval.

| Item | Trigger | Minimal form when it comes |
|---|---|---|
| **Per-task budget dial** (`Effort: low/medium/high/max`) | Arm #1 shows a static budget is the binding constraint | an enum scaling existing knobs (~20 lines); `xhigh` omitted, addable later |
| Prompt-text inference of effort | The dial proves worth tuning per task | a frozen, tier-labelled prompt corpus; prediction accuracy measured |
| `staged_scope` (now vs deferred) | A task where the split changes the outcome | reuse the existing planner; do not build a second one |
| **Graph retrieval** (symbol/import/call edges, one-hop expansion) | **Recall baseline shows touched files missing from the retrieved set** | incremental re-parse of changed files only; vector backend only if fuzzy exploration is then shown to matter |
| `ExecStrategy` trait + `PlanDirectVerify` | A third strategy worth shipping | `RoundServices` + shared `loop_one_round()` already consolidates; add the trait then |
| Adaptive tool exposure | Tool descriptions measurably consume budget | a filter over tool *descriptions*; permission never adapts |
| Tier profiles (`ContextProfile`) | ≥2 tiers measured and knob repetition is annoying | a config file is already the profile — the abstraction is indirection until then |
| Steering (confirm / ask / auto) | An interactive `rof run` exists **and** a run where human-in-the-loop changes outcomes | one `confirm: bool` + one line of stdin; eval stays `Auto` |
| `rof propose` (outer-loop proposer) | ≥5 completed arms a human is currently diffing by hand | a report-diff command; Meta-Harness's filesystem-of-candidates later |

**The honest framing:** the budget dial and graph retrieval were the two most exciting ideas in the draft, and they are exactly the two that most need a measurement before they exist. Deferral is the order, not the verdict — arm #1 gates the dial, arm #2 gates the graph.

## 9. Evidence quality ledger

| Source | Confidence | Used for |
|---|---|---|
| [DeepSeek API docs](https://api-docs.deepseek.com/quick_start/pricing/), [context caching](https://api-docs.deepseek.com/guides/kv_cache/) | Primary | flash pricing, 50× cache, full-prefix-match requirement |
| [OpenAI API docs — GPT-6 Astra](https://developers.openai.com/api/docs/models/gpt-6-astra) | Primary | astra pricing, 10× cache, native Tool search / Apply patch |
| [arXiv:2603.28052 Meta-Harness](https://arxiv.org/abs/2603.28052) | Peer-reviewed preprint | frozen-model comparability; the `rof propose` precedent |
| [agentskills.io/specification](https://agentskills.io/specification) | Primary spec | 3-level progressive disclosure (ROF implements all three) |
| [MindStudio, 2026-09-12](https://www.mindstudio.ai/blog/deepseek-v4-1-flash-benchmarks) | Third-party, cites Deep SWE / TerminalBench / Artificial Analysis | flash-vs-frontier tie on coding; state-tracking weakness |
| [vexp.dev, 2026-05-22](https://vexp.dev/blog/code-indexing-for-ai-agents-embeddings-vs-dependency-graphs-vs-rag) | Third-party engineering blog | graph-vs-vector relevance numbers — **hypothesis, not authority**; not used to justify any code in §4 |

Internal measurements (summarization 8/12→2/12, p=0.036; direct 1/3 vs pipeline 0/3; auto-poke +32% cost; findings 1–3 and the direct-mode drift, all verified in `src/` this session) carry the highest confidence of anything in this document, and are the reason §4 contains only six components.

## 10. Resolved decisions

| Q | Decision | Rationale |
|---|---|---|
| Q1 rollback primitive | **git** | exact diff + rollback in one; also gives the write gate. `git` stays off `ROF_ALLOW_CMDS` |
| Q2 SelectionFailure | **surface to the model as a turn** | costs a round, preserves the lossless invariant, uses model judgment over a rule that may drop the wrong file |
| Q3 plan sequencing | **continue; report per-task** | one failed task must not censor the rest; `compare` gains per-check resolution |
| Q4 strategy default | **`direct` + v3 machinery** | the only arm evidence favors it (1/3 vs 0/3); pipeline's 0/3 is confounded by the unwindowed-reviewer bug; quality mechanisms now live in `RoundServices`. Flip condition: arm #4, decided by a labelled report |
| Quality | **checks pass + goal-quality note** | reviewer does *not* gate the verdict — consistent with Q4 |
| Escalation | **suggest, never execute** | comparability of arms; matches Meta-Harness's frozen model |
| Tree substrate | **git in task copy, shallow when large; `git init` when not a repo** | exact rollback including untracked files; exact diff gate; one substrate also feeds recall. Snapshot alternative rejected — partial rollback is worse than none |
| Scope of v3 | **minimal core (§4) + triggered backlog (§8)** | every component justified by a measurement or a bug; speculative weight made visible before any line is written |

## 11. Open questions

- **Q5 — interactivity.** Deferred with steering (§8). When its trigger fires, decide between a real interactive prompt on the run path and a file/flag-based non-blocking form. Blocks nothing in §4.
- **Q6 — effort inference.** Deferred with the budget dial. The frozen, tier-labelled prompt corpus is the way to make it measurable rather than guessed.
- **Q7 — the reviewer's future.** Arm #4 decides. If an independent verifier moves completion, it enters as a **post-hoc verify pass**, not an inline review loop — that is where the unwindowed-evidence confound lives.
- **Q8 — resolved: git substrate, shallow when large.** Copy cost is taken off the history axis by copying `.git` at `--depth 1` (§4.2), so the concern that motivated this question is structural rather than open. What remains is verification, not a decision: confirm copy time on the largest repo in `docs/baselines/` stays acceptable, and opt into full history only if a check turns out to need ancestry.
