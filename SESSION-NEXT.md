# Session handoff · 2026-11-25

## Pick up here: run Step 2 uncapped-token + append-only calibration on 4 oracle-verified TB4.1 tasks

The 48-run [TB4.1 CPU option screen](docs/experiments/2026-11-25-tb41-option-screen.md)
completed with **0 infra errors** and **0/48 solved** across all four arms at 300k gross tokens
(47/48 halted on tokens; 1/48 halted on 32k reasoning truncation on `r3-music-harmony-response`).
All three mechanisms fired cleanly in WALs; all defaults stay unchanged.

1. **Key and balance (live 2026-11-25):** `.env.deepseek` (repo key): **$3.51 USD** (moved from
   $5.36 → $5.33 on check trial → $5.32 → $3.51 across all 48 runs; **$1.81 live** vs **$3.6813**
   peak price book). Pi key (`~/.pi/agent/auth.json`, `deepseek.key`): **¥157.46 CNY**.
   `~/.config/jcode/deepseek.env` is a dead key; do not use it.
2. **Step 1 plumbing landed (uncommitted in working tree; defaults unchanged):**
   - `--budget-tokens 0` / Harbor `budget_tokens=0` uncaps cumulative tokens (`budget_line()` omits
     the token segment cleanly in both `legacy` and `requests` modes).
   - `--budget-spend-cents N` / Harbor `budget_usd` configures the USD spend cap (`0` disables it;
     omitted keeps the `$4.00` preset).
   - `--max-tokens 0|omit` / Harbor `max_tokens=0|None` omits `"max_tokens"` on the wire (letting
     DeepSeek use its 64K thinking default); `MAX_TOKENS_CEILING` raised from `32_768` →
     **`393_216`** (DeepSeek 384K wire ceiling; omitted truncation jumps `0 → 131_072 → 262_144 →
     393_216`).
   - Verified green: `provider-openai` + `agent-loop` + `rof` tests, `clippy --tests -- -D warnings`,
     `cargo fmt --check`, and 44/44 Python tests.
3. **Free oracle preflights (`4070d6cef56a`, `target/tb41-oracle-20261125/`):**
   - `photonic-waveguide-routing` (0.75 h): oracle **1.0** (0 exceptions, 7m 29s cold build).
   - `html-js-filter` (0.75 h): excluded (`VerifierTimeoutError` in `_separate_verifier_env`, 10m 39s).
4. **Next decision (Step 2 calibration, pending owner go-ahead):** rebuild frozen runtime bundle,
   then run a **4-task × 2-arm × 1-rep** (8 trials, hard-capped at `$1.00`/trial peak / `$1.50`
   total live USD) calibration on `photonic-waveguide-routing` (0.75 h), `music-harmony` (1.0 h),
   `bun-sourcemap-leak` (1.5 h), and `heat-pump-warranty` (4.0 h) with `budget_tokens=0`,
   `budget_usd=1.0`, `budget_steps=200`, `budget_actions=400`, `max_tokens=0` (omitted → 64K server
   default): **Arm A** `5/2` retention vs **Arm B** `1000/1000` append-only prefix retention (tests
   both non-zero solve floor and DeepSeek prefix cache-hit rate vs the measured 42.0% under `5/2`).
5. **Raw data:** `target/tb41-option-screen-20261125/` (`ledger.jsonl`, `summary-48.json`, `jobs/`),
   `target/tb41-oracle-20261125/`.

Disk: Docker/containerd storage is bind-mounted from `/mnt/models` (33 GB free) via
`/etc/fstab` with a systemd `RequiresMountsFor` drop-in; `/` has ~6.8 GB free (`target/debug`
cleaned). Build with `CARGO_INCREMENTAL=0`.

## Done 2026-11-25

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

- `master` is at `d489a38` (pushed to `origin/master`; GitHub Actions CI ✓ green across
  `0aab2ab`, `dd4f8a6`, `d489a38`). CI's stale-reference guard skips `docs/`,
  `SESSION-NEXT.md`, and `NOTES_FOR_PI.md`. Local branch `docs-housekeeping` was deleted.
  `goose/` is an untracked separate repo. Open from housekeeping: parked Slice A roadmap
  items (drop or keep?) and the unverified "20 driver invocations" line in `decisions.md`.
- Run adapter tests with Harbor's Python from `tools/` (`18/18` green + `25/25` `test_rof_*`
  green; Rust workspace `463` passed, `0` failed, `2` ignored live tests, 13 crates). The
  pre-commit hook requires every `.md` under `docs/` and the local research notes to appear
  in `docs/INDEX.md`.

## Evidence pointers

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
- TB4.1 CPU preflight (`target/tb41-cpu-baseline-20261123/`, tasks pinned at
  `4070d6cef56a`): oracle reward 1 on `bun-sourcemap-leak`, `heat-pump-warranty`,
  `music-harmony`, `ontology-kg-querying` (all 4 screened 0/12 per arm at 300k above);
  `production-planning`, `risk-scorer-replay`, `rs-archive-clone` fail in image build;
  `nextjs-performance` retry ended in `VerifierTimeoutError`.
- Slice A faithful baseline 5/18 at 300k/60/120; 47 of 52 failures halted on tokens.
  Exclude `dna-insert`, `dna-assembly`, `make-mips-interpreter` from validating
  diagnosis-motivated changes (oracle solutions were read).
- Agent tree is opt-in and unwired from Harbor (`rof eval` retired in `0aab2ab`).
  `--rollback-scope errors` is built and screened (0/12 at 300k; stays opt-in); write-plus-exec
  workflow changes remain unbuilt.
