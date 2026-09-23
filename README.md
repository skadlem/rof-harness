# rof — a local agent harness with a meta-harness

A self-contained Rust agent runtime. It owns its orchestration, context handling, tools and
evaluation; only model inference goes to an external API. One model does everything by
default (an explicit `ROF_CTX_MODEL` still buys a two-tier A/B) — the arms never showed
a cheap planning tier earning its keep, so context assembly is fully deterministic:
selection, budgeting, windowing and dedupe by rule, never summarization by model.

Orchestration is `Planner -> (Implementer -> Reviewer)*` (or a single direct executor with
`ROF_PLANNER=skip`), bounded by rounds, attempts and a per-task token ceiling, with every
model call, tool call, verdict and budget abort written to a durable JSONL trace that the
eval layer folds into metrics.

## Measured performance (DeepSeek V4.1 Flash via OpenCode Go, Sep 2026)

`eval/suites/repo-tasks.json` (20 tasks: 15 gated on `cargo check`/`cargo test`, 5 oracle-less
analysis judged by the reviewer), best config (`ROF_PLANNER=skip`, `ROF_EXPLORER=yes`,
`ROF_ATTEMPTS=3`, `ROF_MAX_ROUNDS=4`, 250k task cap), three reps:

| rep | total | gated (15) | analysis (5) | billed tokens |
|---|---|---|---|---|
| 1 | **18/20 (90%)** | 13 | 5 | 556k |
| 2 | 16/20 (80%) | 12 | 4 | 713k |
| 3 | 17/20 (85%) | 12 | 5 | 642k |
| **mean** | **17/20 (85%)** | **12.3** | **4.7** | **~640k** |

What moved it, in order (each measured against the row above on the same model):

| lever | effect |
|---|---|
| `ROF_PLANNER=skip` | +4 tasks (14/20 → 18/20) at **2.5× fewer billed tokens** (1.40M → 556k); analysis went 2/5 → 5/5 |
| `ROF_EXPLORER=yes` | retriever-class tasks 0/3 → **3/3** — evidence before action kills both recon-paralysis and wrong-guess patches |
| `ROF_ATTEMPTS=3` + 250k cap | multi-hunk tasks start landing (0/2 → 1/2 on the hard pair); without headroom, 3 attempts die by budget, not capability |
| `ROF_THINKING=off/low` | **no effect** — kept as an A/B lever, not a setting |
| top-level `reasoning_effort` | moved the hardest bug from silence to action (see band below); vLLM-style thinking switches are ignored by this endpoint |

Generated band (SWE-smith on `pallets__click`, kept iff the bug breaks a real test;
counted by independent oracle that asserts it imports from the tree under test):

| bug (tests broken) | rof | hermes | pi |
|---|---|---|---|
| `pass_context` arg order (1) | ✓ 14min | ✓ | ✓ |
| inverted `startswith` (6) | ✓ 15min | ✓ | ✓ |
| param-hint join (22) | ✓ 11min | ✓ | ✓ |
| gutted `ProgressBar` (35) | ✗ (timeout/silence/wrong-edit over 3 tries) | ✓ | ✓ |

The last row is reported the same way as the wins: on one fixed model everything ties
except the hard edge, where rof is the lone failure — a harness-attributed gap on
large-deletion tasks with non-converging reasoning, and the current top priority.
Rival cells are single reps (their variance is unmeasured); rof cells are 1 rep each on
the band, 3 reps on the suite. Wall-clock (~40min per full-20 rep) was the binding
constraint throughout; total quota ≈ $2–3 of the model's $60/mo cap.

Earlier era (Atria, quota-free endpoint): mf-suite 18/18 tied with hermes at 5.1× fewer
billed tokens/task (7,906 vs 40,528, all 18 cells); Terminal-Bench 4.0 0/8 for all three
harnesses on a correctness wall. The repo-tasks numbers above supersede the old headline
(Atria best 12.7/20) on a stronger model with a better harness.

