# Session handoff · 2026-11-24

## 2026-11-24 update (uncommitted)

- Harness audit: https://claude.ai/artifact/7SUFD4vh2TaUC4DskWZeAG (109 choices
  with backing labels; full inventories in `target/no-evidence-census-20261124/`).
- [No-evidence census](docs/experiments/2026-11-24-no-evidence-census.md): zero
  model calls over 286 logged runs plus a nine-harness source survey.
- Built, opt-in, unmeasured on tasks: `--exec-output head-tail` (Harbor
  `exec_output`), `--rollback-scope errors` (Harbor `rollback_scope`). Prepared
  arm `max_tokens: 32768` via the existing option. Default-on correctness fixes:
  exec/test descriptions drop "allowlisted" under `--exec-policy any`; adapter
  grades only exits 0/3/8, withholds usage after usage-unknown errors, and
  versions trials by the binary hash. Contract text under hysteresis fixed.
- Doc drift fixed in architecture, threat-model and harness-experiments notes.
- Benchmark choice: research found TB 4.1.0 CPU (2026-11-03) the only recent,
  cheap option with a leaderboard; OpenThoughts-TBLite dates to 2026-02-20 and
  fails the owner's September cutoff. Owner budget for a campaign: under $10
  (300k-cap run is about $0.083 at list price).
- Disk is at 99%: `target/debug/incremental` was cleared; build with
  `CARGO_INCREMENTAL=0`.

## Pick up here (2026-11-23)

The owner wants credible numbers quickly that show how much ROF's context,
tools and loop can close the Flash-to-frontier gap. The recommended next
experiment is a **frozen 12-task OpenThoughts-TBLite slice**. That recommendation
has not been adopted or started. Keep Terminal-Bench 4.1 CPU for later validation.

1. Pin the TBLite release and select 12 tasks before seeing outcomes. Include a
   range of difficulty and expected control solves. Choose light CPU images and
   preflight four tasks with native oracles first. Record cold setup time, verifier
   time and disk use separately.
2. For an approved paid comparison, use DeepSeek Flash, a starting budget of
   100k gross tokens and 20 steps per run, and stock ROF versus one context
   change. Freeze the same model, endpoint, reasoning effort, and caps for both
   arms. Report native solves, gained and lost solves, tokens per solve, and
   wall time. The promotion guardrail remains 1.3× tokens per solve.
3. The previous spend approval covered the completed budget-line A/B. Confirm
   the scope and cap of a new paid run and read the live balance. The separate
   lookup campaign remains stopped.

For delegation when needed in this session, the owner requested OpenCode HY4
Flash Free and Pi DeepSeek Flash at the highest available reasoning effort.
The delegation catalog exposed both harnesses. The exact model entries and
effort options were not confirmed. Check before delegating.

## Repository and published work

- Active checkout is `knowledge-vault-trial` at `97075cf`, five commits ahead
  of its local upstream reference `6eaefaa`. Local edits include the README,
  this handoff, the research handoff and evidence notes/index. Check
  `git status --short --branch` before editing or committing.
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
