# Where this project stands — handoff for the next agent

Last updated **2026-11-06** (branch `v2-core`, HEAD `1e64ec7` + the README/handoff refresh in
this commit). This file is the entry point. Read it, then the two canonical records:

- `docs/roadmap.md` — *Current state (2026-11-06)* at the top, then `Next (ordered, spend-gated)`.
- `docs/decisions.md` — the decision log; every entry carries the measurement behind it.
- `research/DECISIONS.md` — append-only evidence log, one row per arm, including pre-registrations
  written *before* spend. Local only (git-excluded), same as `NEXT.md` and `research/`.

Canonical code-level doc: `docs/architecture.md`. User-facing summary: `README.md`.

---

## 1. What this is

A Rust coding-agent harness (`rof`) that runs an OpenAI-compatible model through six tools
(`view/search/edit/write/exec/test`) against a scratch workdir or **inside a task's own container**,
and grades with the task's verifier. The only mode that counts for benchmarks is
`rof eval --agent --in-container`: the agent's `exec`/`test` go through `docker exec` into the task
container, and the verifier then runs `bash /tests/test.sh` in that same live container, so
installed packages and running services survive to grading.

---

## 2. Where we stand (all numbers measured, faithful pipeline)

Model `deepseek-ai/DeepSeek-V4.1-Flash` (1017), endpoint `https://lighthouse.dphn.ai/run/text-v/v1`,
recipe 300k gross tokens / 60 steps / 120 actions, shipped defaults (`--steer off`,
`--streak-steer off`), in-container.

| Cell | Result | Reading |
|---|---|---|
| log-summary-date-ranges | **3/3 solved** | ceiling; pristine floor 0/2 |
| kv-store-grpc | **3/3 solved** | pristine floor 0/7; ~$0.01/row; live-server deliverable |
| hf-model-inference | **2/3 solved** | r1 died on the task's own 900s agent wall at 28 steps; r2/r3 solved in 309/352s — path variance |
| custom-memory-heap-crash | 0/3 | at its **5/6 floor** (the untouched workspace already passes 5 of 6); the crux test fails even when the model runs `gdb`/`valgrind` |
| video-processing | 0/1 | at its 2/5 floor, token-capped at 33 steps |
| query-optimize | 0/1 | wall-clocked at its 900s agent budget after 12 steps |
| Steering nudges (`--steer on` vs `off`, 12 paired rows) | **KILLED** | capability identical in all 6 pairs; 2.64x tokens per solved task with nudges on. Default flipped to off |
| `--exec-policy any` (3 heap rows) | **mechanism only** | zero denials, `gdb`/`valgrind` actually used, crux test still fails |

So: the harness solves ordinary mid-difficulty tasks at this model tier, and does not solve the
three legacy "headroom" cells. That is a statement about the cells, not about a missing lever.

Excluded cells (broken reference solutions, not to be used for arm selection):

- `mcmc-sampling-stan` — `solve.sh` dies with `no package called 'rstan'` (source build, needs sudo+apt).
- `protein-assembly` — gold 0/1 (stochastic DNAChisel solve, no gblock.txt).
- `sanitize-git-repo` — gold 2/3 (its own solution trips `test_no_other_files_changed`).

---

## 3. What is void, and why (do not cite pre-2026-11-06 in-container capability numbers)

Five measurement defects were found and fixed on 2026-11-06, each with a test. Every in-container
capability reading taken before them is **void** on cells whose verifier tests could not move:

1. **Workspace was the Docker build context, not the task workspace.** The workdir was seeded from
   `<task>/environment`; the verifier grades the image's `/app`. On heap those differ
   (`program/main.cpp` vs `main.cpp`), and heap's verifier md5-checks and compiles `/app/main.cpp` —
   a structurally impossible 0/6. Also wrong on mcmc, query-optimize (and log-summary ran a
   generator-reconstructed variant). `video-processing` was the only cell whose seed matched.
2. **`/tests` was mounted for the whole run**, so the agent could read the grader (one row opened
   `test_outputs.py`). Harbor uploads tests inside `Verifier.verify()`; ours now `docker cp`s the
   frozen snapshot's tests in immediately before grading.
3. **No wall clock on the agent phase** — rows ran until the token/step caps (an mcmc row was still
   working at ~2,100s where harbor stops at 1,800s).
4. **`task.toml` timeouts were never parsed** (`timeout_sec` ≠ `timeout_secs`, and the key appears in
   both `[verifier]` and `[agent]` with different values), so every cell ran on the 1800s default.
5. **Git/ownership and empty-workdir baselines** — the chowned `/app` made in-container `git` refuse
   the repo (`dubious ownership`), and an empty image `/app` left HEAD unborn so a row that resolved
   7/7 still carried a `snapshot rollback: … no start HEAD` halt reason.

