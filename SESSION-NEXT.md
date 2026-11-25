# Session handoff · 2026-11-25

## Pick up here: harness bottlenecks cleared (97.1% cache, $0.71/4 tasks); test `--review on` or non-zero-floor slice

The 4-task [TB4.1 off-peak streaming pass](docs/experiments/2026-11-25-tb41-offpeak-streaming.md)
(`47e9811`, `append_1000` + `stream=True` + uncapped tokens + omitted `max_tokens`) completed
4/4 cleanly with **0 timeouts**, **0 `usage_unknown`**, **0 infra errors**, **95.5%–98.0% cache
hit**, and **`$0.71` live USD total** (`$1.58 → $0.87`, exact 50% off-peak match vs `$1.4171`
peak):

- `heat-pump-warranty` (4.0 h): `exit=0` (Done), **15/21** tests (`0.0`), 90 turns (`90/90` att=1), `97.3%` cache hit, `$0.06` live.
- `music-harmony` (1.0 h): `exit=0` (Done), **10/13** tests (`0.0`), 91 turns (`91/91` att=1), `98.0%` cache hit, `$0.19` live.
- `bun-sourcemap-leak` (1.5 h): `exit=0` (Done), **29/37** tests (`0.0`), 58 turns (`58/58` att=1), `97.1%` cache hit, `$0.08` live.
- `photonic-waveguide-routing` (0.75 h): `exit=3` (`wallclock` 3,600 s), **12/14** tests (`0.0`), 44 turns (`42/44` att=1), `95.5%` cache hit, `$0.38` live (`7.85 µm` vs `8.00 µm` sep).

1. **Key and balance (live 2026-11-25):** `.env.deepseek` (repo key): **`$0.87 USD`** (moved
   `$1.58 → $0.87` across all 4 off-peak streaming trials). Pi key (`~/.pi/agent/auth.json`,
   `deepseek.key`): **¥157.46 CNY**. `~/.config/jcode/deepseek.env` is a dead key; do not use it.
2. **DeepSeek off-peak schedule (verified `api-docs.deepseek.com`, 2026-11-25):** Peak Mon–Fri
   `01:00–04:00 UTC` (09:00–12:00 UTC+8) and `06:00–10:00 UTC` (14:00–18:00 UTC+8); **50% off all
   other hours** (including 18:00–09:00 UTC+8 and weekends).
3. **Next decision (choose one before spending remaining `$0.87`):**
   - **(a) Wire & test `--review on` in Harbor (~3 lines in `tools/harbor_rof.py`):** on the 3
     voluntary-`Done` near-miss tasks (`heat-pump-warranty` `$0.06`, `music-harmony` `$0.19`,
     `bun-sourcemap-leak` `$0.08`), test whether fresh-context defect review + 1 repair turn
     converts 71%–78% subtest passes into 1.0 binary solves (~`$0.35–$0.50` live off-peak).
   - **(b) Establish a non-zero solve floor** on the two already-preflighted oracle-1.0 tasks
     (`freight-dispatch-shift`, `cargo-flight-dispatch` in
     `~/.local/share/rof-tb/datasets/tb41-next-slice-20261111-full/tasks/`) or run 1–2
     `deepseek-v4-pro` trials to separate flash-model ceiling from harness headroom.
4. **Raw data & frozen runtime:** `target/tb41-offpeak-20261125/` (`run_offpeak.py`,
   `ledger.jsonl`, `offpeak.log`, `jobs/`); runtime `target/stream-calib-runtime-20261125/rof`
   (`47e9811`, SHA-256 `5d67f0cb9e244ecbbaf04e1ef4117e236964c152e49d0d33cd060db3f12e24a3`).

Disk: Docker/containerd storage is bind-mounted from `/mnt/models` (31 GB free) via
`/etc/fstab` with a systemd `RequiresMountsFor` drop-in; `/` has ~5.7 GB free (`target/debug`
cleaned). Build with `CARGO_INCREMENTAL=0`.

## Done 2026-11-25

- **Off-peak streaming + append-only pass (`bdef143`):** 4/4 TB4.1 CPU tasks completed cleanly
  with `0` timeouts, `0` `usage_unknown`, `0` rollbacks, `95.5%–98.0%` prefix cache hit,
  `66/85` subtests passing (3/4 declared Done voluntarily at `$0.06–$0.19` live/run), `$0.71`
  live / `$1.4171` peak (`0/4` binary solves; defaults unchanged)
  ([2026-11-25-tb41-offpeak-streaming](docs/experiments/2026-11-25-tb41-offpeak-streaming.md)).
- **SSE idle-timeout & `usage_unknown` fix (`47e9811`):** removed client-wide `reqwest`
  `.timeout(REQUEST_TIMEOUT)` so SSE streaming uses per-phase/per-chunk idle timeouts (fixing the
  600 s timeout silent double-billing bug), added `Response.usage_unknown` propagation to
  `agent-loop` (`provider-usage-unknown` + cleared `cost_usd`), and wired opt-in Harbor
  `stream: bool = False` → `--stream on`.
