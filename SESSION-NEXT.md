# Session handoff · 2026-11-25

## Pick up here: owner approval for the 48-run TB4.1 option screen

Plan: [tb41-option-screen](docs/superpowers/plans/2026-11-24-tb41-option-screen.md)
(4 tasks × 4 arms × 3 reps = 48 runs, ~$4 price-book, hard stop $8; arms: control,
`exec_output: "head-tail"`, `rollback_scope: "errors"`, `max_tokens: 32768`).

1. **Approval:** the `heat-pump-warranty` adapter-check control trial is **done** (see below);
   the 48 runs still need owner spend approval before launching.
2. **Key and balance:** `.env.deepseek` (repo key) live balance after the check trial:
   **$5.33 USD** (moved from $5.36 USD; $0.03 billed vs $0.0697 peak price book). Pi key
   (`~/.pi/agent/auth.json`, `deepseek.key`) live balance: **¥157.46 CNY**.
   `~/.config/jcode/deepseek.env` is a dead key; do not use it. Read the balance
   (`GET https://api.deepseek.com/user/balance`) before and after.
3. **Runtime:** built from clean worktree `target/option-screen-build` (detached at
   `5159e17`, no `-dirty`), bundled in `target/option-screen-runtime/`: `rof` SHA-256
   `5947b121322700c070c72c1c5984585d1f588fc345153355c2231aa5375cddc6`, Harbor version
   `runtime-bundle-5947b121322700c0` (confirmed in trial `result.json`).
4. **Harbor:** same setup as before, the `harbor-rootlogfix` overlay plus `PYTHONPATH`
   (`harbor-rootlogfix:<repo>`, `docs/current-benchmark-pilot.md`, "Oracle gate");
   working `harbor run -c` config:
   `target/tb41-option-screen-20261125/check-heat-pump-warranty-control.json`; task trees in
   `target/tb41-cpu-baseline-20261123/source/tasks/`.
5. **Check trial (done 2026-11-25):** `heat-pump-warranty__UH3UuFd` in
   `target/tb41-option-screen-20261125/jobs/check-heat-pump-warranty-control/` —
   `runtime-bundle-5947b121322700c0`, exit code `3` (graded, 0 errors), `reward=0.0`
   (`verifier/reward.txt`), `RunEnd=Failed("tokens")`, 32 turns, `299,513` input /
   `88,576` cache / `4,875` output (`1,871` reasoning), `$0.0697` peak (`$0.03` live),
   WAL recorded `raw_stop_reason="tool_calls"` and `attempts=1` on all 32 assistant rows.
   Counts toward the $8 cap.
6. **Write-up (after the 48):** raw jobs in `target/tb41-option-screen-<date>/`, a result
   note in `docs/experiments/` + `docs/INDEX.md` line, arm-table status in
   `docs/research-next-steps.md`.

Disk: Docker/containerd storage is bind-mounted from `/mnt/models` (35 GB free) via
`/etc/fstab` with a systemd `RequiresMountsFor` drop-in; `/` has ~9 GB free. Build with
`CARGO_INCREMENTAL=0`.

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
- **Adapter-check trial:** verified `heat-pump-warranty` control trial on clean `5159e17` bundle
  (details above).

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
concurrent on this 14 GB machine. OpenCode HY4 free failed every task at 9 concurrent.
Gemini Hydrogen (Antigravity) did the living-docs review; Neon unused.

## Repository

- `master` has local commits `1aef3fa` + `5159e17` ahead of `origin/master` (`1136136`, not yet
  pushed). CI's stale-reference guard skips `docs/`, `SESSION-NEXT.md`, `NOTES_FOR_PI.md` and
  `tools/envelope_report.py`. Local branch `docs-housekeeping` was deleted. `goose/` is an
  untracked separate repo. Open from housekeeping: parked Slice A roadmap items (drop or keep?)
  and the unverified "20 driver invocations" line in `decisions.md`.
- Run adapter tests with Harbor's Python from `tools/`. The pre-commit hook requires every
  `.md` under `docs/` and the local research notes to appear in `docs/INDEX.md`.

## Evidence pointers

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
  `music-harmony`, `ontology-kg-querying`; `production-planning`, `risk-scorer-replay`,
  `rs-archive-clone` fail in image build; `nextjs-performance` retry ended in
  `VerifierTimeoutError`. No paid 4.1 baseline ran.
- Slice A faithful baseline 5/18 at 300k/60/120; 47 of 52 failures halted on tokens.
  Exclude `dna-insert`, `dna-assembly`, `make-mips-interpreter` from validating
  diagnosis-motivated changes (oracle solutions were read).
- Agent tree is opt-in and unwired from `rof eval`. Rollback-policy and write-plus-exec
  workflow changes remain unbuilt.