Consequences already applied: the steering layer ships **off** (measured dead, not merely untested);
`README.md` marks the 2026-10 arms as mechanism-only; `docs/steering-experiment.md` carries a void
notice; the October "exec policy is hygiene" and "envelope arms killed" verdicts are re-stated as
mechanism-only until re-run.

---

## 4. How to run a paid arm (the only sanctioned way)

Spend is real and guarded. Every arm is **pre-registered before the first call** in
`research/DECISIONS.md`: binary md5, cells, reps, arm flags, decision rule (PASS/KILL), cost
estimate, stop rules.

- **Binary is frozen per arm**: `cargo build --release -p rof`, copy to
  `~/.local/share/rof-tb/work/<arm>/rof-<arm>`, record `md5sum`. Never mix binaries in one table.
- **Ledger carries everything forward.** The guard (`~/.local/share/rof-tb/work/pilot-streak-2026-11-03/rof-budget-proxy.py`)
  enforces a **$10 cumulative ceiling** against `spent_reserved_nanos`; a fresh campaign must seed the
  new ledger with the full accounted total from the previous one. Never reset it to make it pass.
  Current: `pilot-solve2/ledger.json` = **$7.7913 accounted, $2.2087 remaining**.
- **Unknown usage stops the campaign.** If a 200 response's `usage` cannot be parsed, the proxy keeps
  the full reservation, marks the attempt unknown, and denies everything after (`402 … reservation
  exhausted`). That is by design and has already cost several campaign restarts. The proxy now dumps
  the offending body to the ledger directory's `unknown-body.json`; a fresh ledger may carry the
  unknown reservations forward as spend.
- **Row hygiene**: fresh out-dir per row, skip-guarded report, provider/empty-usage rows voided and
  retried once in a fresh directory, drops never scored as capability.
- **Artefact layout**: `~/.local/share/rof-tb/work/<arm>/` holds `rof-<arm>`, `ledger.json`,
  `reports/<arm-row>.report.json`, `<arm-row>/<task>/{events.jsonl,work/,verifier-logs/}` and a
  `dphn-*.log` line per row. Drivers are `pilot-*-driver.sh` next to them.

Quick shape of a row (see any existing driver for the full flag list):

```sh
"$ROF" eval --tasks-dir <tb-tasks> --ids <task> --agent --in-container \
  --model deepseek-ai/DeepSeek-V4.1-Flash --endpoint http://127.0.0.1:$PROXY_PORT/v1 \
  --exec-policy any --steer off --streak-steer off \
  --out-dir <arm>/<row> --report <arm>/reports/<row>.report.json
```

---

## 5. Free instrumentation that already exists (use it before spending)

- **Floor scan** — the do-nothing score of a cell, and whether its gold solution passes:
  `~/.local/share/rof-tb/work/floor-scan-2026-11-06.md` plus `/tmp/floor-scan2/results*.jsonl`
  (12 cells measured; all are pristine 0/N except heap 5/6, video 2/5, query-optimize 2/6, mcmc 2/6).
  Read a row's named-test count as **floor + delta**, never as raw progress.
- **Nudge census** — count `Unverified declare held` / `0 edits so far after` / `actions remain
  before` / `wrap up: step` in a row's `work/.rof-events.jsonl` to prove an arm's mechanism fired.
- **Denial census** — `denied: command not allowlisted: X` in any row's `events.jsonl` (666 across
  18 rows, top classes `sqlite3`, `which`, `curl`, `valgrind`, `./release`).
- **Traces** — `.rof-events.jsonl` is the WAL; `events.jsonl` is the replayable event dump.

---

## 6. Next, in order (spend-gated)

1. **Re-measure the completion checklist in the goal** — the only lever that was ever
   capability-positive (`dphn-goalchk`: 5/6 → 6/6), and its verdict is void because it ran pre-fix.
   Pre-register it on the faithful pipeline; `hf-model-inference` (2/3, wall-bound) and
   `kv-store-grpc` (3/3, live server) are the natural cells. ~$0.1-0.2.
2. **hf-model-inference × 3 more** to firm up the one cell with real pass^1 variance (2/3), only if
   a lever hypothesis is being tested against it.
3. **Broader pass-rate baseline** on the 7 valid portfolio cells (0/N floors, gold passing): ~$0.05
   per row, gives the number every future lever is measured against.
4. **Parked / not worth spend**: envelope arms (void, mechanism-only), reasoning effort (dead on both
   channels), compaction (inert by construction), model swap (retired by owner), judge
   instrumentation (we now have named ctrf tests, which are a stronger signal).

---

## 7. Finish gate and repo hygiene

```sh
cargo test --workspace                                   # 436 tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

- `docs/` and `README.md` are tracked and must never drift from code; `research/`, `NEXT.md` and
  this file are git-excluded local records (`.git/info/exclude`).
- Commits carry no AI attribution.
- Any numeric claim needs provenance: measured (source + numbers), inferred (from measured), or
  chosen (no anchor — name what validates it).