- **Uncapped-token plumbing (`bd482ba`) & peak calibration pair (`256bdc2`):** added
  `--budget-tokens 0` (uncapped; omitted from `budget_line()`), `--budget-spend-cents N` / Harbor
  `budget_usd`, `--max-tokens 0|omit` (omitted on wire → DeepSeek 64K default), and raised
  `MAX_TOKENS_CEILING` `32_768 → 393_216` (DeepSeek 384K ceiling). Verified 11 coding-agent
  harnesses (0/11 cap cumulative gross tokens). Free oracle preflights on shortest TB4.1 tasks:
  `photonic-waveguide-routing` (0.75 h) **1.0**, `html-js-filter` (0.75 h) excluded
  (`VerifierTimeoutError`). Paired peak calibration on `photonic-waveguide-routing` confirmed
  `append_1000` lifted cache hit `62.4% → 92.9%` (`99.5%` steady-state, 7.1× lower cache-miss
  spend) and caught the 600 s non-streaming timeout bug
  ([2026-11-25-tb41-uncapped-calib](docs/experiments/2026-11-25-tb41-uncapped-calib.md)).
- **Behavior-preserving cleanup (`1aef3fa`):** removed `crates/trace`, 3 unused Cargo deps
  (`eval/tokio`, `bets/serde_json`, `agent-budget/serde_json`), unwired Bet C (`bets/src/race.rs`),
  test-only `drive_tick` + `EffectGate` + no-op `ContextAssembler`, 0-caller `eval`/`snapshot`/
  `provider`/`tools-std`/`agent-event` APIs, and duplicated `cli.rs`/`eval_cmd.rs`/`rof` setup
  and runner logic (−3,760 net LOC across 86 files; 522 Rust + 41 Python tests green; deleted
  merged local branch `docs-housekeeping`).
- **Pre-benchmark bug fixes & WAL metadata (`5159e17`):** fixed `write("/app/<newdir>/<file>")`
  parent creation via shared `policy::strip_app_alias`, aligned `TreeService::is_repo` with
  `workdir_state`'s `.git/HEAD` check, routed `MultiEdit` through `common::overwrite`, dropped
  lock before `kill_tree().await` in `ProcessTool`, propagated `cancel.child_token()` to agent-tree
  children, persisted optional `raw_stop_reason` and `attempts` on WAL `Assistant` rows, and fixed
  `USAGE`/`EVAL_USAGE`/`decisions.md` drift (526 Rust tests green).
- **Strategic pruning (`0aab2ab`):** retired legacy `rof eval`, `crates/eval/`, and four frozen
  historical Slice A scripts (`triage_report.py`, `envelope_report.py`, `readme_chart.py`,
  `deliverable_ab1.sh`) (−7,682 net LOC; 463 Rust + 41 Python tests green).
- **Harbor adapter reliability (`dd4f8a6` + post-screen JSONL fix):** switched Debian `git` install
  to `--no-install-recommends` (78 → 49 packages, fixing `bun-sourcemap-leak` 360 s setup timeout),
  and replaced `.splitlines()` with `.split("\n")` in `tools/harbor_rof.py` and
  `tools/rof_candidates.py` after Unicode line separators (`\u2028`/`\u2029`) in `music-harmony`
  PDF output silently dropped usage to `null` on 6/12 runs.
- **48-run TB4.1 option screen:** completed 48/48 with 0 infra errors, 0/48 solved
  ([2026-11-25-tb41-option-screen](docs/experiments/2026-11-25-tb41-option-screen.md)).

## Done 2026-11-24

- **Docs housekeeping** (merged 2026-11-24): 49 notes condensed under a
  decision-bearing rule (keep every conclusion, caveat and our own measurement; external
  side detail and finished plan items collapse to pointers). Tracked `.md` words
  104,955 → 74,791 (−29%). `roadmap.md` is now open work only; its history moved to
  `decisions.md`. Superseded/void notes are stubs (full text in git at `ebf720b`;
  `NEXT.md` original in `target/docs-housekeeping-originals/`). Corrections, checked
  against raw reports: envelope arm 10 of 11 token halts (not 5), easy-miss binary
  `af0002b`, `hf-model-inference` 3 of 4 recorded runs solved. From the note's own
  table: slice-a-vs-published has four ≥0.88 misses (adds `sam-cell-seg`). Recount of a
  lost script: reread census 59 of 1,698 (original 61). Originals, drafts, logs and
  checks: `target/docs-housekeeping-originals/hk/` (`STATUS.txt` is the ledger).
  Untouched by choice: `docs/decisions/design*.md` and `docs/architecture/design.md`
  (reviewed briefs).
