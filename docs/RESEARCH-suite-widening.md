# Widening the multi-file suite — candidate research (2026-09-21)

## The problem, restated

The 6-task multi-file suite is **score-saturated**: rof, hermes and pi all sit at
18/18 (pi 16/18), so no harness improvement can register on it as a success-rate
change. The suite still discriminates on **cost** (rof 7,906 vs hermes 40,528
billed tokens per task, 5.1x, all 18 cells), but a score claim needs a suite
that sits in Atria's **discriminating band** — harder than the current mf tasks,
not so hard that every harness scores zero.

Terminal-Bench 4.0 is the counter-example already measured: 8/8 CPU tasks at
zero for all three harnesses at 15- and 30-minute caps. That is a wall, not a
band, and its attribution was withdrawn. The widened suite must land between
the two.

Selection principle carried forward from earlier work: **a suite is only useful
if Atria can partially solve it.** A task set where the model scores 0 or 100
measures nothing about the harness.

## Constraints (all measured, not assumed)

| constraint | value | consequence |
|---|---|---|
| model | Atria-Dawn-Preview only (quota-free; every paid key is exhausted) | any suite needs a custom-model path, not a vendor SDK |
| GPU | none available | CPU-only tasks; R2E-Gym/SWE-smith Python repos qualify, GPU-kernel tasks do not |
| Docker | **works** (`docker ps` clean, 12 cores, 14 GB RAM, ~44 GB free) | Docker-backed suites are unblocked; this was the gate, and it passes |
| prompt size | content emission collapses above ~13,000 chars of user prompt | a suite whose task statement is huge will read as "model emits nothing" |
| budget | $0 | self-hosted verifiers only; no paid grader or judge API |
| contamination | Wang et al. (2607.12227) caution: harness-evolution gains are overstated when search and eval overlap | fresh/generated tasks preferred over well-known public ones |

## Candidates researched

### SWE-bench Verified / Lite — DISQUALIFIED
OpenAI's own audit (`why-we-no-longer-evaluate-swe-bench-verified`) found
**59.4% of a 27.6% audited subset have flawed tests that reject functionally
correct submissions**, and that **all frontier models tested could reproduce the
gold patch or verbatim problem-statement specifics**, i.e. the set is in
training data. Their recommendation is to stop reporting it. This is the exact
failure mode the contamination caution predicts, plus a verifier that errs
against correct work. Not usable as a widening target.

### "Harness or Model?" (arXiv 2609.11987) — methodology, not a suite
Private, contamination-controlled 256-task suite; 792/800 runs graded by an
isolated oracle. Findings that mirror ours and constrain the design:
- same-model harness contrasts do **not** resolve a success difference
  (Opus −1.25 pp, GPT-5.5 +1.25 pp, CIs crossing zero) — consistent with our
  saturated 18/18;
- cost per solved task differs 1.3–1.6x — consistent with our measured 5.1x
  billed-token gap, and the two studies agree on the *sign* of the effect;
- **the effect is stratum-dependent**: the native harness trails by 9.0 pp on
  61 repository tasks and leads by 23.7 pp on 19 contest tasks
  (label-permutation p = 0.003, but the partition was chosen post hoc and the
  authors flag it as needing a designed replication);
- 22/81 runs cancelled at the wall-clock ceiling had already produced a passing
  patch — **a timeout is not a failure**, which is a scoring rule to adopt.

The suite itself stays private; the released artifact is the orchestrator and
grading oracle. Lesson to adopt: mix strata deliberately rather than assuming a
single average.

### Arena HarnessTax — confirms the cost framing
Harness choice moves success by ±2–5% but cost by up to 5x. Our own 5.1x
billed-token measurement is on the strong end of that range, so the widened
suite should **report score and billed tokens together** rather than score alone.

### GBA Eval (Mechanize) — a wall, not a band
One long-horizon task: write a Game Boy Advance emulator in Rust compiling to
WebAssembly, graded by lockstep replay against a Mesen2 reference. The grading
is beautiful (the GBA has no entropy source, so replays are deterministic) but
it is a *single* task at the far-hard end. Frontier models score low; for Atria
it would almost certainly read as zero. Not a widening instrument.

### EmulatorBench (Prime Agent's suite) — not separately released
Prime Agent's table places GLM-5.2 at **0.208** on EmulatorBench against
Prime 0.208 / Pi-mono 0.000 / Codex 0.228 — the band is right, and the design is
the useful part: "construct emulators in Rust for a variety of game systems",
scored by a **stepwise verifier** giving partial credit. But it is not
installable; it lives inside PrimeIntellect's own harness. Take the design
(stepwise partial-credit verifier on a CPU build task), not the package.

### R2E-Gym — viable, procedural, contamination-controlled
COLM 2025. 8.1k procedurally generated environments across 13 repos, Docker
images 300–500 MB each, unit-test reward via `env.runtime._calculate_reward()`,
plus an execution-free verifier agent for reranking. Two properties matter:
tasks are **generated from commits, not human PRs**, so they are far less likely
to be in training data; and difficulty is selectable by repo/commit. Requires
Docker (which we have). Cost: one image per repo, and the environment Python API
assumes the R2E agent harness — integrating rof/hermes/pi means driving the
container directly rather than through their `Agent` class.

### SWE-smith — **the instrument for building a widened suite, installed and working here**
NeurIPS 2025 D&B Spotlight. Not a benchmark but a **generator**: "turn any
GitHub repository into a SWE-gym" and create **unlimited** bug-injected task
instances, each kept only if it breaks ≥1 unit test (so the oracle is sound by
construction — the negative control is built into generation). Outputs a
`bug__<type>__<hash>.diff` plus metadata per instance.

