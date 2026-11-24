# Session handoff · 2026-11-24

## Pick up here (2026-11-24): run the TB4.1 option screen

The plan is [tb41-option-screen](docs/superpowers/plans/2026-11-24-tb41-option-screen.md).
It covers 4 tasks × 4 arms × 3 reps, 48 runs, about $4 at price-book rates with
a hard stop at $8. The arms are control, `exec_output: "head-tail"`,
`rollback_scope: "errors"` and `max_tokens: 32768`.

1. **Spend approval is still pending.** The owner chose to start the run in a
   new session. Before any paid call, confirm the scope and the $8 cap, and
   read the live provider balance.
2. **Build the runtime from a clean worktree** at `a43c35e` or later. `goose/`
   is untracked in the main checkout and would mark the build `-dirty`. Record
   the binary's SHA-256 and the adapter's `runtime-bundle-<sha>` version.
3. **Use the same Harbor setup as before.** Earlier runs used the
   `harbor-rootlogfix` overlay and `PYTHONPATH`, not vanilla Harbor (see
   `docs/current-benchmark-pilot.md`, "Oracle gate"). For a working
   `harbor run -c <config>` caller, see
   `target/tb41-cpu-baseline-20261123/oracle.py`. Use the task trees in
   `source/tasks/`.
4. **Run one control trial on `heat-pump-warranty` first.** Check:
   - the version string,
   - the exit-code grading (0/3/8),
   - the reward file,
   - per-request usage.

   Then run the 48 runs, one trial at a time, rotating arm order across reps.
5. **Write up the result.** Raw jobs go in `target/tb41-option-screen-<date>/`.
   Write a result note in `docs/experiments/` and add it to `docs/INDEX.md`.
   Update the arm table in `docs/research-next-steps.md`.

**Disk (fixed 2026-11-24):**
- Docker and containerd storage now live on the `models` partition
  (`/mnt/models`, 35 GB free).
- They are bind-mounted onto `/var/lib/docker` and `/var/lib/containerd` via
  `/etc/fstab`. A systemd `RequiresMountsFor` drop-in keeps the daemons from
  starting before the mounts are up.
- `/` has 12 GB free.
- Build with `CARGO_INCREMENTAL=0` to keep `target/` small.

## Done 2026-11-24

