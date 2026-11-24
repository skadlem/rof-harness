# Session handoff · 2026-11-24

## Pick up here: run the TB4.1 option-screen check trial

Plan: [tb41-option-screen](docs/superpowers/plans/2026-11-24-tb41-option-screen.md)
(4 tasks × 4 arms × 3 reps = 48 runs, ~$4 price-book, hard stop $8; arms: control,
`exec_output: "head-tail"`, `rollback_scope: "errors"`, `max_tokens: 32768`).

1. **Approval:** the owner approved only the one `heat-pump-warranty` control trial
   (adapter check), not the 48 runs. Ask again before the 48.
2. **Key and balance:** use the key from `~/.pi/agent/auth.json` (`deepseek.key`; the
   budget-line A/B used it, `target/budget-line-ab-20261123/run.py:46`) or the repo's
   git-ignored `.env.deepseek`. Live balances 2026-11-24: Pi key ¥174.08 CNY, repo key
   $5.36 USD. `~/.config/jcode/deepseek.env` is a dead key; do not use it. Read the
   balance (`GET https://api.deepseek.com/user/balance`) before and after.
3. **Runtime:** built from clean worktree `target/option-screen-build` (detached at
   `ebf720b`, no `-dirty`): `target/release/rof`, SHA-256
   `55e3383a72d166ee7161d1f21ad759cd3f5b4c7ddf4c9d17c1505ea11247ae95`. The adapter's
   `runtime-bundle-<sha>` version is computed at trial time (`tools/harbor_rof.py:95`).
4. **Harbor:** same setup as before, the `harbor-rootlogfix` overlay plus `PYTHONPATH`
   (`docs/current-benchmark-pilot.md`, "Oracle gate"); working `harbor run -c` caller:
   `target/tb41-cpu-baseline-20261123/oracle.py`; task trees in `source/tasks/`.
5. **Check trial:** confirm the version string, exit-code grading (0/3/8), the reward
   file and per-request usage. It counts toward the $8 cap.
6. **Write-up (after the 48):** raw jobs in `target/tb41-option-screen-<date>/`, a result
   note in `docs/experiments/` + `docs/INDEX.md` line, arm-table status in
   `docs/research-next-steps.md`.

Disk: Docker/containerd storage is bind-mounted from `/mnt/models` (35 GB free) via
`/etc/fstab` with a systemd `RequiresMountsFor` drop-in; `/` has ~9 GB free. Build with
`CARGO_INCREMENTAL=0`.

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

- `master` and `knowledge-vault-trial` are both at `f519145` (pushed; CI green). master now
  carries the whole dev line, merged with its README rewrite. CI's stale-reference guard
  skips `docs/`, `SESSION-NEXT.md`, `NOTES_FOR_PI.md` and `tools/envelope_report.py`.
  Local branch `docs-housekeeping` is merged and can be deleted. `goose/` is an untracked
  separate repo. Open from housekeeping: parked Slice A roadmap items (drop or keep?) and
  the unverified "20 driver invocations" line in `decisions.md`.
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