## Build and run

```bash
cargo build --release                 # single binary: target/release/rof
./target/release/rof "add a doc comment to src/lib.rs"      # run one goal
./target/release/rof eval eval/suites/repo-tasks.json       # run a suite
./target/release/rof eval eval/suites/repo-tasks.json --limit 6 --jobs 2   # first 6, two at a time
./target/release/rof config > my.json                       # effective config (diffable)
./target/release/rof compare a.json b.json                  # delta between two --report dumps
./target/release/rof skills list                            # SKILL.md store + pending proposals
./target/release/rof skills approve <id>                    # apply an agent's proposed skill
./target/release/rof --config my.json eval suites/repo-tasks.json
scripts/live-eval.sh eval/suites/repo-tasks.json --limit 6 --jobs 2        # same, wired to a live model
scripts/live-go.sh eval/suites/repo-tasks.json --limit 6 --jobs 2          # same, via OpenCode Go (needs ROF_TOKEN, see below)
scripts/measure-arm.sh before 3 eval/suites/repo-tasks.json --limit 6 --jobs 2   # an arm: 3 labelled runs
```

Live models need credentials in the environment (`OR_TOKEN` for OpenRouter, or
`DEEPSEEK_API_KEY`/`ROF_TOKEN` with `ROF_CHAT_BASE` for a direct OpenAI-compatible endpoint).
Without them the harness runs against a deterministic stub, which is how the tests exercise the
full loop offline.

OpenCode Go (DeepSeek V4.1 Flash) is the current live path: get the key via opencode
`/connect` → OpenCode Go, then either `export ROF_TOKEN=<key>` or write it to
`~/.rof/go.key` (chmod 600, outside the repo — never committed). `scripts/live-go.sh`
reads one then the other and pins both model slots plus the reasoning-effort default:

```bash
export ROF_TOKEN="<your-go-key>"                 # or: echo "<key>" > ~/.rof/go.key
scripts/live-go.sh eval/suites/repo-tasks.json --limit 6 --jobs 2
```

Go requires an `x-opencode-session` routing header, which rof sends (per-client UUID,
OpenCode endpoints only) alongside a `rof/<version>` user agent.