- [Harness audit](https://claude.ai/artifact/7SUFD4vh2TaUC4DskWZeAG) covers 109
  choices, each with a backing label. The full inventories are in
  `target/no-evidence-census-20261124/`.
- The [no-evidence census](docs/experiments/2026-11-24-no-evidence-census.md)
  used zero model calls. It covers 286 logged runs plus a source survey of nine
  harnesses.
- Commit `dbf52b6` adds two opt-in options, neither measured on benchmark tasks:
  - `--exec-output head-tail`
  - `--rollback-scope errors`

  It also turns on these correctness fixes by default:
  - Exec and test descriptions drop "allowlisted" under `any`.
  - The adapter grades only exits 0, 3 and 8.
  - The adapter withholds usage after usage-unknown errors.
  - The adapter versions trials by the binary hash.
  - The contract text under hysteresis is fixed.
- Commit `a43c35e` adds the docs, the census, the 2026-11-23 notes and the
  README.
- **Benchmark choice: TB 4.1.0 CPU (2026-11-03).** It is the only recent, cheap
  option with a leaderboard. The 2026-11-23 OpenThoughts-TBLite recommendation
  is dropped, because TBLite dates to 2026-02-20 and fails the owner's
  September cutoff.
- **Owner budget:** under $10 for this campaign.

For delegation, the owner allows OpenCode HY4 Flash Free, Pi DeepSeek Flash and
Haiku agents through T3.

## Repository and published work

- Active checkout is `knowledge-vault-trial`, eight commits ahead of its local
  upstream reference `6eaefaa`. The only untracked entry is `goose/`, a separate
  repository. Check `git status --short --branch` before editing or committing.
- Verified remote `master` at `0fb593f`. Its README leads with context,
  tooling and the loop, with container execution described as evaluation
  infrastructure. The docs-only commits are `a540b0b` and `0fb593f`.
  The detached master docs checkout is `target/readme-master-20261123/`.
  The development branch was not merged into master.
- The owner prefers a concise README with measured claims and scoped results.

## Results with evidence

- [Local harness checks](docs/experiments/2026-11-23-local-harness-checks.md).
  Over 40 scripted 8 KiB reads, retaining five observations cut cumulative
  request bytes 72% and final request bytes 83%. Archived stdout recovered
  1 MiB byte-for-byte from a 159-byte collapsed result. Rollback matched all
  file checks in 16/16 injected failures. These are local mechanism measurements.
  Runners and raw results are in `target/context-retention-bench-20261123/`
  and `target/harness-local-checks/`.
- [Budget-line A/B](docs/experiments/2026-11-23-budget-line-ab.md).
  Eight Slice A rows. Requests 1/4, legacy 0/4. The pass did not repeat.
  All rows halted on tokens and the relative efficiency guardrail was undefined.
  Keep the flag opt-in. Harness price total $0.667861 and the account balance
  moved from ¥183.19 to ¥179.58. Raw runs in `target/budget-line-ab-20261123/`.
- Earlier ROF versus Hermes. Regraded copied workspaces for all 36 historical
  runs with the original checks. Both passed 18/18 on six internal multi-file
  tasks with three repeats. Same Atria-Dawn-Preview model and verifiers, with
  harness-specific settings. Mean billed tokens 7,906.5 vs 40,527.9, about 80%
  fewer for ROF. Suite and workspaces in `~/.local/share/rof-crossbench/`.
  Usage in `~/.local/share/rof-cost/mf-billed-by-rep.json`.
- The original 8/8 environment-state claim was wrong. The exact archived arm had
  2/8 native passes. Removed the claim from the README. The audit is
  `target/tb41-cpu-baseline-20261123/fidelity-audit.json`.

## Terminal-Bench 4.1 CPU preflight

Frozen inputs and logs are under `target/tb41-cpu-baseline-20261123/`. The
eight selected task trees match the v4.1.0 source at `4070d6cef56a`. Use
`source/tasks/`. The registry exports in `tasks/` differ from the tag.

- **Oracle reward 1:** `bun-sourcemap-leak`, `heat-pump-warranty`,
  `music-harmony`, `ontology-kg-querying` (retry).
- **No valid reward:** `production-planning`, `risk-scorer-replay`,
  `rs-archive-clone`. Harbor reports verifier timeouts, but their retry
  tracebacks fail in Docker Compose image build before the grader.
- **In progress at handoff:** `nextjs-performance-retry-1`. Check the trial and
  job result under `jobs/nextjs-performance-retry-1/`. The first attempt had
  no native reward. The original production job's running counter is stale
  from the disk-full abort.
- The original runner exhausted disk. Recovered space by clearing rebuildable
  local Cargo debug artifacts and selected BuildKit records from completed
  preflight jobs. Around 3.3 GiB remained at handoff. Preserve `target/` evidence
  and unrelated Docker state. `retry-oracles.py` writes fresh trial names and
  checks disk before each attempt. No paid 4.1 baseline ran.

## References and constraints

- [Documentation index](docs/INDEX.md) and [research handoff](docs/research-next-steps.md)
  track current experiments, earlier decisions and deferred work.
- Slice A faithful baseline was 5/18 at 300k gross tokens, 60 steps and 120
  actions. The trace audit found 47/52 failures halted on tokens. Exclude
  `dna-insert`, `dna-assembly` and `make-mips-interpreter` from validation of
  changes motivated by the diagnosis because their oracle solutions were read.
- Agent tree is opt-in and unwired from `rof eval`. Rollback-policy changes
  and write-plus-exec workflow changes were discussed and remain unbuilt.
- Run adapter tests with Harbor's Python from `tools/`. The local commit
  hook requires every documentation note to appear in `docs/INDEX.md`.