Verified working in this environment (venv preserved at
`/home/madiyar/.local/share/rof-research/swesmith-venv`; the working venv with all
transitive deps resolved is `/tmp/swesmith-venv`). `swesmith` imports, `registry`
loads **673 repo profiles** (1,346 with the `swesmith/` mirror namespace), and
bug generation ran end-to-end on Atria. See "What was verified" below for the
full dependency list; the short version is `swesmith[all]` plus
`swebench==3.0.17` plus the `tree_sitter_*` family plus a `litellm.model_cost`
entry for Atria.

## What was verified (not assumed)

The SWE-smith path was exercised end-to-end against Atria on
`theskumar__python-dotenv.2b8635b7`, and it works. Measured results:

- **Generation works.** 12/12 candidate entities produced an applicable diff,
  8–89 s each (mean ~46 s), all on `Atria-Dawn-Preview`.
- **The soundness gate works and is essential.** Gating the 12 diffs against the
  real repo at the pinned commit: **1 kept (breaks a test), 11 vacuous** — the
  baseline is 149 passed / 1 skipped, and 11 of the 12 bugs leave that unchanged.
  A suite built without the gate would be 92% tasks that pass on the unmodified
  seed, i.e. exactly the vacuous-task failure mode.
- **The one kept task is the right shape.** `parse_variables` gains
  `length = len(value) - 1` hoisted above the loop, deleting the correct
  `len(value)` below — an off-by-one presented as a performance refactor. It is
  subtle, realistic, single-function, and it breaks a unit test. That is a
  discriminating task, not a toy.

Artifacts: `/home/madiyar/.local/share/rof-widening/dotenv-probe-01/`
(12 diffs + metadata + `gate.json`), drivers `gen-bugs.py` / `gate-bugs.py`.

Three blockers were hit and solved, each of which silently killed every run
with no error message:

1. **litellm does not know Atria.** `swesmith.bug_gen.llm.modify` imports
   `completion_cost`, which raises `This model isn't mapped yet` for any model
   absent from `model_prices_and_context_window.json`. The generation call
   itself succeeds in 2.6 s; the crash is in the *cost* step, and it surfaces
   only as a hung progress bar. Fix: register a zero-cost entry in
   `litellm.model_cost` before generating.
2. **`swebench` is an optional extra, and the newest version broke the API.**
   Bare `pip install swesmith` omits `swebench`, `docker`, `python-dotenv`,
   `ghapi`, `litellm`, `jinja2`, `astor`, and the whole `tree_sitter_*` set
   (the adapters import every language eagerly). Install `swesmith[all]`, then
   **pin `swebench==3.0.17`** — 5.x removed `DOCKER_USER`, which swesmith
   imports from `swebench.harness.constants`.
3. **The bundled configs are not in the wheel.** `--config_file` is required but
   no `configs/` ships with the package; clone the repo and point at
   `configs/bug_gen/func_fun.yml`.

Also: Python 3.14 needs a venv (PEP 668), and the console script is not
installed, so invoke as `python -m swesmith...`.

## Yield is the new cost driver

At 1/12 kept, python-dotenv's coverage is too thin to be an efficient source —
its 149 tests simply do not exercise most of `variables.py`. **The binding
constraint on suite generation is the repo's own test coverage**, not model
quality: on a well-covered repo most injected bugs will break something, and on
a thinly-covered one almost none will. Repo choice should be made on test
density, and yield should be measured per repo before committing to it
(same selection principle as everywhere else — measure the substrate).

## Recommendation

Two-layer widening, deliberately mixing strata (the 2609.11987 finding), with
the mf suite retained as the known-good control:

1. **Generate the widened mf suite with SWE-smith** (primary). Pick 3–5
   CPU-only Python repos of moderate size, generate N bug-injected instances
   each, keep only those that break a unit test, and set difficulty by repo and
   by bug type until Atria lands in the 30–70% band on a probe run. This gives
   an arbitrarily large, **contamination-controlled** suite whose oracle is
   sound by construction, at $0, on hardware we have. It directly replaces the
   hand-authored 6-task suite and can be regenerated if it ever saturates.
2. **Add one stepwise-partial-credit stratum** in the EmulatorBench style — a
   CPU build task with a verifier that scores increments, not pass/fail. This is
   the stratum where the harness effect appeared largest and where a binary
   oracle would otherwise throw away the signal. Build it rather than adopt a
   package, since no installable one exists in the band.

Adopt as standing scoring rules: **report billed tokens alongside score on every
task** (the two studies agree cost is where harnesses separate); and **treat a
wall-clock cancellation as inconclusive, not failed** (22/81 cancelled runs had a
passing patch), re-checking the work tree for a passing result before recording
a zero.

## What remains to verify

- **Yield per repo.** 1/12 on python-dotenv says coverage is the constraint.
  Probe 2–3 better-covered repos (the 673-profile registry includes flask, click,
  jsonschema, pytest's own tooling) and pick those with the highest kept rate.
  This is the one measurement that decides which repos to build from.
- **The discriminating band.** Run the kept tasks through rof/hermes/pi and read
  where Atria lands. If rof solves ~all of them the suite is too easy and needs
  harder bug types or larger repos; if ~none, too hard. Target 30–70%.
- **Prompt size vs the emission threshold.** Each generated task must stay under
  ~13,000 chars of goal text per turn. The capped volatile budget (12,000)
  already guards the file windows; the goal statement itself is the unguarded
  half. The dotenv task statement is tiny, but a task spanning several modules
  could carry a large diff context.
- **Docker image build time and disk** per chosen repo against ~44 GB free.
