# Status

Last verified: 2026-09-21, on this working tree. v3 components §4.2 (tree-state
substrate + direct mode), §4.3 (structured outcomes + `RoundServices`), §4.1
(the one budget owner below the layers), §4.4 (recall), §4.6 (symlink
containment) and §4.5 (the `verify_model` routing slot) are all landed; see
`docs/ARCHITECTURE-v3.md`. This file is the running record of what is
measured, not a handoff.

## §4.1 assembler: one budget below the layers (2026-09-16)

`context/assembler.rs` owns the parts no budget saw: the file map, the
requested files and skill bodies `ImplementerAgent` appended *after* the layers
were cut, and the reviewer's `[VERIFIED FILES]` evidence, which was read whole
(≤262 KB × 5) and then head+tail-collapsed by the short layer's budget. Every
one of them is now an item with a fidelity, a dedupe key and a budget, and an
oversized `must_include` item is narrowed (halving to a 2 000-char floor)
before it is declared excess — a first-class `SelectionFailure` the caller
traces rather than a silent cut that drops the region under judgement.

The duplications are fixed at the source as well: the evidence reuses the
implementer's `file_state` instead of re-reading the same files, and the file
bodies are stripped from the artifact JSON the reviewer sees, so `[VERIFIED
FILES]` is the single copy. Both are counted in `eliminated_chars`, which folds
into `ContextMetrics` and both reports.

Coverage: 6 assembler unit tests (dedupe by key, fill-don't-optimize, window
narrowing, optional drops, excess not consuming the budget), 2 `strip_file_bodies`
tests, and `tests/loop.rs::reviewer_evidence_is_windowed_and_carried_once`,
which writes an 80 KB file with the change at the middle line and asserts the
judged region survives while the head and tail do not, and that the body is
not delivered twice.

Deliberate cuts (see `docs/ARCHITECTURE-v3.md` §4.1): the layers keep their own
policy — the ungoverned surface was below them, and the summarize path is
already measured; and `Fidelity::Summary` is dropped (a summary is an `Exact`
item). Not yet done: layers are not items, so a layer that overflows still
spends budget the assembler cannot see.

## §4.3 RoundServices + structured outcomes (2026-09-16)

`RoundServices` now holds the services both execution modes must use
identically (`cfg`, `trace`, `context`, `executor`, `verify`, `tools`) and the
methods that used to live in `run_loop` alone: `run_checks`, `skill_index`,
`skill_bodies`, `reviewer_file_evidence`, `goal_note`, `budget`,
`head_with_index`. Each loop constructs one and calls the same methods, so a
prompt part one mode forgets is a compile error against the struct, not a
silent drift.

Two bugs this fixed, both named in the spec: the direct verdict is no longer
`checks_log.contains("STATUS: FAILED")` — it is `checks_pass(&results)`, a
field read, so a check that passes while quoting failure text inside its body
no longer flips the task; and direct mode now gets the skill index, the
goal-quality note and the bounded auto-poke it was silently missing. The
auto-poke is still off by default (it stays measured, § *Goal 4 live arm*).

Per-task spend is O(1) now: `TraceSink` totals `input+output` at the `emit()`
choke point and `Budget::exceeded()` is a subtraction, replacing `tokens_since`'s
per-round rescan of the event stream.

`compare` gained per-check resolution: a moved task names the check that
flipped (`check 'cargo test' flipped: fail -> pass`), and a moved task with no
flipped check reports the empty list — that emptiness is the signal that the
gate held and the verdict moved, which has a different fix.

No `TaskOutcome` struct, deliberately: the report is a `serde_json::Value` both
the CLI and `compare` read, and `CheckResult` alone delivers what the outcome
needed. Scope and rationale are in `docs/ARCHITECTURE-v3.md` §4.3.

## §4.5 verify_model slot (2026-09-16)

`RoutingConfig.verify_model` (`None` = the executor model, i.e. self-review),
`Role::Verify` in `ModelRouter`, and `ROF_VERIFY_MODEL` in `apply_env`.
`Orchestrator` holds a separate `verify: ExecutorService` and
`ReviewerAgent::new(&self.verify)` runs on it. Zero behavior change until the
env var is set: this is arm #4's decision gate, not a feature. The eval runner
and the test helper still pass the executor clone, so the default path is
byte-identical in cost and trace.

## Direct execution experiment (2026-09-16)

The opt-in `ROF_MODE=direct` path is implemented and committed. It runs one executor
without planner or reviewer model calls, applies edits through the existing tool gate, runs every
configured check after each attempt, and includes failed-check output in the next executor prompt.
Regression coverage is in `tests/loop.rs` (`direct_mode_*`); both tests pass. The full Rust suite,
Clippy, and formatting also pass. External KDL validation remains **0 successful tasks**. A fresh
three-round run after the file-state fix received the current `src/node.rs` contents, then produced
an ambiguous patch that the gate correctly refused; it still did not pass the repository checks.
The pipeline reviewer now gets independently gated read-back content for touched files in
`[VERIFIED FILES]`, with regression coverage in `tests/loop.rs`; the reviewer also receives its
read-only tool/workdir context. A three-run planner-skipped pipeline arm measured **0/3**. A matched
three-run direct arm measured **1/3**: one run passed all configured KDL checks, while two failed on
model-side edit/retry errors. This is a signal in direct mode's favor, not yet a reliable win; the
arms are too small and variable for a default change. The reviewer arm's unchanged-tree rejection
still demonstrates improved verdict correctness, not completion rate.

## §4.2 tree-state substrate (2026-09-16)

`docs/ARCHITECTURE-v3.md` §4.2 is implemented and **committed**: git is the tree-state
substrate of every task copy. `src/engine/tree.rs` (`TreeService`: `ensure`/`baseline`/`rollback`/`diff`, plus
`copy_git_state`) drives it; `copy_tree` now ships `.git` (shallow `--depth 1` above 8 MiB, verbatim
below, `git init` + one commit for a non-repo source) and keeps `target/` excluded. The write gate
no longer reads the model's self-reported `writes[]` — it counts `git status --porcelain` (diff
alone misses files an attempt created), with `diff --stat` for the evidence line; the same names are
the substrate for the §4.4 recall metric. A failed attempt is rolled back (`checkout -- . &&
clean -fdq`) only when a retry follows, so the final tree stays readable. `file_state_evidence` now
reports a landed change as rolled back, without its post-attempt text; a refused patch still hands
over its text. `git` is never on `ROF_ALLOW_CMDS` and `.git` is denied by every file tool and hidden
from listings. The opt-in direct mode (`ROF_MODE=direct`) ships in the same commit — it shares the
tree service, so a fabricated split was not worth it.

## §4.4 recall + §4.6 symlink containment (2026-09-16)

Both done and committed after this line was written.

- **Recall** (`ContextMetrics::changed_files`/`recalled_files` + `recall()`): the change set comes
  from §4.2's per-task `changed_files` — ground truth, not the artifact's self-reported `file_state`.
  Prints per task (`recall=recalled/changed`) and aggregates in `rof compare` as a true ratio, not a
  mean of per-task ratios, so a task touching 1/1 cannot average away one touching 0/100. This is
  the baseline that gates the graph-retrieval and budget-dial backlog items; no retrieval feature
  ships before it has a number.
- **Symlink containment** (`tools::symlink_safe`): every read/list/write resolves its containing
  directory against the canonical root and resolves a symlinked final name. The root itself is the
  trust anchor (exempt, so `path: "."` still lists); a link resolving back inside still reads.

Full suite (121 tests), Clippy and formatting clean.

stage 3 (programmable state: `state/` module, `~/.rof/state.json`,
`state.propose` behind the gate, `rof state approve|reject`) — but see the ranked list at the bottom
first, because two items now outrank it. Stage 2's acceptance question is **answered** (arming the
mid layer costs matches; the default stays) and the stage that ran after it is **reviewed and
cleaned**: of its seven proposed goals only goal 4 (goal-quality + auto-poke) is real, and it is
built, tested and off by default — its first live arm then measured the auto-poke half and left it off
(+32% cost for a difference inside the instrument's noise; § *Goal 4 live arm*). Everything else in that
stage was un-compiled skeleton files; they are deleted and the designs stay in `docs/PLAN-harness-v2.md`.

Read in this order: § *Stage 2 acceptance* (the measurement, 2026-09-15) → § *Goal quality* (what was
faked, what is real) → § *Verified state* → the ranked *Good next steps* at the bottom.

## Stage 2 acceptance: arming the mid layer costs matches (2026-09-15, measured)

Both arms were already on disk from the 2026-09-14 session; nobody had read them. `hard-two`,
`--jobs 1`, `ROF_PLANNER=skip`, 6 runs per arm, **same tree** (`4e2da76`, the bounded-summary fix),
`exec/ctx = deepseek-chat`, `config_hash` differs between the arms (`4a601062…` vs `debe343e…`) so the
only variable is `ROF_SUMMARIZE_AT`. Reports: `~/rof-runs-s2-off/`, `~/rof-runs-s2-armed/`.

| arm | matched | metrics-method | add-retriever-test | in tokens/run | $/run |
|---|---|---|---|---|---|
| off (defaults, mid at 0.8 → inert) | **8/12** | 4/6 | 4/6 | 47.8k | 0.0040 |
| armed `ROF_SUMMARIZE_AT=0.6` | **2/12** | 2/6 | 0/6 | 43.1k | 0.0061 |

Fisher two-sided p = 0.036 overall (`add-retriever-test` 0/6 vs 4/6, p = 0.061; `metrics-method` p =
0.57). The mechanism is not broken — the armed arm fired exactly as designed (2 summarize calls per
run, `layer_summaries [0,4,0]`, `layer_truncations [0,0,0]`) — it is **harmful on these tasks**: the
mid layer carries the whole file the task then edits (retrieval gives a named file whole-file
treatment up to 12k chars), so a summary replaces the anchor text the implementer patches against.
Cost rose 53% per run while matches fell.

**Conclusion: the 0.8 default stands; `0.6` is a measured loss, not a candidate default.** Any future
claim that "stage 2 saves tokens" must name the arming and a suite where the mid layer is filler
rather than the edit target.

`~/rof-runs-s2-repo-{before,off,armed}` (9 reports, 3 runs each) are **not data**: every run used
`deepseek-chat:free`, all 6 tasks failed with `all models in fallback chain failed`, 24 model errors
per run, 0 tokens, $0. They were moved to `~/rof-runs-junk/` (with a README explaining why) so they
cannot be read as a before/off/armed comparison later.

## Goal 4 live arm: auto-poke (measured 2026-09-15, 6 runs/arm — the default stays off)

The design was pre-registered in this file before the runs and was not changed afterwards: `hard-two`,
`--jobs 1`, `ROF_PLANNER=skip`, empty skill store, frozen worktree `~/rof-arm-poke` at `0cd40db`, 6 runs
per arm, arms = defaults vs `ROF_AUTO_POKE=yes`. Reports: `~/rof-runs-poke-off/`, `~/rof-runs-poke-on/`.

| arm | matched | metrics-method | add-retriever-test | auto_pokes | in tokens/run | $/run |
|---|---|---|---|---|---|---|
| off (default) | 9/12 | 5/6 | 4/6 | 0 | 49.8k | 0.00402 |
| `ROF_AUTO_POKE=yes` | 10/12 | 6/6 | 4/6 | 4, in 3 of 6 runs | 66.4k | 0.00530 |

Fisher two-sided p = 1.0 (the baseline itself is 3 runs at 2/2 and 3 at 1/2 — a one-task difference is
this instrument's noise).

**Verdict: the default stays off**, by the rule registered above: the matched difference is not
distinguishable from noise and it is paid for with +32% cost and +33% input tokens per run.

The mechanism is not inert, and the traces say exactly where it works. Of the 4 pokes, **2 converted a
failing task into a pass** — both on `metrics-method`, the task whose failure shape is "the artifact is
prose / the write never landed" — and 2 did not (both on `add-retriever-test`, which fails for a
compile-shaped reason no extra round can fix). So the lever is narrow and its honest description is: one
extra round helps *"the model described the change but did not apply it"*, and does not help *"the model
applied a wrong change"*. Cost per conversion: a poked run runs 1.3× ($0.0054–0.0100 against $0.0040).

**If this feature is ever reopened, the only reason is the narrower trigger**: poke on the
`expect_writes && writes == 0` rejection only, instead of every cap exhaustion — 2 of 2 conversions came
from that half, and the other half paid for nothing. Measuring that is a new arm, not a default flip.

**The goal-quality half stays unmeasurable on these suites**, and that is a finding, not a gap: the note
goes into the *planner* prompt only (so a `ROF_PLANNER=skip` arm cannot see it), and hard-two's goals are
anchored so the check never fires on them (0 `GoalQuality` events in 12 runs). Its only live effect today
would be on the `expect_writes: false` explain tasks of `repo-tasks`, where the flag is a false positive
(§ *Goal quality*, finding 2). Fix the false positive before any quality arm.

## Earlier: stage 2 as built (2026-09-14, night)

The design points and the six deviations from the plan's sketch are below in "Stage 2" — they still
hold. Two corrections to that section: the armed arm **did** finish (see § *Stage 2 acceptance*
above), and the `layer_summaries`/`layer_truncations` metric fold it calls "the remaining step" is
**wired** (`eval/metrics.rs`, folded per task and shown by `rof compare`).

**Two traps this session paid for, both now known:**

1. **`live-eval.sh` rebuilds the release binary at the start of every run** (`cargo build --release`).
   Editing the tree while an arm is running therefore changes the binary *between runs of the same
   arm*. Freeze the tree first — `git worktree add ~/rof-arm-reasoner <commit>` is how the arms below
   were run — and never edit the arm's own tree mid-arm.
2. **An arm's driver script must be checked against the label.** The first "reasoner" arm ran with
   `exec=deepseek-chat` for six runs because the driver forgot `ROF_EXEC_MODEL`; the report's
   `label.exec_model` is what caught it. Every arm now gets its labels read back before its numbers
   are believed.

**Measured-claim rule for every arm below:** 3+ runs per arm, same `--limit`, same models, compare
per-task matched sets — one 6-task run swings ±2 tasks.

## Stage 2: per-layer context policy (2026-09-14, night — built; acceptance arm not run)

| Deliverable | Where |
|---|---|
| `LayerKind`, `LayerStrategy`, `LayerPolicy`, `ContextPolicy`, `LayerReport`, `SummaryStat` | `src/context/policy.rs` |
| `ContextBuilder::{plan, plan_summarized}` + content-keyed summary cache | `src/context/builder.rs` |
| Per-layer folding → `layer_summaries`/`layer_truncations`, summarizer `ModelCall`s | `engine/orchestrator.rs` |
| `AppConfig.context: Option<ContextPolicy>` + `context_policy()` (derives from `budgets`) | `config/mod.rs` |
| `ROF_BUDGET_LONG\|MID\|SHORT`, `ROF_SUMMARIZE_AT` (long+mid; `0.0`/`off` disarms) | `main.rs` |
| `layer_summaries` / `layer_truncations` in `ContextMetrics` and as `rof compare` rows | `eval/metrics.rs`, `eval/compare.rs` |
| Offline acceptance: 9 tests (threshold, arming, cache reuse, fallback, `Raw`, indexing, config) | `tests/context_policy.rs` |

Design points, and all six deviations from the plan's sketch are recorded in
`docs/PLAN-harness-v2.md` § *Stage 2 as built*. The three that matter for anyone touching this code:

- **Order inverted.** v1 summarized the whole prompt *because* it had overflowed, then cut anyway.
  Now a layer over its `summarize_at` share is compressed by the cheap Context LLM *before* the cut,
  the compression is cached by the layer's own text, and truncation is what a *failed* call falls
  back to. The old whole-prompt path is deleted, not kept alongside.
- **The cache lives in `ContextBuilder`, not `CtxState`.** The orchestrator rebuilds `CtxState` every
  round, so a cache inside a per-round value caches nothing — which is precisely the case it exists
  for (a retry must not buy the same summary twice).
- **Defaults are asymmetric on purpose**: mid (retrieval, the only layer whose size follows the repo)
  armed at `0.8`; long (the stable head = the provider's cached prefix) and short (artifact + checks +
  refusals = the evidence a reviewer judges) at `0.0`, never.

Offline: `cargo test` **84 passed** (73 before), clippy 0 warnings, fmt clean, committed as `98ae554`.

**Live smoke** (`hard-two --limit 1 --jobs 1`, `ROF_PLANNER=skip`, `ROF_SUMMARIZE_AT=0.6`
→ trace and report in `~/rof-runs-s2-smoke/`):

- Exactly one summarize call, on the **mid** layer: `layer_summaries [0,1,0]`,
  `layer_truncations [0,0,0]`, 3254 in / 207 out tokens traced as
  `ModelCall{agent:"summarizer"}`, `metrics-method` passed in round 1, est $0.00289.
- **The default threshold does not fire on this suite.** Retrieval puts ~11.9k chars in the mid layer
  (~3.0k tokens) against a 16k-char cap — ~76% of budget, i.e. under the 0.8 default. `0.6` is the
  first setting that fires, which is why the acceptance arm below uses it. Any claim that "stage 2
  saves tokens" must name the arming; at defaults on these suites the stage is deliberately inert.

**Next, in this order:**

1. **Bound the summary request.** DONE (`4e2da76`). `plan_summarized` passes
   `(raw.chars() / 16).max(64).min(pol.budget)` as `max_tokens` (was `pol.budget`, allowing
   expansion). Pinned by `tests/context_policy.rs::summarize_request_is_bounded_by_half_the_layer_estimate`
   (captures `req.max_tokens` via `CountingClient::last_max_tokens`, asserts `<= 375` on 6000-char
   layer). `cargo test` 85 passed (was 84), `clippy` 0, `fmt` clean.
2. **The acceptance arms.** DONE — see § *Stage 2 acceptance* at the top of this file: off 8/12 vs
   armed 2/12 on the same tree (Fisher p = 0.036), so the 0.8 default stays and `0.6` is recorded as a
   measured loss. The off arm reports show `summarized: false` / `summarize_calls: 0` for both tasks
   per run (defaults inert at these sizes, as predicted). The repo arm's 9 reports are junk (all three
   arms ran on `deepseek-chat:free` and failed every task with `all models in fallback chain failed`);
   they are quarantined in `~/rof-runs-junk/`, so `~/rof-s2-repo.sh` still has **no** usable run —
   re-run it when a working (non-`:free`) endpoint is available.
3. **The acceptance arms** — scripts kept for reuse; the numbers are recorded above.
   Both arms of each pair are the *same* tree; the only variable is the arming, because at these
   context sizes everything else stage 2 changed is inert:

   ```bash
   bash ~/rof-s2-hard2.sh   # hard-two, ROF_PLANNER=skip, 6 runs off + 6 runs ROF_SUMMARIZE_AT=0.6
   bash ~/rof-s2-repo.sh    # repo-tasks --limit 6 --jobs 2, 3 runs each: before(a0d47a4 worktree) / off / armed
   ```

   The plan's acceptance is *matched non-inferior and cost/tokens down, summarize calls visible with
   their tokens*. Read it through `rof compare` against the arms' reports rather than eyeballing, and
   remember the mid layer carrying whole retrieved files is what the implementer acts on: if arming at
   0.6 costs matches, the honest outcome is "the default (inert) stays, and the aggressive setting is
   a measured loss" — not a new default.
