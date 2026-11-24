# TB4.1 CPU option screen: three single-field arms against one control, about $4 at list price

**Status: proposed, unstarted.** It needs the owner's spend approval and a live
balance read. Nothing below is a result.

## Question

Do any of the three changes built on 2026-11-24 change native solves on
untouched TB4.1.0 CPU tasks? Each arm changes one Harbor option against a
fixed control:

| Arm | Harbor option change | Census motivation |
|---|---|---|
| control | none | — |
| output | `exec_output: "head-tail"` | the head cut dropped 21% of exit markers (16/178 capped results); 28 timeouts returned no output |
| rollback | `rollback_scope: "errors"` | 11.8% of batches rolled back under `any`; 18/1,825 rollbacks destroyed a successful write |
| response | `max_tokens: 32768` | 1.5% of replies (166/10,771) spent all 8,000 tokens on reasoning |

Source: [no-evidence census](../../experiments/2026-11-24-no-evidence-census.md).
The census counts are from the legacy eval path, partly with the allowlist on,
so they motivate the arms. They do not predict effect sizes.

## Tasks

The four tasks with oracle reward 1 in the
[4.1 CPU preflight](../../../SESSION-NEXT.md): `bun-sourcemap-leak`,
`heat-pump-warranty`, `music-harmony`, `ontology-kg-querying`. These are pinned
at `4070d6cef56a` with digests in `target/tb41-cpu-baseline-20261123/manifest.json`.
They come from the `source/tasks/` trees, not the registry exports.
`nextjs-performance` is excluded because its retry also ended in
`VerifierTimeoutError`. The other three tasks fail during the image build.
Selection used public metadata before any outcomes (**chosen**: convenience
slice, no claim of representativeness).

## Fixed control

- Model and endpoint: DeepSeek `deepseek-flash`, with the same endpoint and reasoning effort in all arms.
- Limits: 300k gross tokens, 60 steps, 120 actions and `max_tokens: 8000`.
- Settings: `--exec-policy any`, observation/thinking keep 5/2, steering off.
- **Chosen**: the 300k cap keeps the spend under $10. At 1.2M (the pilot
  control), 48 runs cost about $16. Slice A halted on tokens in 47 of 52
  failures at 300k, so this screen measures the arms under a binding cap.
  The arms might perform differently with a larger cap.
- One runtime binary built from a clean checkout at or after `dbf52b6`.
  Both the adapter's `runtime-bundle-<sha>` version and the SHA-256 are
  recorded. `goose/` is untracked in the main checkout. Build from a clean
  worktree so the revision has no `-dirty` suffix.

## Design

- 4 tasks × 4 arms × 3 repetitions = 48 runs.
- Each run is paired with the control at the same task and repetition.
- Rotate the arm order across repetitions to spread provider drift.
- Run trials one at a time. The concurrency limit is disk, not spend.
- **Cost (inferred):** the budget-line A/B averaged $0.0835 per 300k run at
  price-book rates, so 48 × $0.0835 ≈ $4.0. The provider balance moved less
  than the price book over that run.
- **Stop rule:** halt at $8 price-book spend or on two consecutive
  infrastructure errors (exit outside 0/3/8) on the same task.
- **Power (inferred):** each arm has 12 paired runs. Only a swing of several
  solves is distinguishable from the variance seen in Slice A, so this is a
  screen, not a promotion test. A promising arm goes to untouched holdouts
  before any default change, per `AGENTS.md`.

## Measures

For each run, record:

- Native reward.
- Halt reason.
- Gross tokens, with `usage_unknown` rows excluded and counted.
- Steps.
- Wall time.
- Exit code.

For each arm, record:

- Solves gained and solves lost against the paired control.
- Tokens per solve, with the 1.3× guardrail. The guardrail is undefined if the control solves nothing.

Mechanism checks confirm that each arm did what it was built to do:

- **output:** count `[exit N]` first lines and partial-output timeouts.
- **rollback:** count completed-call failures that kept edits. Confirm that no
  rollback notice appeared without a revert.
- **response:** count reasoning-only truncations and ladder retries.

## Prerequisites

1. **Disk (done, 2026-11-24).**
   - Docker and containerd storage moved to the `models` partition, which has
     35 GB free.
   - `/` has 12 GB free.
2. **Spend.** The owner approves this scope and the $8 cap. Read the live
   provider balance before the run and after it.
3. **Adapter check.**
   - Run one control trial on `heat-pump-warranty`.
   - Confirm the version string, the exit-code grading, the reward file and
     the per-request usage.
   - Count this trial toward the cap. It is not one of the 48 runs.

## Output

- Raw jobs go in `target/tb41-option-screen-<date>/`.
- The result goes in a note in `docs/experiments/` whose title states the
  outcome. Add its line to `docs/INDEX.md`.
- Update the arm table status in `docs/research-next-steps.md`.