**Always point `ROF_WORKDIR` at a scratch copy** — the harness edits the working directory,
and a run commits a baseline before each attempt (`git init` plus one commit if the tree is
not already a repo):

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
| `ROF_CTX_MODEL` / `ROF_EXEC_MODEL` / `ROF_EXEC_FALLBACK` / `ROF_VERIFY_MODEL` | model routing without recompiling; empty/unset ctx follows exec (single-model default), verify defaults to exec (self-review) |
| `ROF_SUMMARIZE_AT` | `0.0`-`1.0` or `off` — opt back into mid-layer summarization for an A/B (default `off`: the arm cost 8/12 → 2/12) |
| `ROF_VERIFY_TOKEN` / `ROF_VERIFY_BASE` | the judge on a *different provider* than the executor (arm #4); unset = the shared client, run unchanged |
| `ROF_ALLOW_CMDS` | comma-separated exact command allowlist (empty = deny all) |
| `ROF_ALLOW_HOSTS` | comma-separated host allowlist for `http.get` (empty = deny all) |
| `ROF_EXPECT_WRITES` | `yes`/`no` — run mode: must the goal change a file to pass (default yes) |
| `ROF_PLANNER` | `always` (default) or `skip` (treat every goal as one task) |
| `ROF_MAX_ROUNDS`, `ROF_MAX_TASK_TOKENS`, `ROF_COST_LAMBDA` | loop and scoring knobs |
| `ROF_TRACE` | JSONL trace path (append-only, one line per event) |
| `ROF_CHECK` | run-mode checks, comma-separated exact commands |
| `ROF_JOBS` / `ROF_MAX_PARALLEL_TASKS` | suite fan-out (`--jobs N` wins over both) |
| `ROF_TASK_ROOT` | where per-task workdir copies live (default: system temp dir) |
| `ROF_SKILLS_ROOT` | SKILL.md store (default `~/.rof/skills`) |
| `ROF_SKILLS_POLICY` | `readonly` / `propose` (default) / `direct` — how a manage op lands |
| `ROF_GOAL_QUALITY` | `yes`/`true`/`1` — run the local goal-quality pre-check and put its note in the planner prompt (off by default; changes prompt content) |
| `ROF_AUTO_POKE` | `yes`/`true`/`1` — buy one extra implementer round when a task fails at its round cap (off by default; changes rounds) |
| `ROF_EXPLORER` | `yes`/`true`/`1` — one read-only exploration pass before implement, key files join the volatile tail (off by default) |
| `ROF_ATTEMPTS` | `1`-`5` — independent task attempts, cheapest-pass wins (default 1 = historical run) |
| `ROF_VERIFY_GUARD` | `yes`/`true`/`1` — one post-hoc independent-judge pass; a veto fails the attempt (off by default) |
| `ROF_ALLOW_PREFIXES` | comma-separated `proc.run` prefixes (`cargo test` covers `cargo test foo`, never `cargo test-evil`) |
| `ROF_THINKING` | `off`/`low`/`on` — implementer + reviewer reasoning starting posture (default `on`; measured no effect, kept for A/B) |
| `ROF_IMPLEMENTER_MAX_TOKENS` | per-call output cap for implementer calls (default 8192, clamped to [1024, 32768]) |
| `ROF_REVIEWER_MAX_TOKENS` | per-call output cap for reviewer calls (default 4096, clamped to [1024, 32768]) |
| `ROF_REASONING_EFFORT` | `low`/`medium`/`high`/`none` — top-level effort field; the knob Go-style endpoints honor (vLLM switches are ignored there); unset = absent from the wire |

Precedence: defaults < config file < environment < CLI flags. `rof eval <suite> --report out.json`
writes the suite report (per-task verdicts + feedback, folded metrics) next to the trace, so a
baseline survives the terminal it was printed to.

## Comparing runs

Every report carries a label — `git_head`, `config_hash`, `suite_hash`, the model pair, the harness
version — computed from inputs that are canonical by design (the config dump and the suite JSON), so
two runs are comparable instead of anecdotal. `rof compare a.json b.json` prints the label diff, the
matched totals, every task that moved (with the losing side's feedback) and the metric deltas; it
warns when the two reports are not the same task set or config rather than dressing a set difference
up as an effect. A moved task also names the check that flipped (`check 'cargo test' flipped: fail ->
pass`) when the acceptance gate is what moved — and reports no flips when it was not, which is how
you tell a gate effect from a verdict effect.

Each task result also carries context accounting: `retrieved_files`/`retrieved_chars` (what the
retriever put in front of the agents), `referenced_chars` (of those, the chars whose file the task
actually touched), `relevance_proxy` (the ratio — a proxy, not a judgement), plus
`summarize_calls`/`summarize_tokens`, `truncated_views` and `eliminated_chars` (the bytes a
duplicate carried into a prompt the §4.1 assembler refused to deliver twice). Reports written
before these fields existed still load: the fields default to zero.

`docs/STATUS.md` records the measured arms and the bugs they found, including the two that were
not fixes: an eager-files rewrite that turned out to duplicate retrieval already in the prompt, and
a third review round that cost tokens without moving a task. The headline result is `ROF_PLANNER=skip`
on this suite, where every task is a single change — three reps at 12.7/20 against 7.3/20 with the
planner on, at 200k fewer input tokens.

The suite total is only quotable with its split, because five analysis tasks carry no deterministic
oracle and the reviewer was their only judge. As of the last arms the honest numbers are:

| arm | judge | gated (15) | analysis (5) |
|---|---|---|---|
| planner on | self | 6-8 | 0-1 |
| `ROF_PLANNER=skip` | self | 11-12 | 1-2 |
| `skip` + independent | `stealth/union-alpha` | **11.3 (76%)** | 0.7 |

Multi-file suite (6 tasks × 3 reps, Atria): **rof 18/18, hermes 18/18, pi 16/18.**
These are the verified numbers, and they replaced an earlier claim that was wrong — see
below. On Terminal-Bench 4.0 all three score 0.0 on 8 of 8 CPU tasks at 15- and 30-minute
caps. **That floor is no longer attributed to the model** — see the correction below, but
in short: Atria is a 744B GLM-5 MoE that scores 78.3 on Terminal-Bench 2.1, so a model
competent on this benchmark family does not fall to zero across eight tasks because it is
weak. The gap implicates the harness path, and the exhaustion quirk we already found
(`content: null`, `reasoning_content=37683`) on large prompts is the prime suspect. It is
recorded as an open problem, not a settled ceiling.

**The suite is saturated on score — but not on cost.** With all three at or
within two of the ceiling, the multi-file suite has nothing left to discriminate
on *success*: a future arm that moves nothing will still score ~18. Widening it
(larger repos, indirect call chains, cross-file consistency) is now a
prerequisite for a score claim, not a nice-to-have.

Cost is the axis that still separates rof from hermes at equal success. Both
pass 18/18 on the same model, seeds, goals, and oracle, measured through one
wire proxy that bills what the endpoint bills: **rof averages 7,906 billed
tokens per task, hermes 40,528 — a 5.1x gap that holds on every task and every
rep** (worst case 2.0x, best 14x). Billed means uncached input plus output:
hermes re-sends a ~31k-character system prompt per call, but 99% of it is a
cache hit once warm, so raw input would have overstated its cost by 10x. This
confirms the Arena.ai HarnessTax prediction — harness choice moves cost by
multiples while success stays flat — and it is the first axis on which rof
measurably leads since the score saturated.

**Correction, 2026-09-21.** This section previously read "rof 17/18 vs hermes 15/18 and pi
14/18, the separation is `mf-dead-code`, which rof passes 3/3 and both others 2/3."
**That claim did not survive verification against the run's own artifacts.** Re-scoring all
54 stored work dirs — with source files untouched since the original run window — agrees
with the stored result files on every single cell, and both say hermes 18/18 and pi 16/18,
with `mf-dead-code` passed 3/3 by *all three* agents. There was no separation on that task;
the lead it appeared to give rof was a recording error, never checked against the artifacts
it came from. The corrected numbers above are what the stored evidence actually supports.

The 18/18 comes from the red-suite arm: a seeded test asserted the *buggy* value
and was rejecting correct fixes; the harness now surfaces the failing assertion to
the model (`8230015`).

Three findings changed what the numbers mean, and each was caught by decomposing the measurement
rather than trusting the aggregate: a direct-mode run that scored 16/20 was passing the five
oracle-less tasks vacuously (`checks_pass(&[])` is true); the analysis class was unsatisfiable
because the model's prose answer travelled nested where the judge never read it; and the endpoint
returns `content: null` on 7 of 8 probes, which the harness had been scoring as a model that chose
to write nothing. The first is now a fast `no oracle` failure, the second an `ANSWER:` line, the
third a retry.

## Skills

Procedural memory, in the agentskills.io shape: `~/.rof/skills/<name>/SKILL.md` with YAML-ish
frontmatter (`name`, `description`, optional `version`/`tags`) and optional support files
(`references/`, `scripts/`, anything). A repo can ship skills with it in `<workdir>/skills/`
(read-only, and never writable: the harness does not edit a checked-in tree behind your back).

Disclosure is progressive, because a prompt that carries every procedure pays for all of them:

- The **index** — `- name: description`, one line each, sorted, capped — rides the stable head of
  the planner, implementer and reviewer prompts. It is byte-stable, so it stays in the provider's
  cached prefix.
- A **body** is fetched only when it is needed: the task names the skill (the same rule the
  retriever uses for a goal-named file), or the model asks for it with `skill_views: ["name"]` and
  gets one bounded extra turn.

Writes are proposals by default. `skills.manage` (create / patch / write_file / delete) validates
the op and writes `~/.rof/proposals/skills/<id>.json`; nothing reaches the store until a human runs
`rof skills approve <id>` (`reject <id>` marks it refused and keeps the record). `skills.policy`
can be set to `direct` for a trusted loop, or `readonly` to freeze the store.

```bash
rof skills list                 # index + pending proposals
rof skills show <name> [file]   # a body or a support file
rof skills approve <id>         # the human half of the loop
```

Grants: planner `skills.list`; reviewer `skills.list`/`skills.view`; implementer all three. The
skills root is deliberately **not** on the file-tool allowlist — `fs.write` must not become a way
around the write policy.

## Fan-out

`rof eval <suite> --jobs N` (or `max_parallel_tasks` in config) runs up to N tasks at once,
and `--limit K` runs only the first K tasks — a baseline without a second suite file to drift.

Isolation is by copy: each task gets its own scratch copy of the workdir under
`task_root`, so two writers never share a tree and the source tree is never a target.
`target/` is excluded from the copy (rebuildable, and by far the largest subtree); `.git/` is
included as the tree-state substrate — shallow (`--depth 1`) when the source history is large,
`git init` plus one commit when the source was no repo — so every attempt starts from a
committed baseline and the write gate counts what git sees change, not what the model reports.
Agents never reach `.git`: it is excluded from retrieval and denied by every file tool. Results
are recorded in suite order regardless of finish order, and a copy that fails to be made fails
only its own task.

Disk is the price of that isolation: a task whose checks build owns its own `target/`
(~250 MB for `cargo check`, ~1 GB for `cargo test` on this repo). Point `ROF_TASK_ROOT` at
disk rather than a small tmpfs, and `rm -rf` the copies when you are done with them — keeping
them is how a MISMATCH gets inspected after the fact.

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
- `fs.patch` refuses ambiguous or missing search strings rather than guessing. A refusal is not a
  dead end: the file's current text is re-read and handed to the retry round, so the next attempt
  anchors on what is there.
- The reviewer cannot write; a pass with no writes is rejected by the harness itself, not only by
  the reviewer prompt.
- The one place an agent could edit its own instructions — the skill store — defaults to
  proposals: `skills.manage` validates an op and writes a proposal file; only `rof skills approve`
  applies it. The skills root is not on the file-tool allowlist, so `fs.write` cannot bypass that.

## Layout

```
src/engine/   session (RoundServices, CheckResult, Budget), orchestrator (supervisor loop), router (Context/Executor/Verify)
src/agents/   planner, implementer, reviewer — one file per role behind the Agent trait
src/context/  layered state (long/mid/short), budgeted builder, keyword retriever, §4.1 assembler
src/skills/   SKILL.md store: frontmatter parser, index, proposals, approval
src/tools/    registry + permission gate + fs.list/read/write/patch, proc.run, http.get, skills.*
src/llm/      LlmClient trait, Context/Executor services, OpenAI-compatible client
src/eval/     suite loader, runner, trace-folded metrics, report compare
src/obs/      trace events + append-only JSONL sink
src/config/   budgets, routing, permissions, pricing as versioned structs
```

## Testing

```bash
cargo test && cargo clippy --all-targets && cargo fmt --check
```

Tests default to `std::env::temp_dir()` and clean up the task copies they make. Live runs leave
copies under `ROF_TASK_ROOT` (the temp dir by default) — they are disposable; `rm -rf` them.
See `docs/STATUS.md` for what is done, what is left, and how to verify each claim.