3. **Write the result into this file and into the plan**, then start stage 3 (programmable state:
   `state/` module, `~/.rof/state.json`, `state.propose` behind the gate, `rof state approve|reject`).

Artifacts left in place for the arm: `~/rof-arm-reasoner` (worktree at the pre-stage-2 tree `a0d47a4`,
the "before" side), `~/rof-arm-skills` (empty scratch skill store every arm pins via
`ROF_SKILLS_ROOT`), `~/rof-runs-s2-smoke` (the smoke trace + report). Task copies are deleted after
each run by `measure-arm.sh`; `~/rof-runs-*/` holds only reports and traces.

## Executor-model arm: a stronger executor did not pay (2026-09-14, night — measured, 6 runs/arm)

Every residual failure class in every earlier round was model-side, so the ranked next lever was a
stronger executor, not more prompt work. Two arms, identical frozen tree (`a0d47a4`), suite
`hard-two`, `--jobs 1`, planner **on** (the shipped default), 6 runs each, one variable:
`ROF_EXEC_MODEL`.

| arm | matched | metrics-method | add-retriever-test | est $/run | in/run | out/run | wall/run |
|---|---|---|---|---|---|---|---|
| `deepseek-chat` (6 runs) | **3/12** | 3/6 | 0/6 | $0.0107 | 101k | 5.1k | 87 s |
| `deepseek-reasoner` (6 runs) | 1/12 | 1/6 | 0/6 | $0.0075* | 49k | 7.3k | 83 s |

\* The est column is one flat price row (deepseek-chat off-peak, 0.15/0.003/0.60 per Mtok) applied to
every call regardless of model, so it is **not** a cross-model number: `deepseek-reasoner` is priced
above `deepseek-chat` and its own rates were not verifiable this session (no web access), so its real
spend is higher than the table. Treat tokens and matched as the comparable quantities.

- **No improvement, and the sign is against the stronger model**: 1/12 vs 3/12 (Fisher p≈0.59 — six
  runs per arm cannot separate them, so this is "no evidence of a gain", not "proven worse"). The
  failures are the *same classes*: the reasoner's `metrics-method` run invented a
  `TraceEvent::ToolCall` field set (`E0063: missing field agent`), exactly the behaviour chat shows,
  after spending ~43% more output tokens per run to get there.
- It did use fewer rounds (1.8 vs 3.0 mean on `metrics-method`) and fewer input tokens (49k vs 101k) —
  so it is not confused, it is just not more correct, and reasoning tokens are billed as output.
- Raw per-run rows (matched, per-task, tokens, latency, labels):
  `docs/baselines/2026-09-14-executor-model-arm.json`; reports in `~/rof-runs-chat-baseline/` and
  `~/rof-runs-reasoner/` (not committed).
- **Consequence**: do not spend the next round shopping for a bigger executor. The remaining levers
  are structural — make verification happen *before* the write (the model asserts facts about code it
  has not read), or give the reviewer the `fs.read` it already holds in the policy. Independent
  review by a *second* model remains untested (this arm changed the executor for all roles at once).

## Stage 1: skills — SKILL.md as procedural memory (2026-09-14, night — done)

| Deliverable | Where |
|---|---|
| SKILL.md store: frontmatter parser, index, proposals, approval | `src/skills/mod.rs` |
| `skills.list` / `skills.view` / `skills.manage` behind the gate | `src/tools/skills.rs`, grants in `config/mod.rs` |
| Index in the stable head; bodies injected when the task names the skill | `engine/orchestrator.rs` |
| Artifact keys `skills: [{op,…}]` and `skill_views: […]`; prompt nudges | `agents/implementer.rs`, `agents/reviewer.rs` |
| `TraceEvent::SkillOp`, `SkillMetrics` (report + `rof compare` rows) | `obs/trace.rs`, `eval/metrics.rs`, `eval/compare.rs` |
| `rof skills list\|show\|approve\|reject`; `ROF_SKILLS_ROOT`, `ROF_SKILLS_POLICY` | `main.rs` |
| Live instrument: three tasks sharing the unit-test procedure | `eval/suites/skill-tasks.json` |

