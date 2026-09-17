# Status

Last verified: 2026-09-16, on this working tree. v3 components §4.2 (tree-state
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
