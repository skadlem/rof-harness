# Session handoff · 2026-11-25

## Pick up here: choose next TB4.1 non-zero-floor slice or budget

The 48-run [TB4.1 CPU option screen](docs/experiments/2026-11-25-tb41-option-screen.md)
completed with **0 infra errors** and **0/48 solved** across all four arms at 300k gross tokens
(47/48 halted on tokens; 1/48 halted on 32k reasoning truncation on `r3-music-harmony-response`).
All three mechanisms fired cleanly in WALs; all defaults stay unchanged.

1. **Key and balance (live 2026-11-25):** `.env.deepseek` (repo key): **$3.51 USD** (moved from
   $5.36 → $5.33 on check trial → $5.32 → $3.51 across all 48 runs; **$1.81 live** vs **$3.6813**
   peak price book). Pi key (`~/.pi/agent/auth.json`, `deepseek.key`): **¥157.46 CNY**.
   `~/.config/jcode/deepseek.env` is a dead key; do not use it.
2. **Runtime:** frozen bundle in `target/option-screen-runtime/` built from detached `5159e17`:
   `rof` SHA-256 `5947b121322700c070c72c1c5984585d1f588fc345153355c2231aa5375cddc6`, Harbor version
   `runtime-bundle-5947b121322700c0` (confirmed on all 48/48 runs).
3. **Next decision:** before spending more on TB4.1 option screens, either (a) preflight easier
   TB4.1.0 CPU tasks where `deepseek-flash` has a non-zero control solve rate at 300k, or
   (b) test the 1.2M pilot token budget on a smaller subset, and decide whether `MAX_TOKENS_CEILING`
   (`crates/provider-openai/src/retry.rs:36`) should scale above 32,768 when `--max-tokens 32768`
   is set.
4. **Raw data:** `target/tb41-option-screen-20261125/` (`ledger.jsonl`, `summary-48.json`, `jobs/`).

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
concurrent on this 14 GB machine. OpenCode HY4 free failed every task at 9 concurrent.
Gemini Hydrogen (Antigravity) did the living-docs review; Neon unused.

## Repository

- `master` is pushed and up to date with `origin/master`. CI's stale-reference guard skips
  `docs/`, `SESSION-NEXT.md`, and `NOTES_FOR_PI.md`. Local branch `docs-housekeeping` was
  deleted. `goose/` is an untracked separate repo. Open from housekeeping: parked Slice A
  roadmap items (drop or keep?) and the unverified "20 driver invocations" line in `decisions.md`.
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