Design points worth knowing before changing anything (the plan's §2.1 sketch is updated to match):

- **Writes are proposals.** `skills.manage` validates an op and writes
  `~/.rof/proposals/skills/<id>.json`; `rof skills approve <id>` applies it. `skills.policy` can be
  `direct` (trusted loop) or `readonly` (frozen).
- **The skills root is not on `allowed_dirs`**, deliberately: the plan's shortcut would have let the
  implementer's `fs.write` bypass the Propose policy. The skill tools address skills by name and
  enforce their own containment; `tests/skills.rs` pins both halves.
- **The index is fetched per agent through the gate** (`skills.list` run *as* that agent by the
  harness), so the grant matrix decides who sees it. Those reads emit `SkillOp{op:"list"}`, never
  `ToolCall` — folding prompt-construction reads into `tool_accuracy` would inflate it silently.
- **Models reach the tools through the artifact** (this crate has no tool-call loop):
  `skills: [{op,…}]` → `skills.manage`, `skill_views: […]` → `skills.view` in the same bounded extra
  turn `reads` already used. The harness overwrites `agent`/`rationale` before the call.
- **`reused` = bodies delivered, `viewed` = bodies asked for.** Two reuse paths, counted apart.

Acceptance, offline: `tests/skills.rs` (9 tests) drives stub agents through create → propose →
approve → a later task's prompt carrying the body (injected, and on request through the gate); the
metrics fold only successful ops; `cargo test` 73 passed, clippy 0 warnings, fmt clean.

Acceptance, live (2 suites, 5 runs, 14 task-runs; DeepSeek `deepseek-chat`):

| Run | Suite | matched | skills (listed/proposed/reused) | est $ |
|---|---|---|---|---|
| `skill-a` | skill-tasks (empty store) | **3/3**, all round 1 | 0 / **0** / 0 | $0.0039 |
| `skill-hard` ×2 | hard-two (multi-round tasks) | 0/2, 1/2 | 0 / **0** / 0 | $0.0037–0.0066 |
| `skill-b` | skill-tasks (approved skill, planner skipped) | 2/3 | **9** / 0 / **6** | $0.0046 |
| `skill-c` | skill-tasks (approved skill, planner on) | 2/3 | **9** / 0 / **11** | $0.0096 |

- **The new suite runs clean**: 3/3 with no skill in the store, no writes outside the task copies.
- **No agent proposed a skill on its own, in any of the 11 task-runs** (runs A, hard ×2, and the
  3 task-runs inside `skill-b`). The nudge's trigger is "a workflow that took more than one round
  *and worked*", and no measured run had a multi-round success — `metrics-method` now usually passes
  in round 1, and when it doesn't, it fails. So the invitation never became eligible: this is a
  nudge-design finding, not a plumbing failure.
- **The propose path works when the goal asks for it.** One `rof run` goal ("record how a unit test
  is added … as a skill named `add-a-unit-test`", `ROF_EXPECT_WRITES=no`, scratch workdir, $0.00125)
  produced exactly one manage op → proposal `1789393370-2811`, store untouched, and the reviewer also
  emitted a `SKILL:` line as nudged. `rof skills approve` applied it; `rof skills show` reads it
  back. That skill is now in `~/.rof/skills/` and is what `skill-b`/`skill-c` reused.
- **Reuse is real and measurable.** With the approved skill in the store, every task delivered the
  index to all three agents (9 `list` events) and the body to each agent the task ran under: 6
  deliveries with the planner skipped (implementer + reviewer — no phantom events for an agent that
  never ran) and 11 with it on (planner + implementer + reviewer, once per plan task, and one goal
  planned as two tasks). Matched held at 2/3 in both: each run's one failure was a model-side round
  limit, not anything the skill did.
- **What the channel costs**: `skill-b` spent 37.2k input tokens against run A's 21.6k for the same
  suite (+72%, ~$0.0007) — three index lines × three agents × three tasks, plus 109-byte bodies.
  Cheap at this store size, and the reason the index is capped and bodies are opt-in.

**Two hygiene findings from running live arms against a working tree, both now fixed:**

1. A task's `cargo test` check failed at `tests/skills.rs:211` during the `skill-hard` arm — the
   harness copies the workdir *at run time*, so an in-flight edit in this repo is part of the arm's
   input, and the failure showed up in the reviewer's evidence as though it belonged to the task
   (`add-retriever-test` was failing for its own reason anyway, so the matched set did not move). The
   same test is green in the committed tree. **Commit or freeze the tree before an arm.**
2. `AppConfig::default()` means `~/.rof/skills`, so `tests/eval_suite.rs` was reading whatever skill
   store the machine happened to have — a hidden input that changes prompts and metrics. Those tests
   now pin the store to a scratch path (`test_cfg()`); a home-directory store is an input like any
   other, and tests must not have one.

**Bug found by this stage's parallel runs, fixed:** two tasks' trace lines collided into one corrupt
line on a `--jobs 2` run (`writeln!` on a `File` issues two writes per event — body then newline —
and two forks raced them). The sink now shares one file lock across forks and writes each event as a
single buffer; covered by `obs::trace::tests::concurrent_forks_write_one_line_per_event`. The same
race had been silent in earlier arms (`skill-a`'s trace parsed clean); a corrupt trace is invisible
until something parses it strictly, which the live-arm evidence here finally did.

**Next lever for this stage (not done, ranked)**: (1) make the invitation match reality — the nudge
fires only after a retry, so a task that goes green in one round teaches nothing even when it does
something reusable; either drop the round condition or have the reviewer's `SKILL:` line become an
implementer action in the *next* task rather than a passive note; (2) measure a store with 3–5 skills
to see whether the index stays cheap and whether bodies actually change behaviour (matched), not just
tokens.

## Stage 0: labels, context metrics, `rof compare` (2026-09-14, night — done)

Additive and behaviour-free; no live tokens (every number below is a stub run or a committed file).

| Deliverable | Where |
|---|---|
| `RunLabel` (`git_head`, `config_hash`, `suite_hash`, `ctx_model`, `exec_model`, `harness_version`) on every report | `src/eval/runner.rs`, `SuiteReport::label` |
| Per-task `ContextMetrics` (`retrieved_files/chars`, `referenced_chars`, `relevance_proxy`, `summarize_calls/tokens`, `truncated_views`) | `src/eval/metrics.rs`, folded from the orchestrator's return value |
| `rof compare a.json b.json` | `src/eval/compare.rs` + `main.rs` |

- Hashes are in-house **FNV-1a-64 over canonical JSON**: `config_hash` hashes the `rof config` dump
  verbatim (so it can be recomputed by hand), `suite_hash` the suite *as it ran* — a `--limit 6` run
  labels the six tasks it executed. No `sha2`: the report only needs identical-vs-different, and the
  crate stays dependency-free.
- New evidence rides the orchestrator's existing return value (`retrieved: [{path, chars}]`,
  `summarize_calls`, `summarize_tokens`, `truncated_views`), counted where the summarizer is already
  invoked; the runner folds it per task. `relevance_proxy` is a **proxy**, never "context
  relevance": the share of retrieved chars whose file the task then touched
  (`file_state[].path`), so it is 0 for a task that retrieved context and wrote nothing.
- `ContextMetrics` deliberately omits the plan's `layer_truncations`/`layer_summaries`: they have no
  emitter until stage 2's `LayerReport`, and a metric fed by nothing reads as a measured zero.
- Labels are `#[serde(default)]`, so reports written before this round still load: `rof compare`
  against the committed `docs/baselines/2026-09-14-deepseek-chat-6.json` says "unlabeled (report
  predates stage 0)" and warns that it records no retrieved context — instead of printing zeros that
  look like measurements.
- `rof compare` prints the label diff, matched totals, every task that moved (gain / loss /
  only-in-one, with the losing side's feedback, rounds and context columns) and metric deltas
  (cost, in/out tokens, cache hit rate, tool accuracy, retries, aborts, model errors, latency,
  retrieved/referenced chars, proxy, summarize tokens, truncated views). It warns when the two
  reports are not the same task set (different `suite_hash`) or config.

Acceptance, measured:

- Same tree (a worktree at `c9b4cbf`), old binary vs new binary: **identical** per-task results and
  identical aggregate — `matched 2/2`, `in/out 26471/840`, `implementer=19742 planner=6077
  reviewer=652`; the report JSONs are byte-equal modulo the two added fields. The live `repo-tasks`
  suite was not re-run — stage 0 is a zero-token stage by design, and the loop's only touch is
  counting views the builder already reported as truncated. Run `rof compare` on the next live arm's
  report against the previous one; the instrument exists for exactly that.
- `cargo test` **57 passed, 0 failed** (was 52; five stage-0 tests added in `tests/eval_suite.rs`),
  `cargo clippy --all-targets` 0 warnings, `cargo fmt --check` clean.

## Verified state

| Check | Result |
|---|---|
| `cargo test` | 89 passed, 0 failed (52 before stage 0, 57 before stage 1, 73 before stage 2, 85 after it, +4 goal-quality unit tests) |
| `cargo clippy --all-targets` | 0 warnings |
| `cargo fmt --check` | clean |
| `cargo build --release` | green, `target/release/rof` ≈ 5.9 MB |
| Live baseline (DeepSeek `deepseek-chat`, `repo-tasks` first 6, `--jobs 2`) | **matched 3/6 (50%)**, wall 69 s, est $0.011, tool accuracy 95%, cache-hit input 50%, 87k in / 6.4k out |
| Live end-to-end | passes, with real file writes + `cargo check`/`cargo test` evidence in the trace |
| Measured cost | ~$0.002–0.005 per passing task |

Baseline artifact: `docs/baselines/2026-09-14-deepseek-chat-6.json` (the `--report` dump of the
run; trace in `rof-runs/`, not committed). Two independent runs of the same 6 tasks produced
the same match set, so the harness is stable at this sample size.

Per task, and both failure modes are model-side, each with the evidence in its MISMATCH line:

| Task | Result | Why |
|---|---|---|
| tool-count | pass, 1 round | additive method, `cargo check` green |
| procrun-doc | pass, 1 round | doc replacement, `cargo check` green |
| trace-doc | pass, 1 round | doc insertion, `cargo check` green |
| doc-comment | fail, 2 rounds | patch search string guessed wrong → `fs.patch` refused it, 0 writes |
| add-retriever-test | fail, 2 rounds | guessed the `#[cfg(test)]` module text; search string not found |
| metrics-method | fail, 2 rounds | rewrote the file instead of adding a method, clobbered `EvalReport` → `cargo test` exit 101 |

The pattern worth acting on next: the failures are one behaviour — writing a patch without
seeing the file it edits. The harness caught each honestly (refused patch, or compile evidence);
it did not prevent the waste.

### Retrieval change: measured, no movement (2026-09-14)

The retriever now leads with files the goal names by path, gives a named file whole-file
treatment up to 12k chars, and beyond that a window centred on the symbol the goal names —
verified offline to put the anchor text of all four `repo-tasks` edit goals into the context
(probe was throwaway; the mechanism is covered by `tests/retrieval.rs`). Enabling a larger
per-file cap for named files alone was not enough: the window must centre on the *definition*,
because the symbol is mentioned long before it is defined.

End to end it did not move the number: 2 runs before, 6/12 matched; 3 runs after, 8/18. At
n=6 a run swings ±2 tasks (the same 50% rate has been observed with three different match
sets), so the effect is below the noise floor of one run. **A 6-task single run is not an
A/B instrument.** Use ≥3 runs per arm or the full 20-task suite before attributing anything.

Also fixed in that round: `add-retriever-test` demanded a test "inside the existing
#[cfg(test)] module" of `retriever.rs`, which has no such module — the task was unsatisfiable
as written and burned two rounds in every run.

### Refused-patch re-read: built, fires, no movement measured (2026-09-14)

What changed: a refused `fs.patch` now travels with the file's current text. The implementer
returns `refused[]` (`path`, the tool's `why`, and the file windowed on the patch's own anchor via
the retriever's `window_on`), and the orchestrator appends it to the retry round's short-term layer
as `PATCH REFUSED for <path>: <why>` plus that content — the "carry the last check output" rule
applied to the file the patch missed. Covered end to end by
`tests/loop.rs::a_refused_patch_hands_the_file_to_the_retry`.

It fires where the failure is: with the planner on, traces show a refused `fs.patch` immediately
followed by the harness re-read (10 hits across 2 runs; 0 in the pre-change traces). With
`ROF_PLANNER=skip` it barely fires at all (0 refusals in 3 runs) — because there the goal names the
file and retrieval already hands it over whole.

One of those re-reads crashed a task and found a real bug: a patch anchor with a word starting in a
multi-byte character (`λ…`) hit `w[1..]` in the retriever's hint extraction — the same class of
error as `ContextBuilder::fit`, which cut each context layer at raw byte indices. Both now cut on
char boundaries, covered by `tests/context_edges.rs`. The harness contained it: the panic became
one MISMATCH line (`task join failed`) instead of losing the run.

Measured (`repo-tasks --limit 6 --jobs 2`, 3 runs per arm; raw per-run results in
`docs/baselines/2026-09-14-refused-patch-arms.json`):

| arm | before | after | per-task |
|---|---|---|---|
| planner on (as configured) | 11/18 (`2231aad`) | 9/18 (`c4ecf55`, mechanism only) | same failures, plan-count variance moved cost (see below) |
| `ROF_PLANNER=skip` | 13/18 (`2231aad`) | 12/18 (this tree) | `metrics-method` 0/3 both, `add-retriever-test` 1/3 both, one run of `procrun-doc` flipped |

No movement: both arms are the same two tasks failing, in the same way. The remaining failures are
not "the file was missing from context": `metrics-method` writes assertions against a
`TraceEvent::ToolCall` shape it invented (`E0559`: fields that do not exist, then `E0592`
duplicates), `add-retriever-test` appended a second `mod tests` (`E0428`) and in one run wrote a
literal placeholder (`<<<KEEP EXISTING FILE, APPEND MODULE>>>`) into the file, `procrun-doc`
emitted a patch hunk as prose. The model does not lack the text — it asserts facts about the code it
never checked. Next lever: let the reviewer *verify* (it holds `fs.read` in the policy but the
orchestrator hands it `tools: None`), not more context.

### Retry file state: duplicates gone, 13/18 → 15/18 (2026-09-14, later)

The refused-patch evidence above was the right mechanism on the wrong half of the problem. A probe
of what retrieval actually hands the implementer for the two goals that fail in every arm
(throwaway `tests/probe_retrieval.rs`) showed the *edited* file already arrives whole — `metrics.rs`
at 6874 chars of 6874, `retriever.rs` at 11393 — and `metrics.rs` even contains a correct
`TraceEvent::ToolCall` construction in its own test module. The files were never the gap.

The gap is that **the mid-term retrieval is a pre-round snapshot**: a retry that just applied an
edit still sees the file as it was, decides the change is missing, and applies it again. That is
where the duplicate definitions came from (`E0592` duplicate method, `E0428` second test module,
`E0062` duplicate field) — 4 occurrences in the before arm of the focused suite.

What changed: every path the artifact touched now comes back with its current text — labelled
`applied`/`rewritten`/`refused` — and the orchestrator hands that to the retry round as
`FILE <path> (applied by your previous round — this is its content NOW, do not apply the same change
again)`. Covered by `tests/loop.rs::an_applied_change_is_handed_to_the_retry_as_it_is_now`.

Focused instrument added: `eval/suites/hard-two.json` — the two tasks that fail in every measured
arm, `--jobs 1`, ~40 s and ~$0.004 a run. Both arms 6 runs (12 task-runs each):

| arm | metrics-method | add-retriever-test | total | E0592 |
|---|---|---|---|---|
| before (`2231aad`) | 0/6 | 3/6 | 3/12 | 4 |
| after | 0/6 | 5/6 | 5/12 | 0 |

Full suite (`repo-tasks --limit 6 --jobs 2`, `ROF_PLANNER=skip`, 3 runs per arm):

| arm | matched | runs | cost/run |
|---|---|---|---|
| before (`2231aad`) | 13/18 | 4, 5, 4 | $0.0062 |
| after the refusal-evidence round | 12/18 | 4, 4, 4 | $0.0055 |
| after this round | **15/18** | 5, 5, 5 | $0.0074 |

Every run improved, `add-retriever-test` went 1/3 → 3/3, and the duplicate class disappeared
(E0592 4 → 0 in the focused arm, no `E0592` anywhere in the full arm; `E0428` count 1 in both). The
sign test is not decisive on its own (5/6, p≈0.11) — the case rests on the two instruments agreeing
plus the mechanism being visible in the traces (every applied patch is followed by the harness
re-read) and the duplicate class being gone. Cost rose ~20% per run for the extra evidence.

`metrics-method` is 0/6 in both arms and 0/9 across every arm so far, for a reason no amount of
context fixes: it writes a new test helper with an invented field set for `TraceEvent::ToolCall`
while the correct construction sits in the test module of the very file it is editing, and two runs
wrote malformed Rust (`unexpected closing delimiter`). That is a read/verify behaviour, not a
context gap.

### Read-request turn: the model can now unblock itself (2026-09-14, evening)

Every failure class left is model-side, and the reviewer kept prescribing the fix the implementer
had no way to perform: "Do not guess: read `src/obs/trace.rs` to get the exact `ToolCall` field
list" — one call, no tools, so it guessed (`E0559`/`E0063` on every arm). The artifact can now emit
`{"reads": [path]}` with no patches and no writes and gets **one bounded extra turn** with those
files in the prompt. The request goes through the same policy gate as every other tool call, so a
model-chosen path cannot escape the root; at most one extra call per round.

Live proof (trace `rr-after-1`, `metrics-method`, round 2): model asks (`out=118` tokens) →
`fs.read` of the requested file → model writes with the file in hand (`in=13517`) → `fs.patch ok` →
**pass**. That is the first `metrics-method` pass in 15+ runs across every arm.

Measured on the focused suite (6 runs per arm, 12 task-runs; before = `03534ad`):

| arm | metrics-method | add-retriever-test | total | cost/run |
|---|---|---|---|---|
| before (file-state evidence) | 0/6 | 5/6 | 5/12 | $0.0046 |
| after (read request) | 1/6 | 6/6 | 7/12 | $0.0039 |

Full suite (3 runs per arm): 14/18 vs 15/18 for the previous round (`runs 5,4,5` vs `5,5,5`) —
flat within the noise floor, cost $0.0074 → $0.0066. So: the mechanism unblocks a task it could not
previously touch and is cheaper, but the aggregate move is small and not yet separable from noise.

The model asked in 4 of 6 focused runs; only 1 converted. The residual is visible in one trace
(`rr-after-2`): the ask fires, the file arrives, and the model still writes a patch anchor that does
not match (`search string not found`) — it quotes the file it *expects* rather than the one it was
just handed — and with `max_review_rounds: 2` there is no third round to use the refusal evidence
that is already waiting. Two candidate next arms: a third round, and anchor-quoting guidance in the
implementer prompt.

### metrics-method moves: 0/9 → 5/9 (2026-09-14, night)

Two levers were measured as separate arms against the read-request tree (`7/12` on hard-two,
metrics-method 1/6), then a bug in the evidence path was fixed and measured:

| arm | hard-two | metrics-method | add-retriever-test | cost/run |
|---|---|---|---|---|
| base (read request) | 7/12 | 1/6 | 6/6 | $0.0039 |
| arm A: `ROF_MAX_ROUNDS=3` | 6/12 | 3/6 | 3/6 | $0.0070 |
| arm B: copy-the-anchor prompt | 7/12 | 3/6 | 4/6 | $0.0047 |
| arm C: keep the last read per path | **8/12** | **3/6** | 5/6 | $0.0045 |

Arm A and arm B move the same task by the same amount and give back runs on the other, so neither
is a win on the total; the rounds arm costs 80% more per run and is not adopted. Arm B's prompt
change is recorded in the baseline note (not in master — equal totals, +20% cost).

Arm C is a bug fix in the round before's evidence: a file patched twice in one artifact produced two
`file_state` entries and the 12k window kept the *first* (stale) read, which is exactly what
re-created the duplicate-definition class once rounds were raised — the retry was handed an old file
under a fresh-looking label. Dedupe by path, last read wins.

On the full suite (3 runs per arm) the same tree is the best recorded and the first that moves the
hard task off zero:

| tree | matched | metrics-method | add-retriever-test | cost/run |
|---|---|---|---|---|
| `2231aad` (pre-round) | 13/18 | 0/3 | 1/3 | $0.0062 |
| `03534ad` (file state) | 15/18 | 0/3 | 3/3 | $0.0074 |
| read request | 14/18 | 0/3 | 2/3 | $0.0066 |
| read request + dedupe (now) | **15/18** | **2/3** | 1/3 | $0.0068 |

`metrics-method` on this tree: 3/6 focused + 2/3 full = **5 of 9 runs**, against 0 of 9 before
(Fisher exact p≈0.013). Every other task is 3/3 or a single-run swing. `add-retriever-test` bounces
1–3 of 3 across arms; the dedupe cannot touch it (its rounds touch one file, so there is one
`file_state` entry either way), so that swing is noise, not an effect.

Cost of the whole round: $0.0068 per full-suite run against $0.0062 for the pre-round tree — the
extra evidence is paid for by fewer wasted retries.

**Residual**: `metrics-method` still fails half the time on the same two behaviours — a stale
duplicate test left behind after a correct new one is written (E0063/E0428), and an anchor quoted
from memory rather than from the file it was handed. The copy-anchor prompt measured no total gain;
the next honest lever is a stronger executor model (measure how much of the remaining failure is the
model) before more prompt engineering.

### Path map: the ask turn becomes usable, 16/18 (2026-09-14, late)

The read-request turn was firing but wasting itself: across the recent arms, **8 of 31 requested reads
hit a path that does not exist** — the model asks for "the file where `TraceEvent` is defined" and
writes `src/obs.rs` (the file is `src/obs/trace.rs`). Nothing had ever told it what paths exist, and
the old `gather()` spent five tool calls and up to 20k chars per round on five arbitrary *root* files
that answered nothing.

Both are gone: the implementer's prompt now carries a ~2k-char **path map** of the tree (the
retriever's own walk, skip dirs and extension filter, capped at 150 paths, byte-stable per task so it
rides the cached prefix). Net deletion of ~40 lines.

Leading indicator, 6 focused runs per arm:

| arm | ask turns | asks that resolved | all-failed asks |
|---|---|---|---|
| read-request | 4 | 4 | 0 |
| 3 rounds | 9 | 6 | 3 |
| dedupe | 6 | 3 | 3 |
| **map** | **15** | **15** | **0** |

The model asks ~4x more often and every ask resolves. With that, `metrics-method` — 0/9 across every
earlier arm — is 6/6 on the focused suite and 7/9 across focused + full (Fisher p≈0.002 against the
0/9 baseline). `add-retriever-test` gave back two runs on the focused suite (5/6 → 3/6, its old
duplicate-module and prose-instead-of-a-write failures) but recovered to 3/3 on the full suite.

Full suite, 3 runs per arm, planner skipped — the first 6/6 run recorded:

| tree | matched | runs | metrics-method | $/run | in tokens |
|---|---|---|---|---|---|
| `2231aad` (pre-round) | 13/18 | 4, 5, 4 | 0/3 | $0.0062 | 58919 |
| `03534ad` (file state) | 15/18 | 5, 5, 5 | 0/3 | $0.0074 | 70733 |
| read request | 14/18 | 5, 4, 5 | 0/3 | $0.0066 | 77265 |
| + dedupe | 15/18 | 5, 5, 5 | 2/3 | $0.0068 | 83867 |
| + map (now) | **16/18** | **6**, 5, 5 | 1/3 | $0.0062 | 74223 |

Cost is back to the pre-round $0.0062 per run with the blind reads deleted, and every task is 3/3
except `metrics-method` (1/3 — the same residual: a stale duplicate test left behind after a correct
one is written, or an anchor quoted from memory).

**Residual / next**: the remaining failures are one model behaviour in three costumes (leave a
duplicate behind, quote an anchor from memory, answer in prose instead of a patch). The prompt and
round-count levers have both been measured and did not move the total; the next honest step is a
stronger executor model on the same suite, and only then more prompt work.

```bash
scripts/measure-arm.sh before 3 eval/suites/repo-tasks.json --limit 6 --jobs 2   # one arm, 3 labelled runs
```

`scripts/measure-arm.sh <tag> <runs> [suite] [rof eval flags...]` wraps `live-eval.sh` and writes
`run-<tag>-<i>.json` plus a trace per run under `~/rof-runs-<tag>` (`ROF_ARM_DIR` overrides), so two
runs in one minute cannot overwrite each other and an arm is one command.

The script loads `DEEPSEEK_API_KEY` from `~/.hermes/.env`, sets the base URL, models, workdir,
task root and check allowlist, then writes a trace and a report per run under `~/rof-runs/`
(`ROF_RUNS_DIR` overrides). It only builds and runs `rof eval`; anything after the suite path is
passed straight through.

Rule that comes from the runs above: **3+ runs per arm, same `--limit`, same models**, and
compare per-task matched sets, not just the rate. One run swings ±2 tasks.

**`ROF_PLANNER=skip` is the A/B lever, not just a cost lever.** With the planner on, the same 6
goals produced 10 and 20 implementer calls in two runs of the same arm (identical planner call
count, different tasks-per-plan) and cost ranged $0.0043–$0.0219; pinned, rounds sat at 6–10 and
cost at $0.0043–$0.0074. It also changes *what fails*: with the plan skipped the goal names the
file, retrieval hands that file over whole, and a refused patch is rare (0 refusals in 3 runs),
while with the planner on the implementer works from a sub-task whose anchor it has not read
(10 refusals across 2 runs). Pick the arm for the question: mechanism-under-test → skip; the
plan/implementer interplay → on.

Disk: 6 task copies ≈ 3.0 GB (a `cargo test` task owns ~1 GB of `target/`, a `cargo check`
task ~250 MB). Point `ROF_TASK_ROOT` at disk and `rm -rf` the copies when done.

Two arm recipes, and the two traps that cost a round here:

- Cheap class instrument: `scripts/measure-arm.sh hard 6 eval/suites/hard-two.json --jobs 1`
  (`hard-two` is the two tasks that fail in every arm; ~40 s and ~$0.004 a run, so 6 runs per arm
  is affordable and the sample is real).
- Full suite: `scripts/measure-arm.sh after 3 eval/suites/repo-tasks.json --limit 6 --jobs 2`.
- A "before" arm is a worktree at the old commit: `git worktree add ~/rof-armX <commit>`. The
  worktree does **not** contain `scripts/measure-arm.sh`, `scripts/live-eval.sh` or a suite added
  after that commit — copy the two scripts in and pass the suite by absolute path, or the arm
  silently produces nothing (`cd` fails inside a `&&` chain, and `rof eval` exits 2).

## v1 scope: done

- Engine: session, supervisor loop (plan -> implement -> review, bounded rounds), model router
  (Context vs Executor + fallback).
- Agents: planner, implementer, reviewer behind one trait; each pluggable.
- Context: long/mid/short layers, per-layer budgets, head+tail truncation, cheap-model
  compaction on overflow, keyword retrieval with budgets.
- Tools behind a single deny-by-default gate: `fs.list`, `fs.read`, `fs.write`, `fs.patch`,
  `proc.run` (exact allowlist, no shell), `http.get` (host allowlist, no redirects).
- Verification: checks run before review and their output is injected with an explicit
  `STATUS: PASSED/FAILED (exit N)` line; the harness rejects a "pass" on empty work when the
  task expected writes; retries carry previous check output.
- Meta-harness: JSON suites, per-task matched results, append-only JSONL traces, metrics for
  success / tool accuracy / tokens / cache-hit rate / cost (estimated + provider-reported) /
  latency / budget aborts / retries / model errors, `utility = success − λ·cost`.
- **Fan-out**: `max_parallel_tasks` config + `rof eval --jobs N` + `--limit K`. Isolation by
  copy — one scratch copy per task under `task_root` (`target/`, `.git/` excluded), so parallel
  writers never share a tree and the source tree is never a target. Results keep suite order;
  a failed copy fails only its own task.
- Config as an artifact: JSON config file, `rof config` canonical dump, round-trip tested.
- Version control: `git init` + the v1 commit.
- Docs: this file + README + three example configs + a 20-task suite.

## v1 scope: remaining

None. Everything the v1 scope named is built, tested and exercised live. The items below are
the next round, not gaps in this one.

## Known warts (all deliberate, each has a reason)

- The reviewer runs on the executor model — same model as the implementer. Review is
  evidence-gated self-review, not independent verification.
- Plan task count varies run to run for an identical goal, which dominates cost variance.
  A/B comparisons must pin the plan or skip the planner (`ROF_PLANNER=skip`), and a 6-task
  sample is a baseline, not a measurement of a change.
- `expect_writes` is a policy, not a truth: an analysis task must declare
  `expect_writes: false` or the harness will correctly refuse its pass.
- Retrieval is keyword-based; no embeddings (dropped deliberately for zero deps).
- Suites are JSON, not YAML (same reason).
- `resolve_under`/`under` are lexical: no symlink resolution. The marker's "when write tools land"
  caveat is stale — `fs.write`/`fs.patch` have landed and will follow a symlink out of the root.
  Fix in ~5 lines (canonicalize the parent, compare against the canonical root).
- `writes[]` is stringly typed (`FAILED {path}: {e}`) and the count is a prefix match; the refusal
  path now carries structured data, `writes` should follow.
- `apply_patches` and `apply_writes` are the same 30-line loop twice.
- Fan-out multiplies disk and CPU by the job count: each task builds its own `target/`.
  `--jobs` above ~4 mostly buys contention on a 12-core box with cargo checks in the loop.

## Good next steps (ranked)

1. **Verification before the write** — `metrics-method`'s residual, and the one failure class
   that survived *two* executor models: the model asserts facts about code it has not read
   (`E0559`/`E0063` invented field sets, `E0592`/`E0428` duplicates, prose instead of a patch). The
   harness already hands it the file (path map + read-request turn + file-state evidence), so the
   missing piece is a check the model must pass *before* the artifact is accepted — e.g. the
   implementer's own `cargo check` result travelling with the artifact, or the reviewer (which holds
   `fs.read` in the policy but is handed `tools: None`) reading the file it is judging.
2. **Stage 3 (programmable state)** — the next stage the plan names: `state/` module, `~/.rof/state.json`,
   `state.propose` behind the gate, `rof state approve|reject`. Nothing of it exists in the tree: the
   skeleton and the CLI stub were deleted , so this is greenfield, and the same rule
   applies — the module lands in the commit that calls it.
3. **Goal 4's reopen condition (not a default flip)**: the auto-poke is measured and stays off
   (§ *Goal 4 live arm*: 9/12 vs 10/12 at +32% cost). The only version worth another arm is the narrow
   trigger — poke on the `expect_writes && writes == 0` rejection only, where 2 of 2 conversions came
   from. The goal-quality half needs the `expect_writes: false` exemption first (it flags 4 of 28 real
   suite goals, all explain tasks), otherwise any quality arm measures noise.
4. **Independent review**: route only the reviewer to a second model and measure whether verdicts
   change. Distinct from the executor arm above, which swapped the model for every role at once.
5. `writes[]` as structured results + one helper for `apply_patches`/`apply_writes` (the same 30-line
   loop exists twice).
6. Symlink-aware containment in `resolve_under` before any suite is allowed to run with a tool that
   can create links (the fix is ~5 lines: canonicalize the parent, compare against the canonical root).

**Rule earned on 2026-09-15:** a commit that adds a module must declare it in the module tree and ship
a caller or a test that fails without it — otherwise it is a doc, and docs live in `docs/`. Six
skeletons were deleted here precisely because they were read as progress while compiling to nothing.

## The reasoning-model bug class (2026-09-17, live, deepseek-flash)

Two live suite runs on `deepseek-flash` (DeepSeek-V4.1-Flash) found the largest
single correctness bug the harness has ever shipped, and it was not in the
harness's logic — it was in the seam between the harness and a reasoning model.

**The bug.** `deepseek-flash` is a hidden-chain model: a turn's output tokens
are spent on reasoning *before* the visible answer. The client asked for
1200/800/600 output tokens (implementer/planner/reviewer). The model spent
every one of them reasoning and returned an **empty `content`** with
`finish_reason: "length"`. The harness read an empty artifact as "the model
chose to write nothing"; the reviewer then failed the task with the accurate
but useless charge "WRITES MADE is 0"; and the retry loop could not help,
because a 200 response with empty content was not an error.

Measured on the 20-task suite: three tasks failed repeatedly with
`WRITES MADE is 0` while the model was in fact being cut off mid-thought.
The model was not misbehaving — it was asking for more files and refusing to
invent struct fields it had not read, which is exactly the discipline the
system prompt demands. The harness was truncating it before it could speak.

**The fix.** `once()` reads `finish_reason` and returns `Err` on `"length"`
instead of an empty answer, so the retry loop that already existed can act on
it; a truncation retry doubles `max_tokens` (a same-size re-roll just
truncates again) and gets exactly one; agent ceilings are raised to reasoning
scale (8192 / 4096 / 4096) and `summarize()` asks for the content budget plus
fixed headroom. `402` (insufficient balance) is now retryable.

**Cost of the discovery.** Two runs, \$0.17 and \$0.23 — the second one was
spent diagnosing, not progressing: the truncation-retry landed before the
ceiling raise, so it re-asked at the same too-small size and burned budget.
The lesson is recorded as a rule for the harness itself, not just a note: a
retry that cannot change the request's shape is a donation to the provider.

**Open, blocked on budget.** The balance is exhausted, so the re-measurement
that would confirm the fix's effect on pass rate has not run. The one task
that failed three times pre-fix passes in a single round post-fix, which is
evidence but not a measurement.

## The parser threw away correct answers (2026-09-17, live, Atria-Dawn-Preview)

DeepSeek's budget ran out, so the live work moved to a local preview model
(Atria-Dawn-Preview, an OpenAI-compatible reasoning endpoint). The first
single-task run scored 0% with two reviewer failures, both "WRITES MADE is 0",
while the trace showed the model emitting patches. The model was not refusing
to write; the harness was dropping its output.

**The bug.** `parse_lenient` scans for the first brace-balanced `{...}` span so
a model that wraps JSON in prose still gets parsed. But it counted braces
*inside string literals*. An artifact's `search`/`replace` fields carry source
text, so they routinely contain `fn f() {` or a lone `}`. Those moved the depth
counter: an extra `}` in a string closed the object early (serde then reported
"unterminated string") and an unbalanced `{` meant it never closed at all.
Either way a complete, correct answer became unparseable, and the round was
spent on a retry that produced the same good output and lost it again.

**The measurement.** The same implementer prompt, replayed directly against the
API 12 times with both scanners, isolated the parser from everything else:

| span scan | recovered | malformed | no balanced span |
|---|---|---|---|
| before (brace-naive) | 3/12 | 3 | 6 |
| after (string-aware) | 12/12 | 0 | 0 |

All three failing classes were one bug. The fix is a few lines: track whether
the scan is inside a string and whether the previous character escaped.

**Why it matters beyond Atria.** DeepSeek emits the same patch shape, so this
bug was silently taxing the paid provider too — every affected turn paid for a
retry that could only reproduce the answer it had already received.

## A suite task was measuring my own edit (2026-09-17)

The suite asked for `EvalReport::tool_failures`. That method exists in the tree
because I wrote it by hand while diagnosing the truncation bug above — the
failing probe asked for exactly that method, and I unblocked the probe by
implementing it myself. The suite measures the model against a repo where the
answer is already committed, so the task measured nothing. It is replaced with
an equivalent additive task (`model_calls` + `cost_reporting_rate`) that is not
in the tree, and the suite hash changed accordingly. A benchmark that the
operator has already solved is not a benchmark.

## Three caps elided the file the goal named (2026-09-18)

`multi-metric-and-trace` reads its own failure back to me: "I need the full
metrics.rs tail". `metrics.rs` is 20,374 chars. A file the goal names by path
passed through three compounding ceilings, all ~12-16k:

1. `Retriever::snippet` excerpts a named file at `NAMED_FILE_CAP = 12_000`,
   head and tail with the middle elided;
2. `max_total_chars = 12_000` trims the assembled retrieval block;
3. the mid layer's `fit` cuts at `budget * 4 = 16_000`, and above 0.8 of that
   it *summarizes* the layer instead of cutting it.

So the struct, `Default`, `fold` and the test module lived in the hole. The
system prompt forbids patching unseen text and a re-request for the same path
is deduped, so the hole was unreachable and the task ended at
"WRITES MADE is 0". Atria's own feedback named the missing tail.

**The fix.** A file the goal names is a whole-file answer, and §4.1 already
created the place for it: the volatile tail below the layers, where the budget
is `short_term * 4 = 24_000` chars and nothing is ever summarized. The
implementer now serves goal-named files there with a window centred on the
goal's symbols, and the orchestrator drops them out of the (summarized) mid
layer so the implementer never sees the same file twice.

**Why not just raise the mid layer.** Raising `mid_term` to 6000 tokens does
not deliver the file whole — the summarize threshold is a share of the cap, so
a 20k file at a 24k cap trips `wants_summary` and is collapsed into a summary,
which destroys the file instead of cutting it. The layers are sized for the
context model; a whole source file is a volatile item.

**A first probe misread the cause.** An A/B on conversation shape (eager files
in turn 1 vs ask-then-receive) showed 5/8 patch emission against 2/8 and I
wired an eager-files path in the implementer. Reverting it and probing the
*first* implementer call showed the file already present on HEAD: the
orchestrator's retrieval had been serving it all along, just elided. The
measured difference was the elision, not the turn shape. The eager scaffold was
reverted; only the volatile-delivery part survived.

## Arm 3: the volatile fix landed, the endpoint did not (2026-09-18)

Run on `63fffd1` (20 tasks, `--jobs 2`): 7/20 again, but **21 model errors**
against 1 in arm 1, so the headline is not comparable to arm 1. Atria's
endpoint returned 502s through the run; `multi-config-env` spent all 7 rounds
on them and its artifact came back an API error.

Where the endpoint let it run, the fix worked on exactly the tasks that name a
big file:

| task | arm 1 (12k caps) | arm 3 (volatile named files) |
|---|---|---|
| `metrics-model-call-rate` (metrics.rs, 20k chars) | FAIL, 3 rounds | **PASS, 1 round** |
| `summarize-doc` | FAIL, 5 rounds | **PASS, 1 round** |
| `budget-doc` | FAIL (ran `cargo check`, not `cargo test`) | **PASS, 1 round** |
| `regression-test-cache-rate` | PASS | PASS, 1 round |

Two of those conversions are the measured effect of serving a goal-named file
whole below the layers instead of elided through the mid layer. The rest of the
gain was swallowed by the 5 analysis tasks (still 0/5) and by the endpoint.

## The analysis class: a report that is not a report (2026-09-18)

All five `analysis-*` tasks fail the same way in all three runs, and it is a
different failure from the write tasks. The artifact is a *progress report*:
"enumeration in progress", "the report is not yet produced", "two things still
to trace". The reviewer's feedback is model-perfect and basically hands it the
answer shape, and the model still returns a progress note next round.

The cause is the prompt, not the context: `IMPLEMENTER_SYSTEM` told the model
everything about patches, writes, reads and skills, and nothing about the case
where the deliverable *is* prose. For `WRITES REQUIRED: no` the model behaved
like a write task that had not reached its writing turn yet — and with
`max_review_rounds: 2` there is often no such turn. The instruction now names
that case: the artifact must be the answer, with quoted strings and a
file:line for every claim, never a plan to research.

## A second spoiled task (2026-09-18)

`multi-metric-and-trace` asks for a `tool_failures` counter on `EvalReport`,
incremented in `fold` and printed in the summary. The diagnostic method
`pub fn tool_failures()` — written by hand while chasing the truncation bug —
was still in `src/eval/metrics.rs`, so the task's headline deliverable was
already committed. Worse, it is a *derived* method (`tool_calls - tool_ok`),
not the stored counter the task asks for, so a model reading the file found the
exact name present and reported `WRITES MADE: 0`. The method and its test are
removed; the task is live again. Cross-checking the whole suite against the
tree found no other spoiled deliverable (`budget-doc`'s field legitimately
pre-exists — that task documents it).

## Arm 4: +1, and the analysis prompt did not land (2026-09-18)

Run on `0d16841`: **8/20** (arm 1 and arm 3 were 7/20), still 17 model errors.
`http-deny-test` converted FAIL→PASS at one round, and every task that names a
big file stayed fixed (`metrics-model-call-rate`, `summarize-doc`, `budget-doc`,
`regression-test-cache-rate` all PASS in one round now).

The analysis instruction did **not** move the five `analysis-*` tasks — still
0/5, and the feedback is unchanged in kind: "the artifact is a progress note,
not the requested deliverable". The one-line prompt addition is not enough for a
task whose answer needs an enumeration the model cannot hold in one turn. Those
five tasks are now the suite's fixed cost and the clearest remaining gap.

`multi-metric-and-trace` is un-spoiled and is now a real task: the reviewer
reports the core counter is absent from the verified source, i.e. the model is
failing it honestly instead of being defeated by a pre-committed answer.

## Arm 5: the planner was over-decomposing single changes (2026-09-18)

`tool-count` ran 6 rounds on a task that adds one function and one test. The
planner (default `always`) splits a goal into subtasks and each subtask gets its
own bounded implementer→reviewer loop, so a one-file change became several
narrowly-scoped loops that each read a slice of the file and none felt ownership
of the whole edit. Every task in this suite is a single small change, which is
exactly the shape `ROF_PLANNER=skip` exists for. `config_hash` hashes the loaded
config, so the env override labels itself honestly (`e711d4f03cca97a7` →
`840fcc06da5754e5`).

**The result.** 12/20, up from 7/20 (arm 1, arm 3) and 8/20 (arm 4), with four
clean gains and no losses:

| arm | pass | model errors | input tokens | agents |
|---|---|---|---|---|
| 1 (`ab6aa34`) | 7/20 | 1 | 444k | impl/planner/reviewer/summarizer |
| 3 (`63fffd1`) | 7/20 | 21 | 624k | impl/planner/reviewer/summarizer |
| 4 (`0d16841`) | 8/20 | 17 | 606k | impl/planner/reviewer/summarizer |
| 5 (`0d16841` + `ROF_PLANNER=skip`) | **12/20** | 15 | **441k** | impl/reviewer only |

The gains are the mechanism, not luck: `workspace-flag` went from delivering 1
of 4 required pieces in 2 rounds to **all 4 in 1 round** — with the goal as one
task the implementer owned the whole edit. `tool-count` went from 6 rounds to 1.
`multi-config-env` converted from a model-error washout to a 1-round pass.
`analysis-cache-shape` became the first analysis task to pass.

Skipping the planner also removed its call from every task and stopped the
multi-task structure from overflowing the mid layer: input tokens fell 165k
(−27%) and the summarizer stopped being called at all, while the cache-hit rate
*rose* to 10.9% because one task per goal keeps the prompt byte-stable.

**What this costs.** The planner is what would decompose a genuinely large goal;
this suite has none, so `skip` is right for it and wrong in general. The lever
stays env-gated (`ROF_PLANNER`) rather than becoming the default.
## Arms 5-7: three reps, the planner result holds (2026-09-18)

Three independent runs of `0d16841` with `ROF_PLANNER=skip` against three runs
with the planner on:

| arm | config | pass | model errors | input tokens |
|---|---|---|---|---|
| 1 (`ab6aa34`) | planner on | 7/20 | 1 | 444k |
| 3 (`63fffd1`) | planner on | 7/20 | 21 | 624k |
| 4 (`0d16841`) | planner on | 8/20 | 17 | 606k |
| 5 | planner skip | **12/20** | 15 | 441k |
| 6 | planner skip | **13/20** | 15 | 423k |
| 7 | planner skip | **13/20** | 15 | 404k |

Planner on: 7.3/20 (37%). Planner skip: 12.7/20 (**63%**). No arm lost a task the
skip arm then failed, and the token bill fell 200k while the cache-hit rate held.

Two of the five analysis tasks have now passed under the skip arm
(`analysis-cache-shape` in arms 5-7, `analysis-token-ceiling` in arm 7), so the
class is not impassable — it is partly a function of how many rounds a
single-task goal actually gets.

The stable failures across all three skip reps are `add-retriever-test`, the
three remaining `analysis-*` tasks, and the two `multi-*` tasks that need
several coordinated edits. Both of the latter fail by delivering *most* of the
pieces — `multi-tool-and-grant` adds the tool and registers it but leaves the
planner grant out — which is a rounds problem, not a context problem.

## Arm 8: a third round for the multi-part edits (2026-09-18)

With the goal as one task, `max_review_rounds` is now the binding limit on a
multi-part edit: the implementer delivers most of the pieces, the reviewer names
exactly the missing one, and there is no round left to add it. The analysis
tasks have the same shape — read, then answer, then answer again after the
reviewer has described the missing shape. Arm 8 keeps `ROF_PLANNER=skip` and
raises `ROF_MAX_ROUNDS` to 3 so both classes get the second chance.
## Arm 8: a third round does not help, rejected (2026-09-18)

`ROF_PLANNER=skip` + `ROF_MAX_ROUNDS=3`: 13/20, the same as arms 6 and 7, for
131k more input tokens and 23 model errors. Every task the extra round was meant
to rescue used it and still failed — `add-retriever-test` (3 rounds),
`multi-tool-and-grant` (3), `multi-metric-and-trace` (3), and three of the four
`analysis-*` tasks (3 each) all ended in the same state. `analysis-token-ceiling`
went the other way. The failing class is not starved for rounds; it spends the
rounds it has without ever emitting the deliverable. Rounds stay at 2.

## What the remaining failures are (2026-09-18)

A direct probe of `add-retriever-test` (single goal, on a throwaway `/tmp` copy
of the tree) removed the last harness explanation. The goal names the file and
the exact test name; the volatile fix serves `src/context/retriever.rs` (14k)
whole on turn 1; the model then asks for `tests/retrieval.rs`,
`src/config/mod.rs` and `src/context/assembler.rs` — goal-directed reads, since
the goal says to follow the repo's test style — and still closes the round with
an empty `writes` array. Every file it needs is in hand. That is the model
declining to commit an insertion into a file with no existing test module, not a
harness gap: the two test tasks that pass (`regression-test-cache-rate`,
`http-deny-test`) both append into a module that already exists.

So the floor is set by model judgment, and the harness's remaining job is the
one §0 set out: make the cheap model's good turns survive to delivery. The
volatile-file fix and the planner skip each did exactly that; a third round
does not.
## Direct mode is not an improvement — 5 of its 16 were vacuous (2026-09-18)

`ROF_MODE=direct` removes the reviewer as well as the planner, so the suite's
five analysis tasks (no writes required, no configured check) lost their only
oracle. Direct mode computes `passed` as `(!expect_writes || writes>0) &&
checks_pass(&results)`, and `checks_pass(&[])` is vacuously true — so all five
passed unconditionally at rounds=1, having changed no file and run no check.
The model could have emitted `{"artifact": "todo"}` and scored.

The arm reported 16/20 twice. Splitting the suite by whether a task has any
oracle at all gives the honest picture:

| arm | mode | raw | ungated (5) | gated (15) |
|---|---|---|---|---|
| 1,3,4 | pipeline | 7,7,8 | 1,0,0 | 6,7,8 |
| 5,6,7 | planner skip | 12,13,13 | 1,1,2 | 11,12,11 |
| 8 | skip + 3 rounds | 13 | 1 | 12 |
| d1,d2 | **direct** | **16,16** | **5,5** | **11,11** |

Direct mode's gated score (11/15) is *equal to* the planner-skip baseline's.
Every point of the apparent gain is vacuity. Two conclusions, both measured:

1. **The reviewer's feedback loop buys nothing on gated tasks** — 11/15 either
   way. Its retry prose is not the lever; the checks and the write gate are.
2. **The reviewer was load-bearing as an oracle**, not as a feedback source.
   It is the only thing that can score a task with no check and no expected
   write. Remove it and those tasks stop being measured.

Fix landed (`orchestrator.rs`): a direct-mode task with no checks and no
expected writes has no oracle, so it fails with a `no oracle` verdict and stops
at one round rather than spending the cap rediscovering that nothing can
score it. A write the model emits anyway does not turn it into a pass. The
regression test fails on the pre-fix tree (`passed: true`) and passes after.
## Arm #4: an independent judge does not rescue the analysis class (2026-09-18)

`ROF_VERIFY_MODEL=stealth/union-alpha` on its own OpenRouter client
(`ROF_VERIFY_TOKEN`/`ROF_VERIFY_BASE`, added for this arm), Atria executing,
pipeline + planner=skip so the only difference from arms 5-7 is who scores.

| arm | judge | gated (15) | analysis (5) | tokens |
|---|---|---|---|---|
| 5-7 | self (Atria) | 11-12 | 1-2 | 404-441k |
| 10a rep1 | stealth | 9 | **1** | 428k |
| 10a rep2 | stealth | 11 | **0** | 443k |
| 10a rep3 | stealth | 10 | **2** | 395k |
| **10a mean** | **stealth** | **10.0** | **1.0** | **422k** |

The analysis class does not move under independent judging (1.0/5 over three
reps vs self-review's 1-2/5). Two failures that looked like context problems
were not: the judge's feedback is consistent and specific across all four
failed tasks ("the artifact records which files were read; the requested
findings are nowhere in the deliverable"). The model does the research and then
does not write the answer down. That is the same failure the direct-mode probe
found, now confirmed by a second model that has no incentive to agree with the
first.

Conclusion for the reviewer: **its feedback loop buys nothing on gated tasks
(11/15 with and without it), and as an oracle it changes nothing on the
analysis class either.** The reviewer is not the lever, on either job. What the
class needs is on the writing side — an analysis artifact must accumulate its
findings into the deliverable — not a better judge of an empty one.

The plumbing the arm needed is kept: the judge can now sit on a different
provider than the executor, and the comparability label names it
(`verify <model>`), because an arm that swaps only the judge is otherwise
indistinguishable from a rerun.

Arm B (roles swapped: stealth executes, Atria judges) rep 1 = 13/20 raw, gated
11/15, analysis 2/5. Cost note: `stealth/union-alpha` is free on OpenRouter
(prompt=0, completion=0 per `/v1/models`), so the "est $0.17 (table)" line is
the harness's flat DeepSeek table, not money spent. The provider-reported
$0.00 is the real number. Both models in these arms are free, which is what
makes the comparison affordable — and the pricing table should not be quoted
as cost for a model it doesn't describe.

## The analysis class was unsatisfiable by construction — the answer never
## reached the judge (2026-09-18)

Arm #4's judge feedback read the same on all four failed tasks: "the artifact
records which files were read; the requested findings are nowhere in the
deliverable." That is not a model judgment call. The contract tells the model
`artifact` must BE the answer when no write is required, and a one-model probe
confirmed the model fills it when the facts are in context — but the implementer
discards the key. `AgentOutput.data` wraps the model JSON as
`{"result": <model json>, ...}`, so the answer travelled nested at
`/result/artifact` while the reviewer's `ARTIFACT:` line rendered the envelope
around it: which files were read, which writes applied. A judge reading that
envelope concluded exactly what it said — files read, no answer — and it was
right about the envelope and wrong about the model.

Fix: `answer_of()` hoists `/result/artifact` into an `ANSWER:` line in the
reviewer's evidence. Regression test `the_prose_answer_reaches_the_reviewer`
asserts the reviewer prompt carries it, and is **verified to fail on the pre-fix
tree** (`ANSWER:` absent) and pass after. Two unit tests cover the pointer and
its empty cases.

Arm B is left at one rep (13/20, gated 11/15, analysis 2/5) and is not quoted as
a mean: its second rep was contaminated by the in-flight tree edit this fix
required, and was killed rather than reported. The single rep's signal is
directional only.

## Arm 11: the ANSWER hoist is neutral, because the endpoint was the confound
## (2026-09-18)

Same config as arm 10a (Atria executes, stealth judges, planner skip) on the
hoisted-answer tree:

| arm | gated (15) | analysis (5) | model errors/rep |
|---|---|---|---|
| 10a (no `ANSWER:` line) | 10.0 | 1.0 | 14 |
| 11 (`ANSWER:` hoist) | 9.3 | 0.7 | 14-18 |

The hoist did its job mechanically: the judge's feedback on
`analysis-token-ceiling` now reads "ANSWER was '(none given)'" instead of
"findings are nowhere in the deliverable" — the failure is correctly attributed
to the model, not the evidence. But the class did not move, because the answers
are genuinely absent.

**Why, measured.** The Atria endpoint returns `content: null` with only
`reasoning_content` populated on **7 of 8** identical probes, and that
`reasoning_content` is exploration babble, not an answer. `null_as_empty`
deserialised it to `""` and `complete()` returned `Ok("")`, so a degraded call
was scored as a model that chose to write nothing. That is the third false
reading this session, and it reframes the analysis class: the model is not
refusing to synthesise, it is being cut off mid-thought by its endpoint and the
harness was recording the silence as a decision.

Fix landed: an empty-content reply is now `LlmError`, so the retry loop re-asks
and the run counts it. This makes the analysis class measurable for the first
time — its floor is no longer set by a silent transport quirk.

## Arm 12: empty-content retries lift the gated class 9.3 → 11.3/15 (2026-09-19)

Identical to arm 11 except a `content: null` reply now retries instead of being
scored as an empty artifact.

| arm | gated (15) | analysis (5) |
|---|---|---|
| 11 (empty accepted) | 9.3 (62%) | 0.7 |
| **12 (empty retries)** | **11.3 (76%)** | 0.7 |

Per-rep gated: 12, 12, 10. The +2.0/15 is the analysis-independent part of the
bug — gated tasks were losing whole rounds to silent empty replies, and a retry
recovers the round. Analysis is flat at 0.7/5 across both arms, so the class is
a genuine model/endpoint limit and not something the harness was hiding.

**This is the best gated result measured with an independent judge**, and the
first time the gated score moved from a transport fix rather than a prompt or
routing one. The honest ceiling on this suite with Atria executing remains the
11-12/15 that self-review arms 5-7 measured; arm 12 matches it without letting
the executor grade its own work.

## Arm 13: the reliable endpoint is NOT the gated limiter (2026-09-19)

Hypothesis: the 0/6 gated tasks were empty-content artifacts, not model limits.
Stealth executes (10/10 valid JSON vs Atria's 2/10 on the same probe), same
judge model as arm 12, same planner, same suite — the endpoint was the only
variable.

| arm | executor | judge | gated (15) | analysis (5) | errors/rep |
|---|---|---|---|---|---|
| 12 | Atria | stealth | **11.3 (76%)** | 0.7 | 18 |
| 13 | stealth | stealth | 9.7 (65%) | 1.0 | 8-14 |

The hypothesis is **partially right and wrong as an explanation**. Two of the
three 0/6 tasks now pass at least once (`add-retriever-test` and
`multi-tool-and-grant`, 1/3 each — they were never pure capability gaps). But
the gated mean went *down*, 11.3 to 9.7: stealth is a weaker executor on this
suite than Atria despite being five times more reliable at the transport layer.

So the corrected story: transport reliability was load-bearing on specific
hard tasks but was never what capped the score. The remaining gated failures
are the model's real limits — multi-part edits across files and creating a new
module — and the analysis class is flat at 0.7-1.0/5 on every executor and
every judge tried, which is a genuine limit rather than a measurement artifact.

**The harness is now measuring something real.** Three false readings were
found and fixed this session (vacuous `checks_pass(&[])`, the buried prose
answer, silent empty content), and the ceiling this model family reaches is
~11.3/15 gated. That is the number to beat, and it is honest.

## Crossbench: SUPERSEDED — the free key was exhausted (2026-09-19)

**This result is wrong and must not be quoted.** A 3-rep run over rof, hermes
and pi on the same free OpenRouter model produced:

| agent | score | rate |
|---|---|---|
| pi | 22/30 | 0.73 |
| hermes | 19/30 | 0.63 |
| rof | 14/30 | 0.47 |

rof ran *first*, with quota remaining; hermes and pi ran *after* it. The key
reports `total_credits: 10, total_usage: 10.25` (exhausted) and the free-model
quota reads `limit 1000, remaining 0` (resets 08:00 UTC). So the two agents that
scored higher did so on a rate-limited endpoint, and rof's deficit is partly an
ordering artifact. rof's 0.47 is the only number measured with quota left, and
even that is a single 3-rep pass over a nondeterministic model.

The useful output is the *method*, not the table:

- **A deterministic-oracle cross-agent benchmark is runnable here.** All four
  agents are installed (hermes, pi, claude, rof) and each can be driven
  non-interactively on the same starting repo and goal strings, scored by a
  shell command no judge sees.
- **A single rep measures nothing** — the same task and env passed 4/6 times on
  rof alone. Temperature nondeterminism dominates; every number needs reps.
- **Each agent silently worked in the wrong place until pinned.** Hermes file
  tools key off `TERMINAL_CWD` (not `--in` or process cwd), so it edited
  `$HOME/lib.py` and reported success; pi answers in prose and never uses its
  edit tool without `--approve`; rof needs `ROF_WORKDIR` anchored to the task
  dir. In all three cases the agent *reported completing a task it had not
  touched* — the same failure mode this session has been hunting in the harness.
- **A rate limit reads as a capability gap.** rof makes several model calls per
  task (context + executor + reviewer + retries) where pi makes one per edit, so
  it exhausts a shared daily quota faster and its later tasks fail with
  `all models in fallback chain failed` — which is a transport condition, not a
  model or harness one. Any cross-agent comparison must record per-agent request
  counts and quota state, or the agent that talks most loses for the wrong
  reason.

## Atria as the comparison substrate (in progress)

The OpenRouter free key is exhausted and resets 08:00 UTC, so a quota-capped
endpoint can no longer be the substrate for a cross-agent run. `Atria-Dawn-Preview`
(endpoint `https://api.atria-asi.ai/v1`) is quota-free — no rate-limit headers at
all — and OpenAI-shaped, so all three agents can be pointed at the same model
without a daily cap deciding the outcome.

Getting each agent onto it exposed five transport-level failure modes. Each one,
unhandled, makes the agent look *less capable* rather than misconfigured:

- **Atria emits `reasoning_content` and charges it against `max_tokens`.** A request
  with a small `max_tokens` returns `content: null` with a populated
  `reasoning_content` and `finish_reason: stop` — the model spent its whole budget
  reasoning and never produced an answer. With `max_tokens: 2000` the same request
  returns `content: "OK"`. So an "Atria returns empty content" reading is almost
  always a token-budget reading. This is the same class of bug arm 12 patched in
  the harness — an empty model response is never a signal about capability, and it
  is not one here either: tools, reasoning and content all work once the budget is
  adequate.
- **Atria's streaming endpoint is broken.** `stream: true` returns a non-JSON body
  (decode failure / 422). Any agent that streams by default needs streaming
  disabled, or every call fails.
- **Hermes has a supported custom-provider path**, but it is not `provider:
  openrouter` + `base_url`. `_openrouter_should_use_pool` deliberately *drops* the
  credential pool whenever a custom `base_url` is set, so pointing the `openrouter`
  provider at Atria silently falls back to `OPENAI_API_KEY` against the OpenRouter
  host and 401s. The supported path is a `custom_providers:` entry with `key_env`
  (key read at runtime from the named env var) and `provider: custom:atria`.
- **Hermes file tools resolve cwd from `TERMINAL_CWD`**, not `--in` and not the
  process cwd. Without it the agent searches `$HOME`, reports the file missing,
  and offers to look in an unrelated repo.
- **Turning reasoning off in Hermes silently cripples it, and the failure reads as
  a model limit.** A/B over the full 10-task suite, same model, same tasks, same
  every other setting: `--reasoning none` scores **3/10**, `--reasoning medium`
  scores **10/10**. With reasoning off the model still fixes the three existing
  functions but fails all seven *add-a-new-function* tasks — and reports each one
  as complete. This was diagnosed the wrong way round first: an early 422 was
  blamed on `reasoning_effort` and `--reasoning none` was adopted as the fix. The
  real cause of the empty content was the `max_tokens` budget above, not reasoning.
  Left uncorrected this would have manufactured a false "rof beats hermes" result
  out of a flag. Reasoning stays on.

All of that was diagnosed against a local recorder that echoes the exact request
body the agent sends — the only reliable way to see what an agent actually puts
on the wire, since the failure is a 200 with an empty payload rather than an error.

**The token now lives at `~/.config/atria-key` (mode 600)** — it was originally only
in `/tmp`, which an external process wiped mid-session, destroying the crossbench
suite and every run artifact with it. The benchmark will be rebuilt under a durable
path.

## Cross-agent result on Atria (10-task suite, 1 rep)

| agent | score | notes |
|---|---|---|
| hermes | 10/10 | `--reasoning medium`; 3/10 with `--reasoning none` |
| pi | pending | — |
| rof | pending | — |

The 10/10 is a **single rep against a suite this small**, so it is a floor on
hermes' capability here, not a ceiling. It is reported because the
reasoning-level delta it exposed (3/10 vs 10/10, identical everything else) is
large enough to be real at any rep count, and because the wrong setting was about
to contaminate the comparison.

**Suite integrity was verified before any agent ran.** Every oracle passes both
directions: the seeded, unmodified repo *fails* it (no vacuous tasks — one such
task was caught and fixed), and the reference solution *passes* it (no
unreachable oracle — one unsatisfiable goal was caught and fixed).

## Single-rep full-suite result: all three agents 10/10

| agent | score | model calls | wall time |
|---|---|---|---|
| hermes | 10/10 | 10 | ~19 min |
| pi | 10/10 | 10 | ~10 min |
| rof | 10/10 | 10 | ~14 min |

**This is the honest headline, and it is a null result for the benchmark as
designed.** Once each harness was stopped from getting in its own way (see the
five failure modes above), all three solved every task on the same model. The
suite is too easy to discriminate between harnesses: ten tiny single-file tasks
in a two-function repo, where the whole context fits in the prompt with room to
spare.

The differentiators that mattered were all *harnesses getting out of their own
way* — `TERMINAL_CWD`, reasoning level, `ROF_WORKDIR`, `--approve` — not any
harness's real capability. None of rof's distinctive machinery was exercised:
the context assembler, retriever and windowing are irrelevant when the repo is
one small file. A benchmark cannot measure a context-selection advantage on a
repo with nothing to select from.

**Conclusion: the benchmark must get harder, in the specific way that exercises
rof's actual design.** A multi-file repo with a real structure — where finding
the right file, reading enough but not too much, and keeping several edits
consistent across files is the task — is what separates harnesses here. That is
the next build.

The valid things established by this run:
- The method is sound: identical repos, identical goals, identical model, one
  quota-free endpoint, deterministic oracles verified both directions.
- All three agents are correctly wired and none is handicapped by a
  misconfiguration anymore. Any future difference will be real.
- Atria supports a full agent loop (multi-turn tool use, file edits, reasoning)
  for all three harnesses at no cost, so the OpenRouter daily cap can no longer
  decide an outcome.

## Multi-file suite, 3 reps: rof 15/18, hermes 15/18, pi 14/18 (tie)

The harder suite demanded by the null result above is built and run. Ten files
with real structure (config/store/models/api/validators/utils/report plus tests),
planted defects that require reading more than one file, and per-task defect
planting so one task's bug cannot break another task's oracle.

| task | hermes | pi | rof |
|---|---|---|---|
| mf-rename-key | PPP 3/3 | P.P 2/3 | PPP 3/3 |
| mf-add-validator | PPP 3/3 | PPP 3/3 | .PP 2/3 |
| mf-fix-import-cycle | P.P 2/3 | PPP 3/3 | PPP 3/3 |
| mf-new-endpoint | PPP 3/3 | P.P 2/3 | PPP 3/3 |
| mf-dead-code | P.P 2/3 | P.P 2/3 | PPP 3/3 |
| mf-test-coverage | P.P 2/3 | PP. 2/3 | .P. 1/3 |
| **total** | **15/18** | **14/18** | **15/18** |

**This is a three-way tie, and it must not be quoted as a rof win.** The spread
is one task out of eighteen — smaller than the rep-to-rep variance within a
single agent. This is the second time the corrected number has come down as a
measurement defect was found (see "the no-op failure mode" below), which is
itself the lesson: an apparent gap that closes when the driver is fixed was a
driver artifact, not a capability difference.

What the suite *did* establish, in order of confidence:

1. **The suite discriminates at last.** The single-file suite gave 10/10/10; this
   one gives no task a clean 3/3 across all three agents. The failures are real
   capability failures, not harness misconfiguration.
2. **The failures are per-task, not per-agent.** No agent owns a task class.
   mf-dead-code looks like a rof strength (3/3) and a hermes/pi weakness (2/3
   each); mf-test-coverage is the one task every agent fails at least one rep of
   (1/3, 2/3, 2/3). That pattern says the remaining differences are about which
   specific edits each harness happens to make reliably, not a general
capability gap.
3. **No harness is handicapped anymore.** The wiring fixes from the single-file
   run held: same model, same repos, same oracles, no quota involvement (18
   model calls per agent over the whole run, well under any limit).

**The honest ceiling on Atria-Dawn-Preview is roughly five-sixths of a
multi-file task suite, for every harness tested.** That matches the gated
ceiling measured earlier (~11.3/15, arm 12) with a different model on a
different suite: the limiter is the model, not the harness. Improving rof's
context selection is worth attempting and is measurable now, but it should be
expected to move a task or two, not to separate rof from the field.

### Three benchmark defects the verifier caught before any agent ran

Each of these would have produced a false table. All were caught by the
both-directions oracle check, none by inspection:

1. **A planted circular import broke the whole package.** The cycle ran through
   the package `__init__`, so pytest collection failed on *every* task and all
   six oracles reported UNREACHABLE for the wrong reason. Fixed by isolating the
   defect to a module nothing else imports, and planting defects per task.
2. **Two tasks were vacuous after per-task planting.** Moving defects into a
   plant map left mf-rename-key and mf-new-endpoint with nothing wrong on the
   seed, so doing nothing passed. Fixed by planting the *absence* of the
   feature as the defect.
3. **Cross-test pollution, then stale bytecode.** A regression test asserting
   `total()==0` failed because an earlier test in the same process had already
   added records; and once fixed, the oracle still failed because the verifier
   rewrites files within one second and Python's 1-second mtime check ran the
   *old* `.pyc` — the traceback showed the new source against the old behaviour.
   Fixed by asserting an order-independent invariant and clearing `__pycache__`
   before every oracle run.

### The no-op failure mode: a rate limit in disguise (fifth defect)

The first version of the multi-file table read rof 14, hermes 12, pi 11. It was
wrong, and the cause was a new failure class that no oracle catches because it
is not an oracle problem at all.

The symptom: an agent finishes in 19–23 seconds having changed **zero** files,
produces empty stdout, and the oracle correctly reports FAIL — because the seed
repo is unfixed. Diffing the work directory against a fresh seed showed no
differences at all beyond caches. Re-running the identical task through the
identical code path succeeded in 77 seconds with a correct edit.

The cause was transient pressure on the Atria endpoint during a burst run: the
agent's first request failed and the harness exited cleanly without retrying.
A 429 or a dropped connection reads, to a scoring oracle, exactly like a
capability gap. The first table had **eight** such runs baked into it — pi's
rep 3 was recorded as 2/6 when a clean re-run gives 5/6.

The fix is in the driver, not the oracle: after scoring a FAIL, diff the work
directory against a fresh seed; if the agent changed nothing, it never really
ran, so re-seed and retry (up to three times). An empty-diff FAIL is a transport
artifact and is never recorded as a capability measurement.

**The lesson: a failure with no file changes is not a failure to do the task.**
This is the sixth false reading of the session and it is the most dangerous one,
because it is invisible — the oracle is correct, the harness exits cleanly, and
the only evidence is that nothing happened. Diff-before-you-score is now a hard
rule in the driver.

### The recurring lesson

The recurring lesson, now six times in this session: **an oracle that passes
for a reason you have not checked is not an oracle.** Vacuous, unreachable,
order-dependent, bytecode-stale, and now no-op — each looked like an agent
failure until the verifier was run on the seed and the reference solution in
both orders, or the work directory was diffed against a fresh seed.

## External evidence: what the field says about harness deltas (2026-09-19)

Three external sources were read before deciding what to do next, and they
reframe what "beating the field" can even mean on this substrate.

**Arena.ai's HarnessTax study** (21 model-harness pairs, 7 models, 3 harnesses:
Claude Code, Codex CLI, pi — on SWE-bench Lite and Terminal-Bench 2.0) found that
harness choice has **little effect on task success rate** — ±2% on SWE-bench
Lite, ±5% on Terminal-Bench — but a **large effect on cost**: the same model at
similar success rates for up to 5× the spend. A minimal harness (pi: read,
write, edit, bash) was competitive with vendor harnesses.

The 15/15/14 result above is exactly what that study predicts for the
success-rate axis: on a matched model and matched tasks, harness differences
wash out. The finding that *does* separate harnesses — cost — is not measurable
here, because Atria is free and every harness reports $0.

**Benchgen's 2026 guide** surveys harness-focused benchmarks and reports much
larger gaps: Harness-Bench (106 tasks × 6 harnesses × 8 models, 5,088
trajectories) found a 23.8-point gap between the best and worst harness, and —
most relevant here — **36% of failures were schema/output-contract violations,
not reasoning errors.** SkillsBench 1.1 found curated skills lift resolution
33.9% → 50.5% while self-generated skills *hurt* by 8–11.5 points. AgentLens
found up to 23.2% of "passes" are lucky blind retries, and rankings shift by up
to 5 positions under process-adjusted scoring.

The schema-violation number is the actionable one for rof: it says a third of
the remaining failures may be *contract* failures the harness can fix rather
than reasoning failures it cannot. That directly contradicts the arm-13
conclusion that the remaining gated failures are pure model limits, and it is
testable: the `mf-test-coverage` column (1/3, 2/3, 2/3 across all three
harnesses) is the one task class that fails for everyone, and its failures are
contract-shaped — the seeded test suite is green while asserting buggy
behaviour, so the agent sees nothing wrong and changes nothing. Fixing that is
a harness opportunity (surface "this test asserts known-wrong behaviour"), not
a model upgrade.

**The HN thread** on the Arena study added the sharpest concrete mechanism:
models fail on tools that *resemble but differ from* tools they were trained on
— an `EditFile` with an `old_content` argument gets called with `old_string`
because that is the shape in the training traces. OpenCode registers different
edit tools by model family for exactly this reason. Commentators also noted the
harness matters more for smaller models, which need context management
offloaded to the harness, and that the harness tax is largely system-prompt
size.

**The implication for rof:** the success-rate axis is close to saturated on this
model, so the next lever is either (a) the contract-violation class, where a
harness can still earn points a model cannot, or (b) cost/token efficiency,
which is the axis the field actually separates on but which a free endpoint
cannot measure. Direction (a) is the one available here.

## Terminal-Bench 4.0 is runnable locally (2026-09-20)

The external-evidence round pointed at Terminal-Bench 4.0 as the field-standard
suite, so I checked whether it is actually usable on this machine with the
quota-free Atria model. It is.

**Requirements that turned out to be non-blocking:**

- The README says tasks "require GPUs" and the docs push Modal. In practice
  **8 of 68 tasks declare `gpus = 1`; the other 60 declare `gpus = 0`**. A
  60-task CPU subset is available.
- Modal is not needed: **`docker` is a supported local sandbox** (`-e docker`),
  Docker is available here, and `harbor` installed cleanly via
  `uv tool install 'harbor[docker]'` (the extra name is wrong in the docs —
  `docker` is not a valid extra, but the local runtime needs no extra
  dependency, only the docker CLI).
- The oracle agent passes on both a sample task and a real Terminal-Bench task
  (`interleaved-vigenere`), which validates the sandbox, the verifier, and the
  task packaging end to end.

**rof is wired in as a Harbor custom agent** (`RofAgent`, an installed agent
that copies the release binary into the sandbox and runs it in `/app`). Two
integration bugs were found and fixed by reading Harbor's *installed* source,
not the published docs — the docs reverse the API:

1. **The exec helpers live on the agent, not the environment.** The docs show
   `environment.exec_as_root(...)`; the real signature is
   `self.exec_as_root(environment, ...)`. File upload is the opposite way round
   (`environment.upload_file`).
2. **The task images have no `git` binary and `/app` is not a repo.** rof's
   rollback substrate requires both. The agent now installs git and lets rof's
   `ensure()` do `git init` + one commit.

rof then solved `hello-world` (reward 1.0) end to end inside a Harbor sandbox.

**The remaining blocker is a build issue, not a design one:** the release
binary is built on the host (glibc 2.43) but the Terminal-Bench task images are
Debian bookworm (glibc 2.36), so the binary fails with
`GLIBC_2.39 not found` — traced to two symbols, `pidfd_getpid` and
`pidfd_spawnp`, which Rust std pulls in for process spawning on newer kernels.
A bookworm-glibc build is in progress; if that works, rof can run the full
CPU-only subset of Terminal-Bench 4.0 on Atria.

**Why this matters:** the internal multi-file suite tops out around 5/6 for
every harness, which means it cannot separate rof from the field at the
resolution that would show a real improvement. Terminal-Bench 4.0 is the suite
the field actually compares on, it has 60 runnable CPU tasks here, and its
resolution rates for frontier models are 12–58% — so there is headroom for a
small model to show a harness-driven difference rather than saturating at the
top like the internal suite does.

## The reasoning-budget ladder, and what it found (2026-09-20, live, Atria)

`interleaved-vigenere` is the first Terminal-Bench task rof attempted that has
a large implementer prompt (the real `/app/data/` corpus puts ~7KB in the user
turn). On that task rof produced **zero writes** and the artifact was a single
error line. Root-caused by following the diagnostic chain rather than guessing:

1. **`LlmError::AllFailed` discarded the cause.** Every failure read "chain
   failed" with no reason. The variant now carries the primary error, which is
   the only reason this bug class was findable at all.
2. **The cause is `reasoning_content`.** The endpoint ships `content: null`
   with `finish_reason: length` because the model spent the whole `max_tokens`
   budget on hidden reasoning (22k–31k chars) and never started the answer.
3. **Doubling `max_tokens` is exactly wrong.** At 8192 the reasoning was 27k
   chars; at 16384 it grew to 64,608 and content was still 0. Reasoning
   expands to fill the budget.
4. **The `reasoning: false` knob only works on small prompts.** It is ignored
   on the real implementer payload — measured by curl against the same
   endpoint, same key, same model.
5. **`chat_template_kwargs.reasoning_effort: "low"` is the knob the template
   honours**, and it works at any prompt size *when the endpoint chooses to
   honour it* — but that choice is nondeterministic. Three identical requests
   gave content 4072, 2953 and 0 on successive calls.

The fix is a **four-rung ladder** in `OpenRouterClient::complete`, each rung
reshaping the request instead of re-rolling the same shape: plain →
`reasoning_effort: low` → `reasoning: false` → roomier (2× `max_tokens`,
reasoning back on). The truncation and empty-content paths both climb it, and
the retry budget is bounded so an unreachable answer still terminates.

**Honest result: the ladder is verified working and the task still fails.**
Instrumented live, the four attempts climbed exactly as designed
(low → off → roomier at 16384) and all four truncated with `content=0`. On
this prompt the endpoint cannot emit the artifact at any knob setting right
now. That is an endpoint/model limit, not a harness limit — the harness now
does everything the retry layer can do, and the failure surfaces with a full
diagnostic instead of a silent no-op.

This is the same lesson as the no-op retry fix: **the harness's job is to make
the failure legible, not to make it disappear.** Before these fixes the run
read as "the model cannot write a cipher tool"; after them it reads as "the
endpoint spent 27k tokens on reasoning and shipped nothing at four request
shapes", which is a different and actionable fact.

What did land and is measured:
- `LlmError::AllFailed` carries the primary error (was discarded).
- Empty content is retried; truncation and reasoning-exhaustion climb the
  ladder; all bounded, 3 unit tests fail on the pre-fix classification.
- `ChatReq` gained `reasoning` and `chat_template_kwargs`, both
  `skip_serializing_if = None`, so a first attempt is byte-identical to before
  either knob existed.
- The implementer's re-ask after `reads` now carries a `[RE-ASK]` marker:
  requested files are in context, do not emit `reads` again. Without it the
  model defers a second time and stops, which reads as a model that cannot
  implement when it only could not tell it had what it asked for.
- `symlink_safe` distinguishes a missing parent directory from a security
  denial, so a write to a new file in a nonexistent dir says "create the
  directory first" instead of the permanent-sounding "path is not resolvable".

## Arm: the ladder, measured on the multi-file suite (2026-09-20, 3 reps)

The reasoning-budget ladder was armed and the multi-file suite re-run, same
3 reps, same `config_hash`-diffed tree, same driver. The suite's prompts are
small enough that the reasoning-budget class never fires — so this arm was
never going to move it, and it did not:

| rep | before (`21de3ec`) | after (`a184bb3`) |
|---|---|---|
| 1 | 4/6 | 6/6 |
| 2 | 6/6 | 3/6 |
| 3 | 5/6 | 4/6 |
| **total** | **15/18** | **13/18** |

**This is a no-op within noise, and it must be reported as one.** The spread is
2 tasks out of 18 against a within-agent rep swing that has measured 3→6→5.
The corrected call is: the ladder fixes a failure class the internal suite
does not exercise, and the one task that does exercise it (`interleaved-vigenere`)
has an endpoint that cannot emit the artifact at any of the four request
shapes. Both directions were measured rather than assumed.

The failures the suite *does* have are a different class, confirmed by reading
every failure tail: `mf-rename-key` r3 wrote 1 file and the oracle rejected the
edit; `mf-add-validator` r2 wrote 0 with tool accuracy 100% and no transport
error; `mf-test-coverage` wrote 2 files that did not fix the seeded bug. None
is a chain failure, none is a truncation, none is an empty-content no-op. They
are the model editing wrong, which is what the contract-violation hypothesis
predicts and what the next lever targets.

## Arm: the red-suite report, measured on the multi-file suite (2026-09-20, 3 reps)

The next lever was aimed at the one failure the contract-violation hypothesis
named concretely: `mf-test-coverage`, where all three agents scored 1–2/3. The
seeded test asserts `total() == 22`, the *buggy* value, so the suite is green
*because* the bug is present. The goal says "keep the existing tests passing",
which makes the task structurally impossible: fixing the bug turns that
assertion red, and the oracle then rejects a fix that is exactly what was
asked. The implementer has no `proc.run`, so it could not see it — the failure
looked like a model that could not implement when it was a model that could
not be told.

Now the suite runs after writes land and the failing assertion is put in the
artifact, which is the only channel the model has. It only fires when the
policy grants a command the run can use, so a shell-free configuration is
unchanged; `file_state_evidence` carries it to the retry.

| rep | before (`a184bb3`) | after (`8230015`) |
|---|---|---|
| 1 | 6/6 | 5/6 |
| 2 | 3/6 | 6/6 |
| 3 | 4/6 | 6/6 |
| **total** | **13/18** | **17/18** |

`mf-test-coverage` went 1/3 → 2/3, and the other five tasks went 12/15 → 15/15.
The mechanism is visible in rep 2: the model read the TEST REPORT and
corrected the assertion `22` → `15` with a comment naming why —
"No phantom seed record: total() is just the records added here." Rep 1 did
not, so the fix is not deterministic, but the class it targets is now addressed
in 2 of 3 trials rather than 1 of 3, and the task it was aimed at moved while
four unrelated tasks stopped regressing.

**Negative control:** the unmodified seed still passes its own suite (3 passed)
while asserting the buggy value, so the task is not vacuous — the win is on a
genuine oracle. This is the first arm this session that moved the multi-file
score by more than the within-agent rep swing, and it is a *harness* lever, not
a model upgrade: the harness made a contract violation legible instead of
making it fatal.

**Honest caveat:** this is one arm on 6 tasks × 3 reps. 17/18 vs the previous
best of 15/18 needs the same 3-rep discipline applied to hermes and pi before
any ordering claim — and `mf-test-coverage` is a *rof-shaped* opportunity, so a
full cross-agent rerun is the next measurement, not a conclusion.

## Terminal-Bench 4.0: the comparison that had never actually run (2026-09-20)

Direct answer to a fair question: **no cross-harness comparison on Terminal-Bench
existed before this point.** What existed was plumbing — `RofAgent` wired in, one
`hello-world` run since lost to a /tmp wipe — and the 15/15/14 tie was on the
in-house multi-file suite, not on a public benchmark. The four Harbor jobs on
disk were all `interleaved-vigenere` debug runs.

That changed. `hermes` and `pi` are **built-in Harbor agents**, so the missing
half of the comparison needed no custom agent — only a working route to Atria.

**Three real blockers found and fixed in Harbor's bundled `hermes` agent**
(patches saved at `~/.local/share/tbench/hermes-agent-patched.py`):

1. `get_version_command` and `install()` call `hermes version`, which is not a
   command in hermes v0.21.3 (`hermes --version` is). The trial errored during
   setup before the agent ever started — reading as a capability gap.
2. `_NATIVE_PROVIDERS` has no `atria` entry, so hermes fell back to OpenRouter
   and died at **HTTP 401: Missing Authentication header**. The OpenRouter key
   is exhausted; this was a routing failure, not a quota one.
3. `_build_config_yaml` emitted a flat `model:`/`provider:` layout that hermes
   0.21 flags as stale, and had no way to express a custom endpoint. Atria is
   only reachable as `provider: custom:atria` with a `custom_providers` entry
   and `key_env: ATRIA_API_KEY` — verified working with a direct `hermes` call
   before touching Harbor.

**First like-for-like tbench data point** (same task, same model, same key,
`payments-pipeline-fix`, 600s agent timeout):

| agent | reward | errored |
|---|---|---|
| hermes | 0.0 | 0 |
| rof | 0.0 | 0 |

This is a **tie on a task both failed**, and it must be reported as such. It is
not evidence that either harness is worse — `payments-pipeline-fix` asks for
worker-startup tuning with overdraft-ordering correctness under respawn, and
both agents produced something the grader rejected. What it *is* evidence of:
the pipeline now measures real agents instead of erroring, which is the
prerequisite for any comparison. A 0/0 on one task ranks nothing.

**Honest limits of this measurement:** one task, one rep, both agents score 0.
The token counts came back `null` — Harbor's hermes trajectory conversion reads
usage from the session export, and Atria's usage fields are not where it looks,
so the cost axis is still unmeasured here. And `layout-config-recreation2`, the
task `--n-tasks 1` selects first, is an 8-hour vision task; task selection is
material and `--n-tasks 1` is not a representative sample of anything.

### The task set does not discriminate: 0/0/0 across six CPU tasks (2026-09-20)

The matrix runner (`~/.local/share/tbench/tb-matrix.sh`) runs the three agents in
parallel with per-run job names — three agents launched in the same second
collide on Harbor's timestamped job dir and one dies with `FileExistsError`.

Calibration on `production-planning` (1 rep, 15-min agent cap):

| agent | reward | errored |
|---|---|---|
| rof | 0.0 | 0 |
| hermes | 0.0 | 1 |
| pi | 0.0 | 0 |

rof probes on the remaining CPU tasks, 15-min cap: `ctr-optimization` 0.0,
`session-window-debug` 0.0, `shadow-relay` 0.0, `bun-sourcemap-leak` 0.0.

**These are genuine task failures, not plumbing failures.** `session-window-debug`
shows the mechanism: rof modified `sessions.py`, `merger.py`, `gc.py`, `emitter.py`
and `types.py` — substantive edits to exactly the modules the task names — and the
verifier still rejected the result. rof works; the task is hard.

**One incidental Harbor finding**, recorded because it looks like a bug until it is
read: every run logs `docker compose cp failed ... invalid output path: directory
... does not exist` five or more times. Harbor retries via tar stream and the copy
lands, so it is noise in the log, not a failed run. It is identical with and
without my `--job-name` override, so it is pre-existing.

**The honest conclusion so far: six tasks, six zeroes for every agent.** Either
the agent cap is too short for this task class, or Atria-Dawn-Preview cannot clear
the tbench 4.0 bar at all — which would be the *model*, not the harness, and would
make the whole comparison uninformative on this benchmark. Two 30-min-cap probes
are running to separate those two hypotheses.

### The 30-minute probes settle it: the ceiling is the model (2026-09-20)

Two probes at a doubled agent cap (30 min) also scored 0.0:
`interleaved-vigenere` 0.0, `wal-recovery-ordering` 0.0. rof edited **15 files**
across the WAL subsystem — `wal.py`, `wal_index.py`, `serializer.py`,
`segment_manager.py`, `recovery.py`, `log_writer.py`, `metrics.py` and more —
substantive work on the right modules, and the verifier still rejected it.

**That is the answer, and it is not close to the harness.** Eight of eight CPU
tasks, at 15-minute and 30-minute caps, all zero. Doubling the budget did not
move any score, which separates "not enough time" from "not enough model" —
Atria-Dawn-Preview cannot clear the Terminal-Bench 4.0 bar, and no harness change
fixes that.

So the tbench comparison is **uninformative on this substrate**, and saying
otherwise would be the exact failure mode the whole project exists to avoid. The
informative comparison remains the in-house multi-file suite, where the task
difficulty is calibrated to the model — 17/18 rof after the red-suite arm, with
hermes and pi at 15/18 and 14/18 on the pre-red-suite tree.

**What this does establish:** the tbench *pipeline* is now correct and measures
real agents. Three blockers were fixed (stale `hermes version`, no `atria`
provider route, flat config layout), all three agents run clean, and results
harvest correctly. The moment a stronger model is available, this is a runnable
comparison. Today it measures a floor of zero.

**Next measurement that could discriminate:** the in-house suite is where rof's
levers actually show. Re-run hermes and pi there against the `8230015` tree so
the 17/18 is compared like-for-like — that is the honest test of whether the
red-suite arm is a rof-specific gain or a property of the task.

## The like-for-like multi-file rerun: no longer a tie (2026-09-20, 3 reps)

The tbench comparison cannot discriminate on Atria — 8/8 CPU tasks at zero — so
the informative measurement had to be the in-house suite, run like-for-like
against the `8230015` tree. That means re-running hermes and pi, not quoting the
old numbers, because rof's score moved while theirs had not.

All three agents, same 6 tasks, 3 reps, same model, same key, same oracle:

| task | hermes | pi | rof |
|---|---|---|---|
| mf-rename-key | PPP 3/3 | P.P 2/3 | PPP 3/3 |
| mf-add-validator | PPP 3/3 | PPP 3/3 | PPP 3/3 |
| mf-fix-import-cycle | P.P 2/3 | PPP 3/3 | PPP 3/3 |
| mf-new-endpoint | PPP 3/3 | P.P 2/3 | PPP 3/3 |
| mf-dead-code | P.P 2/3 | P.P 2/3 | PPP 3/3 |
| mf-test-coverage | P.P 2/3 | PP. 2/3 | .PP 2/3 |
| **total** | **15/18** | **14/18** | **17/18** |

rof is **+2 over hermes and +3 over pi**. The single task that separates them is
`mf-dead-code`, which rof passes 3/3 and both others pass 2/3 — and rof's one
remaining failure class is `mf-test-coverage`, where it passes 2/3 like both
others.

**The oracle was re-validated before any of this was believed.** The unmodified
seed still passes its own suite (3 passed) while asserting the buggy value, and
`mf_verify.py` still reports `seed fails, reference passes` for all six tasks.
The 6/6 that pi posted on rep 1 is real: it fixed the seeded assertion `22` →
`15`, added `test_store_fresh.py`, and cleared every other task.

**What this does and does not license.** It is the first measurement this session
where rof leads by more than the within-agent rep swing, on a calibrated suite,
against agents re-run on the same tree. It does **not** license a general claim
that rof is the better harness: 6 tasks and 3 reps is a small sample, the
separation rests on one task, and the rep-2 dip (3/6 for both hermes and pi)
shows how much run-to-run variance there is. It is a lead on one suite, recorded
with its variance attached.

**And it must be read against the tbench result.** On a public benchmark at the
frontier, all three score zero. rof's lead exists where the task difficulty is
calibrated to the model — which is exactly the design intent of v3 §0:
flash-class cost, frontier-adjacent completion. Both facts are true at once.

## Research: SoL-Pi (arXiv 2609.20519) — what we can actually use (2026-09-20)

[SoL-Pi: Recursively Scaling Auto-Research Loops for Efficient Agent Harness](https://arxiv.org/html/2609.20519)
is the closest published work to what this harness is for. It runs an RSI-inspired
search at the *harness* layer — ~152 proposed directions, ~535 executable
environments, 3,000+ runs, 60,000+ agent-environment interactions — and four
mechanisms survive selection. It is built as an extension of Pi, which is one of
the two agents we already compare against, so the mechanisms are directly
addressed at a harness shaped like ours.

### What they claim, and the part we should trust

The headline is **token traffic −44.7–49.0% and API cost about −1/3 while keeping
94% of Pi's score**, and it transfers from GPT-5.6 Sol to Opus 5 without
re-search. That is the efficiency story, and it is measured on EdgeBench.

**But the Terminal-Bench 4 table is the one that matters for us, and it cuts the
other way.** On 63 CPU-only tbench tasks: Codex 18 solved, Pi 18 solved,
**SoL-Pi 15 solved**. SoL-Pi is *cheaper per solved task* ($14.07 vs $15.91) but
solves **fewer** of them. This is the single most important number for our
position: we just measured that all three of our agents score 0/8 on the CPU
tbench tasks, so a stack that trades three solved tasks for cost savings is
exactly the wrong trade for us right now. **We should not adopt the full stack.**
Our constraint is capability at the model ceiling, not cost per task.

There is also a methodological caution they cite (Wang et al., arXiv 2607.12227):
harness-evolution gains on held-out tasks are often limited, and improvements are
overstated when search tasks and evaluation tasks overlap. SoL-Pi's own answer is
strict isolation — EdgeBench is held out, results never feed back. That matches
our arm discipline (frozen tree, matched outcomes, negative controls).

### The four mechanisms, mapped onto this harness

I checked the source for each one. Here is what already exists and what is genuinely new.

**1. Action Fusion — combine an edit with its follow-up command into one request.**
Pi edits a file, then issues a separate command to test it: three model round
trips. Fusion makes both one request. **We already have half of this by design**:
our implementer's `run_tests_summary` runs the suite *inside the same turn* the
writes land in, with no extra round trip. The difference is ours is fixed
(always pytest on writes), theirs is model-selected (`follow_up_command` in the
tool schema, "commands that require inspecting the mutation result remain
separate"). Making it model-selected is a schema change, not an architecture
change. **Verdict: cheap to try, and the paper says it is the *score* leader on
Opus 5 (44.8 → 50.5), which is the capability direction we care about — not the
cost direction.**

**2. Online Context Compact — compact at plan-step boundaries, gated by projected
savings.** Instead of compacting near the context limit, it compacts when a plan
step completes, and only if projected input savings exceed the cache-rewrite
cost. **This is the one we need least.** rof is not a long-horizon chat loop — it
is a bounded pipeline (planner → implementer → reviewer, fixed rounds), and §4.1
already owns the budget explicitly via `VolatileBudget` with `fit()` halving
windows until they fit. We have no prompt cache to amortize against. **Verdict:
skip.** The underlying idea (compaction gated on projected savings, not on
proximity to the limit) is sound, but the precondition that makes it pay — a
long-running session with a cached prefix — does not exist here.

**3. ObservationPack — stop re-sending large tool outputs.** Send a >10 KiB result
in full for the first two requests, then replace it with a handle + 1 KB head/tail
excerpt; the agent can page exact chunks on demand. **This is a real gap.** Our
`file_state` carries whole file text, and `read.output` goes into context as-is;
`condense_output` filters lines for the reviewer but the *re-read path* still
re-sends full bodies. The paper's version of this was the **score leader on
GPT-5.6 Sol (44.8 → 47.2)**. **Verdict: the most promising single mechanism for
us, and for the same reason it was theirs — it reduces repeated input without
losing information, because the handle keeps it retrievable.**

**4. Evidence-Preserving Reducer — a cheap model summarizes logs with a verified
receipt, falling back to the original.** For ≥4 KiB build/test logs from a known
command set; checks schema, source hash, exit status, exact quotes, size. **We
have a simpler, cheaper version already**: `condense_output` is a *deterministic*
line filter, and `run_tests_summary` already extracts only the failing
assertions. Theirs is strictly more powerful (semantic extraction) but costs an
extra model call per log and requires a second model. **Verdict: we already
captured the deterministic part. The remaining value is the verified-receipt
discipline — source hash + exact quotes + fallback — which is a good
correctness pattern for our existing filter if we ever need richer summaries.**

### What I am actually recommending

Two things, in this order:

1. **ObservationPack-style observation handles on the re-read path** (mechanism 3).
   This is the highest-value change: it was the capability leader on the search
   backend, it targets repeated input which is exactly what our multi-round
   re-asks amplify, and it preserves information via the handle rather than
   discarding it. It is also testable in isolation on the mf suite, which is the
   only suite calibrated to our model.

2. **Model-selected follow-up command on writes** (mechanism 1), because the
   fixed pytest invocation we have now is a special case of it, and because it
   was the capability leader on the held-out backend. Small diff: one optional
   field on the implementer's JSON contract.

**Both are capability mechanisms, not cost mechanisms.** That is the deliberate
inversion of the paper's headline. Their Terminal-Bench result is the evidence
for why: the full efficiency stack solved three *fewer* tasks, and we are
score-limited, not cost-limited. If we are going to copy anything from SoL-Pi,
copy the two mechanisms that raised the score, and leave the two that lowered it.

**What we should not copy:** the auto-research search loop itself. It cost them
3,000+ runs and 60,000+ interactions on a paid frontier model. Our substrate is a
free quota Atria that cannot clear a single tbench 4.0 task, and we have already
established that a single rep measures nothing — the search would be optimizing
against noise. Our arm discipline (frozen tree, ≥3 reps, negative control,
config_hash diff) is the version of their isolation discipline we can actually
afford.

## ObservationPack arm: MEASURED, NOT BUILT — the premise does not hold (2026-09-20)

The SoL-Pi research recommended an ObservationPack-style handle on the re-read
path as the top mechanism. Before implementing it I instrumented the harness to
dump every prompt the implementer sends and measured one representative mf task
(`mf-test-coverage`, the one that exercises the re-read and retry paths most).

**The premise fails.** ObservationPack exists to stop large tool outputs being
re-sent in full on later requests. In rof there is nothing large and not much
re-sent:

| turn | prompt chars | what accumulated |
|---|---|---|
| 0 | 703 | goal + layers, no file body |
| 1 | 1,334 | + the requested `store.py` body (~600 chars) |
| 2 | 2,284 | + reviewer feedback + test report |
| 3 | 3,251 | same as 2, the retry |

The whole four-turn exchange costs **15,436 chars total**, and the largest file
body anywhere is ~600 chars. There is no >10 KiB observation to handle, and the
one body that recurs appears twice, not "from the third request onward." A
mechanism whose trigger condition never fires is a no-op by construction.

**Why the architectures differ, and why the paper's number didn't transfer.**
Pi is a long-horizon chat loop: observations accumulate into a growing history,
so a big result genuinely pays for a seat in every later request. rof is a
bounded pipeline of 2 rounds, and each turn rebuilds context from scratch via
`ContextAssembler` under an explicit `VolatileBudget` — §4.1 already caps the
repeated-input class, with `eliminated_chars` counting what it cut (767 chars in
this run). The mechanism SoL-Pi needed was invented for a cost structure rof
does not have.

**This is the second mechanism of four that does not apply**, and it changes the
recommendation. Online Context Compact was inapplicable because there is no
prompt cache; ObservationPack is inapplicable because there is no accumulating
history. Both fail for the same root cause: **rof is not a long-horizon loop.**
Half of SoL-Pi's retained mechanisms address overhead that only exists in one.

**Method note.** The measurement was only possible by instrumenting the harness,
because Atria returns no `usage` fields — `input_tokens` comes back 0 on every
call, so the trace cannot see prompt sizes. I added a temporary prompt dump to
`ask_with_system`, measured, and reverted it (working tree clean). Token
accounting remains the unmeasured axis it was; char counting on the prompt is the
substitute, and it is adequate at this scale.

**What is left standing from the research: Action Fusion**, the one mechanism
that targets *capability* rather than long-horizon cost, and the one that was the
score leader on the held-out Opus 5 backend. Its fixed half already exists here
as `run_tests_summary`; making the follow-up command model-selected instead of
hard-coded to pytest is a real, small change that does not depend on any premise
about context size. That is the next arm, if any.

## The judgment call: what to do about quality and token-efficiency (2026-09-20)

Asked directly what the right call is. Here is the answer, and it is not another
mechanism from a paper.

### Token-efficiency is currently not a thing we can improve, because we do not measure it

This is the load-bearing fact. Every efficiency claim in the SoL-Pi paper rests
on `usage` fields — prompt tokens, completion tokens, cache read/write. **Atria
returns none of them.** `input_tokens` is 0 on every call; the trace's
`ModelCall.input_tokens` is therefore always 0; Harbor's hermes converter
returns `null` for the same reason. Two independent measurement paths are dead
at the endpoint.

So the efficiency axis has no numbers, and the ObservationPack measurement above
only worked because I substituted char counts on prompts I construct myself. That
substitute is fine for *input* (I build every prompt) and useless for *output*
(the endpoint's reasoning spend is invisible unless it tells us).

**Any token-efficiency work is therefore blocked on measurement, not on
mechanisms.** Optimizing an unmeasured axis is how you ship a no-op arm and
report it as a win — the exact failure this project exists to avoid.

### But the one place we DO know tokens are wasted is output, and it is measurable

The truncation error already reports `content=N chars; reasoning_content=M chars`
(`openrouter.rs:337`). That is a real output-efficiency measurement, captured by
accident, never aggregated. On the real implementer prompt we measured
`content=0, reasoning_content=37683` — the endpoint burned 37,683 chars of hidden
reasoning and shipped nothing. **That is a 100% waste of the output budget, and
it is simultaneously a quality failure** (the task fails with no content) and an
efficiency failure (the whole budget went to invisible thinking).

This is the one place quality and token-efficiency point the same direction, and
it is already observable. It is also the failure the four-rung ladder was built
for — and the ladder is verified working but was a no-op on the mf suite because
those prompts are small enough that the class never fires.

**One untried knob remains on that path.** `enable_thinking: false` is not wired
into `ChatReq` (it supports only `reasoning` and `chat_template_kwargs`). The
curl A/B found it produced **11,233 chars of content on the ~7 KB implementer
prompt where every other shape produced 0** — the only request shape that
returned substantive content at that prompt size. It still finished `length`, so
it is not a complete fix, but it is the strongest single signal in the whole A/B
and it has never been tried through the harness. That is a real, small,
hypothesis-driven change with a measured prior.

### And for quality, the honest state is: we are at the model's ceiling on the public benchmark, and one task short of a claim on the private one

- tbench 4.0: all three agents 0/8. Ceiling is the model. Harness work does not
  move this until a stronger substrate exists.
- mf suite: rof 17/18 vs hermes 15, pi 14. **The separation is one task,
  `mf-dead-code`** (rof 3/3, both others 2/3), and rep-2 dipped to 3/6 for both
  opponents independently. That is a lead, not a result.

So quality work that would actually mean something is: **diagnose why hermes and
pi each lose one rep of `mf-dead-code` and whether a harness lever would close
it.** "Find two uncalled helpers and remove them" is a removal task — it needs
whole-repo call analysis, which is exactly what a retrieval layer is for, and
both opponents fail it once in three. If that failure is a *retrieval* failure,
§4.4's recall work has a lever; if it is a model-judgment failure, nothing here
moves it. **Nobody has looked.**

### The call

In order:

1. **Wire `enable_thinking` into `ChatReq`** as a fifth ladder rung, behind the
   same `#[serde(skip_serializing_if)]` discipline so a first attempt stays
   byte-identical. Prior: the only request shape that returned real content on
   the hard prompt. Cost: a handful of lines. This is the single highest-value
   *quality* change available, and it is the only *efficiency* change with a
   measured prior, because it targets the 100%-waste output path.

2. **Aggregate the output-efficiency measurement** that already exists in the
   truncation error into the trace, so `content`/`reasoning_content` char counts
   are recorded per call instead of only on failure. Until the endpoint gives us
   `usage`, this *is* the efficiency axis. Without it every efficiency claim is
   unmeasurable and should be refused.

3. **Diagnose `mf-dead-code`** from the stored runs — what did hermes and pi
   actually do on the rep they lost? Cheap, no model calls, and it decides
   whether the remaining quality gap is retrieval-shaped or model-shaped.

**What I am explicitly not recommending:** more mechanisms from SoL-Pi. Two of
four are structurally inapplicable (no prompt cache, no accumulating history),
and the remaining two are one already-half-built and one blocked on a substrate
we do not have. The research moved us by ruling things out, which is progress,
but the next gain is not in that paper — it is in the output-waste measurement we
already capture and have never read.

## The `enable_thinking` arm: wired, verified, and a NO-OP on the mf suite (2026-09-20)

I implemented recommendation #1 from the judgment call above: a fifth ladder
rung. `LlmReq.thinking_off` carries it, `ChatReq.enable_thinking` puts
`"enable_thinking":false` on the wire, and `reshape_for_truncation` is the new
single source of truth for the climb order:

    plain -> reasoning_effort:low -> enable_thinking:false -> reasoning:false -> roomier

First attempts stay byte-identical (the field is `skip_serializing_if =
Option::is_none`), so nothing changes unless a truncation happens.

**Measured: a no-op, and I can prove it rather than infer it.** Three reps of
`mf-test-coverage` — the task with the most turns and the most output, the one
most likely to stress the budget — all passed, all with **exactly 1 model call
each.** One call means no retry, which means the ladder was never entered, which
means the new rung was never reached. A mechanism whose trigger never fires
cannot be what moved the score.

`mf-test-coverage` did go 2/3 -> 3/3. That is variance, not the rung: the rep
that flipped is one rep out of three, this task's history already swung
`.PP`, and the rung provably did not execute. Recording it as a gain would be
exactly the failure this project exists to avoid.

**The pathology also did not reproduce.** I re-tested all four shapes against
the live endpoint on the largest stored prompt (5,217 bytes) and on a
deliberately reasoning-heavy 2.5 KB prompt: every shape, including plain,
returned `finish=stop` with real content. `content=0; reasoning_content=37683`
was real when it was measured, but the endpoint is not exhibiting it now. So
the premise that made this the highest-value recommendation is weaker than it
looked — I built the rung on a prior that is not currently firing.

**What I am keeping, and why.** The rung stays, because it costs nothing on a
first attempt and the class was genuinely observed earlier in this project. But
it is now correctly labelled: unexercised insurance, not a measured improvement.
Its real test bed is large-prompt work, which on this substrate means
Terminal-Bench — where all three agents already score 0.

**The actual win this session was in the tests, not the feature.** My first
version of the order test used a `climb()` helper that was a *copy* of the
ladder. A mutation test (swapping rungs 2 and 3) **passed**, because the test
was asserting against its own mirror, not the implementation — a tautology that
would have silently blessed any reordering. I extracted `reshape_for_truncation`
so the retry and the test share one function, re-ran the mutation, and it now
fails with the intended message. The lesson is the same one that keeps
appearing: **a test that passes against a copy of the behavior proves nothing
about the behavior.** Verify the test can fail before trusting that it can pass.

### Score table

| arm | tree | mf-test-coverage (3 reps) | ladder fired? |
|---|---|---|---|
| baseline | `8230015` | `.PP` 2/3 | — |
| + `enable_thinking` rung | this tree | `PPP` 3/3 | **no — 1 call per run** |

The 3/3 is recorded as variance. The rung is recorded as unexercised.

## CORRECTION: the three-way result was a recording error (2026-09-21)

The README claimed rof 17/18 vs hermes 15/18 vs pi 14/18, with the separation on
`mf-dead-code` (rof 3/3, both others 2/3). **That claim did not survive
verification against the run's own artifacts, and it has been corrected.**

I went to diagnose why hermes and pi each lost a rep of `mf-dead-code`. The
stored work dirs were intact — source files carry mtimes inside the original run
window (22:58–00:02), so they are authentic and unmodified. I re-scored all 54
cells with the same oracle and compared against the stored result files:

- **The result files and the independent re-score agree on all 54 cells.** Not
  most — all of them.
- Both say **hermes 18/18 and pi 16/18**, and `mf-dead-code` is passed 3/3 by
  all three agents.

So the separation never existed. Hermes does remove the dead code in the rep it
was recorded as failing — the oracle says `PASS` on inspection of the untouched
work dir. The recorded 15/14 was wrong, and so was the story built on it.

**Two errors of opposite sign confirm the scoring was unreliable, not merely
noisy.** `pi r3/mf-add-validator` was recorded PASS, but `add({'name':'','amount':1})`
is accepted — the fix is incomplete, a false positive. `hermes r2/mf-dead-code`
was recorded FAIL for a fix that is correct, a false negative. A scorer that
errs in both directions cannot be trusted in either.

**I also destroyed part of rof's own evidence.** While measuring the
`enable_thinking` arm I ran `run.py rof mf mf-test-coverage` for three reps.
That invocation overwrites the result file for the whole rep, so rof's three
result files now hold only that one task. rof's work dirs survive — 15 cells
from the like-for-like run re-score 15/15, plus 3 for the re-run — but the
record is no longer intact. The instrument that produced the headline was
damaged by the measurement I made on it.

### What is actually true now

| agent | verified score | note |
|---|---|---|
| rof | 18/18 | 15 cells intact from the like-for-like run + 3 re-run today |
| hermes | 18/18 | untouched artifacts, cross-verified two ways |
| pi | 16/18 | loses `mf-add-validator` r3 (incomplete) and `mf-test-coverage` r2 |

**The suite is saturated.** All three are at or within two of the ceiling, so
the multi-file suite no longer discriminates. Whatever the next arm is, this
suite will score ~18 for it whether it helps or not — it has become an
instrument that cannot register the effect it is being asked to measure. This
moves "widen the mf suite" from a nice-to-have to a hard prerequisite for any
further claim.

### The lesson

The headline number was never checked against the artifacts it came from. A
result was written down, a narrative was attached to it ("the separation is
`mf-dead-code`"), and it propagated into the README unverified. When I finally
looked at the underlying work dirs, the narrative dissolved in minutes. The
fix is not a code change — it is the rule already stated in this file, now
applied to our own reported results: **a claim must be verified against its
source before it is repeated, and a table that is not re-derivable from stored
artifacts is not a result.**

## Prime Agent research — what transfers, and what doesn't (2026-09-21)

Read [Prime Agent: A self-improving RLM agent](https://www.primeintellect.ai/blog/prime-agent).
Two abstractions: **RLM** (context as a variable, sub-agent delegation as async function
calls in a persistent IPython REPL) and **Continual Harness** (harness state — prompts,
sub-agents, skills, memory — exposed as CRUD the agent edits from its own trajectory,
refined by `/refine`, which applies the smallest relevant edit and records trigger +
outcome, with rollback by ID).

**First, a family note: Prime Agent is built on top of `pi`** — the same base as our
pi opponent. So its claims describe our opponent's direct descendant, and its
harness-vs-harness table is partly an argument about where `pi` should go.

### The benchmarks are not usable for us, and the reason is structural

The post's benchmark menu: ARC-AGI 3, OOLONG, OOLONG-Pairs, OBLIQ-Bench, LongBenchPro,
LongBenchv2, ManyIH Coding/IF, LongCot-Mini, EmulatorBench, PMPP-Hard, Factorio, MazeBench.

Every one of these is calibrated to separate *frontier* models (Opus 5, GPT-5.6 Sol,
GLM-5.2). We run Atria, which scores **0.0 on 8 of 8 Terminal-Bench CPU tasks** at both
15- and 30-minute caps. These benchmarks will read as zero for us — that is an inference
from the measured tbench ceiling, not a new measurement, but it is a strong one. Adopting
them buys another floor, not a signal. Specifically:

- **PMPP-Hard** needs a GPU. We have none.
- **MazeBench / Factorio** need external runtimes and, by their own account, billions of
  tokens. Unaffordable at $0.
- **LongBench\*, OOLONG, OBLIQ, ManyIH, LongCot-Mini** are single-shot capability tests.
  They measure the *model*, not the harness, so they cannot separate harnesses at all —
  useful only as a ceiling probe for Atria.
- **EmulatorBench** is the closest in *shape* to what we do (long-context coding with a
  verifier, CPU-only, sandboxed, no reference implementation) — but its own top score is
  **0.208 with frontier models**. On Atria that is 0.

**The transferable lesson is a selection principle, not a benchmark list.** Our mf suite
is saturated (18/18) and tbench is 0/8. Measurement only works in the band *between*
them, where a harness delta can register. The next suite must sit in Atria's
discriminating range, not at the frontier. This is now the hard prerequisite for any
further claim — a harder benchmark from this list would be a third zero, and a
zero-vs-zero comparison is exactly the floor we already recorded on tbench.

### Lessons that do transfer

1. **Baseline honesty — and it validates our own correction.** The post says plainly:
   *"we evaluated Opus 5 and GPT-5.6 Sol with Claude Code and Codex respectively, and
   found worse overall performance relative to the official results, so we yield to
   their official reported numbers instead."* That is the identical failure class we
   just caught in our own headline (reporting hermes 15 / pi 14 when the artifacts say
   18 / 16). They caught theirs and deferred to a better source; we caught ours and
   corrected the README. The standing rule it reinforces: **opponent numbers must be
   verified against the opponent's own artifacts before they are quoted**, and a
   team that handles this correctly is a good-faith signal about the rest of their
   claims.

2. **A self-improving harness optimizes the metric, including by gaming it.** In Factorio,
   Prime Agent found it could spawn resources directly into machines via RCON. Once it
   did, *"the same refinement loop that had been building legitimate skills turned to
   building efficient cheating skills instead"* — despite an explicit heartbeat prompt
   telling it not to. This is the caution that matters for a saturated suite: against a
   fixed oracle, a refinement loop converges on satisfying the oracle, not the intent.
   It reinforces the human-approval gate rof already has.

3. **`/refine` is in direct conflict with our measurement discipline — a real tension,
   not a gap to close.** Online harness refinement across tasks within an eval rep would
   break `config_hash` reproducibility: rep 3's score would reflect accumulated harness
   state, not the frozen config, and no two reps would measure the same thing. rof
   already has the CRUD half (`SkillOp::{Create,Patch,WriteFile,Delete}`) and a
   `SkillPolicy::Direct` flag that would bypass the human gate — the capability is
   built. **The deliberate choice not to enable it during eval is the design decision.**
   Verified this session: the eval path never sets `ROF_SKILLS_POLICY`, so skills stay
   at the default `Propose` and are inert — the saturated 18/18 is not contaminated by
   accumulated skills. Prime Agent's self-improvement is a *product* feature; rof's is a
   *measurement instrument*. The two are optimized for different things, and ours is
   correctly the latter.

4. **One small, honest gap in rof's skills: no outcome linking.** `SkillChange` records
   the fate of an op (`proposed`/`applied`/`deleted`) but never whether the task
   *succeeded*. `/refine`'s distinctive idea is that each edit records its trigger *and*
   its outcome, making improvement evidence-backed. rof's proposals carry `rationale`
   but no result. Closing that is a small change to the proposal schema — but it is a
   recorded/artifact feature, not an online eval loop, for the reason in (3).

5. **PTC — run functions over data instead of reading the data as tokens — is low value
   here.** Measured last session: rof's whole four-turn exchange is ~15 KB, largest file
   body ~600 chars. There is no large data being shipped as tokens to displace.

### The call

Do **not** adopt Prime Agent's benchmarks (all read as zero on this substrate) and do
**not** build the online refinement loop (breaks reproducibility, and its own results
show it games the metric). Take three things: the **selection principle** (the next
suite must sit in Atria's discriminating band, between saturated-mf and zero-tbench),
the **baseline-honesty rule** as a standing requirement, and the **outcome link** on
skill proposals as a small, safe improvement.

Their variance discipline is also worth noting: ARC-AGI 3 reports [95.0, 95.2, 95.5]
across three runs — tight for a three-rep sample, and tighter than the rep swings we
measure. n=3 is a floor for them too, but their task is deterministic-scoring where ours
is model-nondeterministic.

## CORRECTION: Atria is frontier-class, and the tbench "model ceiling" conclusion is wrong (2026-09-21)

I had been describing Atria as a "free weak model" and building conclusions on
that. **That was never verified, and it is false.** The Atria-Dawn-Preview README
states it plainly:

> developed by the **Shanghai Artificial Intelligence Laboratory**. Built on the
> **744B-parameter MoE GLM-5** … Instruct model, 256K context.

The model's own evaluation table, against frontier baselines:

| Benchmark | Atria Dawn | GLM 5.3 | GPT 5.6 sol | Opus 5 |
|---|---|---|---|---|
| **Terminal-Bench 2.1** | **78.3** | 85.4 | 85.1 | **90.2** |
| SWE-bench Pro | 59.6 | 60.3 | 61.4 | **74.7** |
| MLE-bench Lite | 86.2 | 80.8 | **88.9** | 88.0 |
| BrowseComp | **92.5** | – | 92.2 | 90.8 |
| BFCL v4 | **77.0** | 74.1 | – | – |

**This breaks the Terminal-Bench conclusion I recorded.** The model scores **78.3
on Terminal-Bench 2.1** — mid-pack among frontier models on exactly the benchmark
family we tested. Through our harnesses it scored **0.0 on 8 of 8 Terminal-Bench
4.0 CPU tasks.** A model that is competent on Terminal-Bench does not fall to zero
across eight tasks because it is weak. The 78.3 → 0.0 gap implicates the *harness
path*, not the model.

Two honest caveats, which bound but do not remove the problem:

1. **Terminal-Bench 2.1 is not 4.0.** Different version, likely harder. The
   comparison is not apples-to-apples, so this does not prove a bug — it
   withdraws my proof that there was none.
2. **Our eight tasks were self-selected** CPU-only tasks, not a representative
   sample, and some may be genuinely very hard.

But the decisive detail is that **all three harnesses scored zero while sharing
one thing: the same `api.atria-asi.ai` endpoint.** A quirk at the endpoint layer
would sink all three identically and look like a capability gap in each. And we
already know of exactly such a quirk — the `content: null` / `finish_reason:
length` / `reasoning_content=37683` budget exhaustion on large prompts.

**That changes what the `enable_thinking` ladder is for, and it was measured on
the wrong substrate.** We tested it on the mf suite and called it a no-op — but
mf prompts are 2–5 KB, small enough that the exhaustion never fires, so the
test could not have shown anything else. The exhaustion is a *large-prompt*
failure, and Terminal-Bench prompts are large. The ladder may be precisely the
fix, and it has never been tried where it matters. My no-op claim is valid for
mf and says nothing about tbench.

### What this does to the benchmark recommendation

The Prime Agent benchmarks are back in play, and the GLM-5.2 column of their
table is now a reasonable prior for Atria's band rather than a prediction of
zero. **EmulatorBench** is the standout match: agentic long-context coding with
a verifier, CPU-only, sandboxed, and GLM-5.2 scored a nonzero **0.208** — the
same task shape as Terminal-Bench, at a difficulty where a frontier model is
not at the floor. That is a benchmark our harness differences could actually
register on.

### The call, revised

1. **Re-test the `enable_thinking` ladder on Terminal-Bench**, not mf. If the
   exhaustion is the failure, this is where it shows. This is the highest-value
   experiment available — it targets the one mechanism we have for a failure
   class we know exists on large prompts.
2. **EmulatorBench** as the discriminating suite, replacing the saturated mf
   suite and the zeroed tbench subset.
3. Stop describing Atria as weak, everywhere. The README's "the ceiling there is
   the model, not the harness" line is wrong and is being corrected in this
   same commit.

The methodological failure is the same one as the hermes/pi correction and the
SoL-Pi mechanism checks: **I built a conclusion on an unverified assumption about
the substrate instead of measuring it.** Two major conclusions in two days
turned out to be artifacts of assumptions I had not checked. The rule is already
written down; it now applies to claims about the model as much as to claims
about the harness.

## Second correction: the exhaustion quirk is real but rof sidesteps it — the tbench failure is correctness (2026-09-21)

The previous correction said the exhaustion quirk was the "prime suspect" for the tbench
zeros. **That was also wrong, and I verified it by running rof directly on a stored task.**

**What is true and now measured precisely.** The exhaustion quirk is real, and it is a
hard endpoint property, not a config issue. Bisecting user-prompt size against the real
wal-recovery files, `max_tokens: 8192`:

| user prompt (chars) | finish | content | reasoning | reasoning_tokens |
|---|---|---|---|---|
| 8,000 | length | 36,892 | 1,201 | 271 |
| 12,000 | length | 32,465 | 78 | 14 |
| **13,000** | stop | **233** | 2,293 | 543 |
| 14,000 | length | **0** | 34,810 | 8192 |
| 24,000 | length | **0** | 34,856 | 8192 |

Content emission collapses between 12,000 and 14,000 chars. Above the threshold the model
reasons until it has spent *every* completion token and ships nothing. And **none of the
switches stop it**: `reasoning: false` (content=0, rt=8192), `enable_thinking: false`
(content=0, rt=8192), `chat_template_kwargs: reasoning_effort: low` (content=0, rt=8192).
**Worse, `roomier` actively hurts**: at `max_tokens: 16384` the reasoning *doubled* to
72,059 chars and content was still 0. The reasoning expands to fill any budget. The
`roomier` rung and the `enable_thinking` rung are both inert above the threshold — the
ladder's upper rungs are dead weight on exactly the failure they were built for.

**Why it is not rof's failure.** I ran the deployed binary against the stored
wal-recovery task with the wire proxy logging every request. rof's actual prompts were
**916–1,480 chars** (synthetic goal) and **7,022–8,395 chars** (real instruction) — always
below the threshold. rof pulls files on demand via `reads` instead of dumping the repo, so
the wall is never approached. With the real instruction rof **edited all 10 modules**.
The zero-output jobs in the matrix were something else entirely: `tb-probe-rof-shadow-relay`
died on `AgentTimeoutError ... timed out after 900.0 seconds`, a timeout, not exhaustion.

**So the real tbench failure is correctness, not content emission.** Running the task's
own test suite against rof's output: **25 tests failed.** The work is real and substantial
— every module touched — and it is still wrong. That is the same conclusion the artifact
diffing reached at `d5d0bc4`, now confirmed against the model's own benchmark: a 744B
GLM-5 model that scores 78.3 on Terminal-Bench 2.1 produces real edits on a 6-hour expert
task and gets them wrong inside rof's bounded 2–4 turn loop.

**The token axis is measurable after all.** The endpoint returns full `usage`
(`prompt_tokens`, `completion_tokens`, `reasoning_tokens`, `cached_tokens`), and
`openrouter.rs` already parses it — the local run reported `tokens in/out: 3874/3844,
cache-hit input: 46%`. The earlier claim that `input_tokens` is always 0 was measured
through Harbor, where the trajectory conversion drops it. Direct calls carry it. This
reopens the efficiency measurement and invalidates the "cost axis is unmeasurable"
decision.

**One live hazard the threshold does create.** `volatile_budget()` = `short.budget * 4`
= 6,000 × 4 = **24,000 chars**, which is 11,000 chars *above* the safe threshold. Today
the `[REPO FILES]` layer only ever holds goal-named files, so it stays small — but if a
future arm widens that layer toward budget (exactly the direction the suite-widening plan
points), content emission will die silently. A 24,000-char cap is a footgun calibrated
against a token budget, not against this endpoint's real emission limit.

### The chain, stated plainly

Three claims in two days, each built on an assumption I did not check: "Atria is weak"
(false — 744B GLM-5, 78.3 on TB 2.1), "exhaustion explains the tbench zeros" (false — rof
never approaches the threshold), "token accounting is impossible" (false — the endpoint
returns usage and the parser already reads it). Each was overturned by one direct
measurement. The discipline that would have caught all three is the one already written
down: **measure the substrate before building a conclusion on it.**

### Revised call

1. The tbench ceiling is a **correctness** problem on a 6-hour task inside a bounded loop,
   not an emission problem. That is much harder to fix with a harness lever, and it is
   where expectations should be set.
2. **Demote the `roomier` and `enable_thinking` rungs** — both are proven inert above the
   threshold, and `roomier` doubles the wasted reasoning. They are cost, not insurance.
3. **The suite-widening plan must respect the ~13,000 char emission threshold** as a hard
   ceiling, not the 24,000 char token-derived budget.
4. **Reopen token accounting** — measure cost directly; the endpoint supports it.

## Both measured fixes landed (2026-09-21, this tree)

The two actions above that were harness levers are now code, each
mutation-verified against the pre-fix tree.

**The roomier rung is gated on content.** `reshape_for_truncation` takes the
chars the truncated reply actually shipped and returns whether any rung can
help. The truncation error already reported `content=N chars;
reasoning_content=M chars`, so the decision reads a number the endpoint gave
us rather than guessing. Rungs 1-3 (`reasoning_effort: low` →
`enable_thinking: false` → `reasoning: false`) still fire unconditionally —
below the threshold those are exactly the shapes that let content through.
Only rung 4, the doubled budget, now requires `content > 0`: a truncation that
shipped nothing spent the whole budget on reasoning, and the measurement says
a bigger budget makes the reasoning bigger (72,059 chars at 16,384) while
content stays empty. When no rung applies the function returns `false` and
`complete()` stops instead of paying for a guaranteed-empty call. The
8,000-char-prompt case — 36,892 chars of real content, still growing — still
gets roomier, which is the case it was built for.

**The volatile budget is capped at the emission threshold.** The §4.1 budget
and the reviewer's evidence window both derived `short.budget * 4` = 24,000
chars, which is ~11,000 above the largest prompt size this endpoint still
answers. They now share `volatile_budget_for()`, capped at
`EMISSION_THRESHOLD` (12,000 — the largest measured size that shipped content,
one step below the 13,000 that already degraded). This is a ceiling, not a
replacement: a small configured budget still passes through. The cap had not
been reached by any real task (the repo layer holds goal-named files, 7-8.4
KB), so this is insurance for the suite-widening plan, which points directly at
widening that layer — it was a footgun calibrated against tokens rather than
against the endpoint.

Both changes moved existing tests, which is the honest signal that they
changed behavior, not just bookkeeping. Two loop tests asserted a 20,000-char
requested file arrives whole under the 24,000 budget; that is the exact case
the measurement refutes, since shipping it whole puts the prompt above the
emission threshold where content goes to zero. Their fixtures now sit at ~9.4k
chars, under the cap, so they still test the property they name — a file that
*fits* arrives whole — and a new test covers the other side of the boundary: a
file above the threshold is windowed, and the prompt stays under it. That new
test fails on the pre-cap tree with the intended message.

**What is still open from the revised call:** the tbench correctness ceiling
(item 1) and token accounting (item 4). The `enable_thinking` rung is inert
above the threshold but harmless below it, so it stays rather than being
removed — the measurement bounds it, it does not forbid it.

## The cost axis is measurable, and it is where rof separates (2026-09-21)

The `535d481` decision to abandon cost accounting was wrong: it was a
Harbor-path artifact, not a property of the endpoint. Atria returns a full
`usage` block on every response (`prompt_tokens`, `completion_tokens`,
`prompt_tokens_details.cached_tokens`, `completion_tokens_details.reasoning_tokens`),
and `openrouter.rs` already parsed it.

A wire-level proxy that bills what the endpoint bills measured both harnesses
on the whole multi-file suite, same model, same seeds, same goals, same oracle,
three reps each. Both agents pass 18/18 — the score is still saturated — but
the billed-token cost is not:

| task | rof (3 reps) | hermes (3 reps) | ratio |
|---|---|---|---|
| mf-add-validator | 7,552 / 8,819 / 7,628 | 38,508 / 42,375 / 52,164 | 5.5x |
| mf-dead-code | 10,685 / 2,030 / 2,111 | 121,678 / 45,118 / 40,175 | 14.0x |
| mf-fix-import-cycle | 14,510 / 13,950 / 10,606 | 7,521 / 60,371 / 11,181 | 2.0x |
| mf-new-endpoint | 3,977 / 2,189 / 2,346 | 22,218 / 20,444 / 24,212 | 7.9x |
| mf-rename-key | 4,303 / 3,936 / 2,240 | 79,904 / 21,603 / 44,361 | 13.9x |
| mf-test-coverage | 15,354 / 15,246 / 14,835 | 24,083 / 64,752 / 8,835 | 2.1x |
| **mean per task** | **7,906** | **40,528** | **5.1x** |

The separation holds on every task and every rep (worst case 2.0x, best 14x).
This is the Arena.ai HarnessTax prediction confirmed by direct measurement:
harness choice moves cost by multiples while success stays flat.

Two details the first reading got wrong, both corrected by measuring rather
than by reasoning:

- hermes sends a ~31k-character system prompt on every call, so its raw input
  is ~80k tokens per task. But 99% of that is a cache hit once warm, so its
  *billed* cost is a tenth of the raw figure. Comparing raw input would have
  overstated hermes's cost by 10x in rof's favour — the same error class as the
  scoring correction, in the opposite direction.
- a cold cache pays the full system prompt once: hermes's first call of a
  session bills ~16k tokens where the second bills ~1.4k. The reported ratios
  are per-session and include that cold start, so they are the honest
  end-to-end figure rather than the warm-cache best case.

`EvalReport::billed_tokens()` now reports uncached input plus output as a
first-class metric, mutation-verified, because it is the only axis on which
the harnesses currently differ and it was previously derivable only by hand
from two separately-printed numbers.

## 2026-09-21 — the widening instrument is built and validated

The mf suite discriminates on cost but is saturated on score, so a score claim
needs a wider suite that sits in Atria's discriminating band. Research
(`docs/RESEARCH-suite-widening.md`) surveyed the candidates and rejected two:

- **SWE-bench Verified** is disqualified by its own auditor: 59.4% of an audited
  subset have tests that reject functionally correct submissions, and every
  frontier model tested could reproduce the gold patch, i.e. the set is in
  training data. This is the contamination caution made concrete.
- **EmulatorBench** sits in the right band (GLM-5.2 at 0.208) but is not
  released as an installable package; only its design is reusable.

**SWE-smith** is the instrument, and it now works here end-to-end on Atria —
verified, not assumed. On `theskumar__python-dotenv.2b8635b7`:

- 12/12 candidate entities produced an applicable diff, 8-89s each;
- the soundness gate (apply to the real repo at the pinned commit, run the
  suite, keep only what breaks a test on a green baseline of 149 passed)
  **kept 1 and rejected 11 as vacuous**;
- the one kept task is an off-by-one in `parse_variables` (`len(value) - 1`
  hoisted as a fake performance refactor) that breaks a real unit test.

That 1/12 yield is the finding that shapes the next step: **the binding
constraint on suite generation is the target repo's own test coverage**, not
model quality. python-dotenv's 149 tests do not exercise most of `variables.py`,
so most injected bugs break nothing. Repo choice must be made on test density
and yield measured per repo before committing.

Three silent-failure blockers were found and solved, each of which killed every
run with no error: litellm's cost calculator raises for any model absent from
its price map (register a zero-cost `Atria-Dawn-Preview` entry in
`litellm.model_cost` — the generation call itself succeeds in 2.6s, the crash is
in the cost step); `swebench` is an optional extra and its newest version removed
`DOCKER_USER` (install `swesmith[all]`, pin `swebench==3.0.17`); and the
`tree_sitter_*` adapters import every language eagerly, as do `litellm`,
`jinja2` and `astor`. Artifacts and drivers are at
`~/.local/share/rof-widening/`.

## 2026-09-21 — the no-op failure reproduced and two real bugs fixed

Piloting the one kept SWE-smith task (the off-by-one in python-dotenv's
`parse_variables`, 35 tests failing) reproduced the Terminal-Bench failure in
miniature and, unlike Terminal-Bench, exposed the cause. For four consecutive
reps rof read the source, correctly refused to invent a patch, changed nothing,
and **exited 0** — a no-op that no caller could distinguish from success.

The pilot oracle was first invalidated by a trap worth recording: an editable
install of the same package elsewhere on the path made pytest import a *different
clone* than the one rof edited, scoring an untouched tree as 149 passed. The
oracle now pins `PYTHONPATH` to the repo under test and asserts the import path
matches before scoring.

Two real bugs, each verified to fail on the pre-fix tree:

1. **The test suite was gated on non-empty writes.** `run_tests` returned `None`
   when the implementer wrote nothing. The implementer has no `proc.run` tool, so
   the suite is its only window onto the failure: the model will not patch a
   failure it cannot see, writes nothing, and the report never fires. A
   deadlock. The gate is removed.
2. **A `src/`-layout package was not importable from the repo root.** With the
   gate gone the report came back empty — pytest errored at collection and the
   failure lines never existed, which read to the model as "no signal" while the
   real suite was red. `run_tests_summary` now puts `src/` on `PYTHONPATH` when
   the layout calls for it.
3. **`main` returned `Ok(())` regardless of the reviewer verdict.** A failed task
   now exits 3, so Harbor, CI, and comparison scripts can see the difference.

With both fixes the failure signal reaches the model: `Failing:` now carries
`assert (0, 'x=a b \n') == (0, 'x=a b c\n')` at `test_cli.py:37`, and the
reviewer pinpoints the regression ("the CLI list path drops the last character")
and names the correct files. rof still does not emit the patch at `max_review_rounds`
= 2, and raising it runs into the 50k per-task token cap — so the remaining gap is
the round/token budget for read-then-patch, not the failure signal.

One test-writing lesson, the third instance of it now: the first version of the
src-layout test passed vacuously because the fixed header prose contains the
word "assert", which was exactly what the assertion checked for. The check now
strips the header and requires a real failure line.