- [Harness audit](https://claude.ai/artifact/7SUFD4vh2TaUC4DskWZeAG): 109 choices with
  backing labels; inventories in `target/no-evidence-census-20261124/`.
- [No-evidence census](docs/experiments/2026-11-24-no-evidence-census.md): zero model
  calls, 286 logged runs plus a nine-harness source survey.
- `dbf52b6`: opt-in `--exec-output head-tail` and `--rollback-scope errors` (unmeasured on
  benchmark tasks), plus default-on correctness fixes (exec/test descriptions drop
  "allowlisted" under `any`; adapter grades only exits 0/3/8, withholds usage after
  usage-unknown errors, versions trials by binary hash; hysteresis contract text).
- **Benchmark:** TB 4.1.0 CPU (2026-11-03), the only recent cheap option with a
  leaderboard; OpenThoughts-TBLite dropped (2026-02-20, fails the September cutoff).
- **Budget:** under $10 for this campaign.

## Agents

Pi DeepSeek flash children each start a rust-analyzer via pi-lens (0.5–1 GB): keep ≤ 6
concurrent on this 14 GB machine (2 concurrent worked reliably). OpenCode HY4 free failed
every task at 9 concurrent but worked cleanly at 1 concurrent. Gemini Hydrogen and Neon
(Antigravity) both handled codebase audits/refactors well (watch Neon individual quota).

## Repository

- `master` is pushed to `origin/master` through the 2026-11-25 streaming/calibration handoff
  (`bd482ba`, `256bdc2`, `47e9811`, `bdef143` + this handoff). CI's stale-reference guard skips
  `docs/`, `SESSION-NEXT.md`, and `NOTES_FOR_PI.md`. `goose/` is an untracked separate repo.
  Open from housekeeping: parked Slice A roadmap items (drop or keep?) and the unverified
  "20 driver invocations" line in `decisions.md`.
- Run adapter tests with Harbor's Python from `tools/` (`19/19` `test_harbor_rof.py` + `25/25`
  `test_rof_*` = `44/44` green; Rust workspace `provider-core`/`provider-openai`/`agent-loop`/
  `rof` tests + `clippy --tests -- -D warnings` + `cargo fmt --check` green). The pre-commit
  hook requires every `.md` under `docs/` and the local research notes to appear in
  `docs/INDEX.md`.

## Evidence pointers

- [TB4.1 off-peak streaming pass](docs/experiments/2026-11-25-tb41-offpeak-streaming.md) (4
  Harbor runs: `append_1000` + `stream=True` + uncapped tokens; 0/4 binary solves, 66/85 subtests
  passing, 3/4 voluntary `Done`, 95.5%–98.0% cache hit, 0 timeouts, `$0.71` live / `$1.4171` peak).
- [TB4.1 uncapped calibration pair](docs/experiments/2026-11-25-tb41-uncapped-calib.md) (2 peak
  Harbor runs on `photonic-waveguide-routing`: `control_5_2` vs `append_1000`; confirmed prefix
  cache fix `62.4% → 92.9%` and exposed 600 s non-streaming timeout double-billing; `$1.88` live).
- [TB4.1 CPU option screen](docs/experiments/2026-11-25-tb41-option-screen.md) (48 Harbor runs
  at 300k: 0/48 solved across control, `exec_output`, `rollback_scope`, `max_tokens: 32768`;
  `$1.81` live / `$3.6813` peak; mechanisms verified, defaults unchanged).
- [Local harness checks](docs/experiments/2026-11-23-local-harness-checks.md),
  [budget-line A/B](docs/experiments/2026-11-23-budget-line-ab.md) (requests 1/4, legacy
  0/4, did not repeat; flag stays opt-in).
- ROF vs Hermes: both 18/18 on six internal multi-file tasks × 3 reps, mean billed tokens
  7,906.5 vs 40,527.9 (~80% fewer for ROF). Suite `~/.local/share/rof-crossbench/`, usage
  `~/.local/share/rof-cost/mf-billed-by-rep.json`.
- The earlier 8/8 environment-state claim was wrong (archived arm: 2/8 native passes);
  audit `target/tb41-cpu-baseline-20261123/fidelity-audit.json`.
- TB4.1 CPU preflight (`target/tb41-cpu-baseline-20261123/` & `target/tb41-oracle-20261125/`,
  tasks pinned at `4070d6cef56a`): oracle reward 1 on `photonic-waveguide-routing` (0.75 h),
  `music-harmony` (1.0 h), `bun-sourcemap-leak` (1.5 h), `heat-pump-warranty` (4.0 h),
  `ontology-kg-querying` (20.0 h), plus `freight-dispatch-shift` & `cargo-flight-dispatch` in
  `~/.local/share/rof-tb/datasets/tb41-next-slice-20261111-full/tasks/`; `production-planning`,
  `risk-scorer-replay`, `rs-archive-clone` fail in image build; `nextjs-performance` and
  `html-js-filter` ended in `VerifierTimeoutError`.
- Slice A faithful baseline 5/18 at 300k/60/120; 47 of 52 failures halted on tokens.
  Exclude `dna-insert`, `dna-assembly`, `make-mips-interpreter` from validating
  diagnosis-motivated changes (oracle solutions were read).
- Agent tree is opt-in and unwired from Harbor (`rof eval` retired in `0aab2ab`).
  `--rollback-scope errors` is built and screened (0/12 at 300k; stays opt-in); write-plus-exec
  workflow changes remain unbuilt.
