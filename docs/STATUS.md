# Status

Last verified: 2026-09-14, on this working tree (`git log` has the v1 commit; this file is the
running handoff).

stage 2 of `docs/PLAN-harness-v2.md` (context policy + cheap Context
LLM). Stages 0 (observability) and 1 (skills) are **done** and recorded below; stage 1 has one live
finding worth reading before touching prompts again: agents wrote no skill unless the goal asked for
one, and the reason is the nudge's own trigger.

**Measured-claim rule for every arm below:** 3+ runs per arm, same `--limit`, same models, compare
per-task matched sets — one 6-task run swings ±2 tasks.

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
| `cargo test` | 73 passed, 0 failed (52 before stage 0, 57 before stage 1) |
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

1. `metrics-method` needs a read/verify behaviour, not context: it is 0/9 across every arm. The
   cheapest experiments, in order: (a) tell the implementer to reuse an existing helper in the file
   it edits before writing a new one; (b) let the artifact request reads (`reads: [...]`) and give it
   one bounded turn with those files before it writes; (c) let the reviewer use the `fs.read` it
   already holds in the policy. All three are prompt-or-plumbing small; measure on
   `eval/suites/hard-two.json` first, then confirm on the full suite.
2. Try a stronger executor model on the same suite (the `RoutingConfig` seam exists). Every failure
   class in every arm is model-side; the harness now reports them honestly, so the next question is
   how much of the ceiling is the model.
3. Independent review: route the reviewer to a second model and measure whether verdicts change.
4. `writes[]` as structured results + one helper for `apply_patches`/`apply_writes`.
5. Symlink-aware containment in `resolve_under` before any suite is allowed to run with a tool that
   can create links.
