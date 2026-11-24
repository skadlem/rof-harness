# TB4.1 CPU option screen: three single-field arms against one control, about $4 at list price

**Status: proposed, unstarted.** It needs the owner's spend approval and a live
balance read; nothing below is a result.

## Question

Do the three changes built on 2026-11-24 change native solves on untouched
TB4.1.0 CPU tasks? Each arm changes one Harbor option against a fixed control:

| Arm | Harbor option change | Census motivation |
|---|---|---|
| control | none | — |
| output | `exec_output: "head-tail"` | the head cut dropped 21% of exit markers (16/178 capped results); 28 timeouts returned no output |
| rollback | `rollback_scope: "errors"` | 11.8% of batches rolled back under `any`; 18/1,825 rollbacks destroyed a successful write |
| response | `max_tokens: 32768` | 1.5% of replies (166/10,771) spent all 8,000 tokens on reasoning |

The counts come from the
[no-evidence census](../../experiments/2026-11-24-no-evidence-census.md), on the
legacy eval path, partly with the allowlist on: they motivate the arms and do
not predict effect sizes.

## Tasks

The four tasks with oracle reward 1 in the
[4.1 CPU preflight](../../../SESSION-NEXT.md): `bun-sourcemap-leak`,
`heat-pump-warranty`, `music-harmony`, `ontology-kg-querying`, pinned at
`4070d6cef56a` with digests in `target/tb41-cpu-baseline-20261123/manifest.json`,
from the `source/tasks/` trees rather than the registry exports.
`nextjs-performance` is excluded because its retry also ended in
`VerifierTimeoutError`; the other three tasks fail during the image build.
Selection used public metadata before any outcomes (**chosen**: convenience
slice, no claim of representativeness).

## Fixed control

DeepSeek `deepseek-flash`, same endpoint and reasoning effort in all arms; 300k
gross tokens, 60 steps, 120 actions, `max_tokens: 8000`; `--exec-policy any`,
observation/thinking keep 5/2, steering off.

- **Chosen**: the 300k cap keeps spend under $10; 48 runs at 1.2M (the pilot
  control) cost about $16. Slice A halted on tokens in 47 of 52 failures at
  300k, so this screen measures the arms under a binding cap, where they might
  perform differently.
- One binary from a clean checkout at or after `dbf52b6`, recording the
  adapter's `runtime-bundle-<sha>` version and the SHA-256. `goose/` is
  untracked in the main checkout; build from a clean worktree so the revision has
  no `-dirty` suffix.

## Design

- 4 tasks × 4 arms × 3 repetitions = 48 runs, each paired with the control at the
  same task and repetition. Rotate arm order across repetitions to spread
  provider drift; run one trial at a time, since disk rather than spend is the
  limit.
- **Cost (inferred):** the budget-line A/B averaged $0.0835 per 300k run at
  price-book rates, so 48 × $0.0835 ≈ $4.0; the provider balance moved less than
  the price book over that run.
- **Stop rule:** halt at $8 price-book spend, or on two consecutive
  infrastructure errors (exit outside 0/3/8) on the same task.
- **Power (inferred):** 12 paired runs per arm; only a swing of several solves is
  distinguishable from Slice A's variance, so this is a screen, not a promotion
  test. A promising arm goes to untouched holdouts before any default change, per
  `AGENTS.md`.

## Measures

Per run: native reward, halt reason, gross tokens (`usage_unknown` rows excluded
and counted), steps, wall time, exit code. Per arm: solves gained and lost
against the paired control, and tokens per solve against the 1.3× guardrail,
undefined if the control solves nothing.

Mechanism checks:

- **output:** `[exit N]` first lines, partial-output timeouts.
- **rollback:** completed-call failures that kept edits; no rollback notice without a revert.
- **response:** reasoning-only truncations, ladder retries.

## Prerequisites

1. **Disk (done, 2026-11-24).** Docker and containerd storage on the `models`
   partition (35 GB free); `/` has 12 GB free.
2. **Spend.** Owner approval of this scope and the $8 cap; read the live
   provider balance before and after.
3. **Adapter check.** One `heat-pump-warranty` control trial, counted toward the
   cap and not one of the 48: confirm the version string, exit-code grading,
   reward file and per-request usage.

## Output

- Raw jobs in `target/tb41-option-screen-<date>/`; the result in a
  `docs/experiments/` note whose title states the outcome, with its line in
  `docs/INDEX.md`.
- Update the arm table status in `docs/research-next-steps.md`.
