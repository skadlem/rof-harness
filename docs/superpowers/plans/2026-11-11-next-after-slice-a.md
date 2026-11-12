# Next after the Slice A session (2026-11-11)

**Cost convention (corrected 2026-11-12).** The `dollars` field in every report is the price-book **PEAK upper bound** (`crates/provider-openai/src/pricing.rs`: DeepSeek bills off-peak at exactly half). Sums below are therefore upper bounds, not the bill: the owner's live balance shows **$1.56 remaining** of $3.12, i.e. roughly half the reported sum, which is what off-peak billing predicts. Quote the balance, not the sum.


State at hand-off: **5/18 on the faithful Slice A slice**, **$1.56 left of the $3.12 balance** (owner's live balance, 2026-11-12). Report `dollars` sums are PEAK upper bounds and overstate the bill by about 2x — quote the balance, never the sum. Frontier-expected on the same 18 tasks is 12.08 (67.1%), a 39-point
gap that is **not attributable** because the frontier column is frontier-class models and
ours is flash-class.

## Status (2026-11-12): P1–P4 closed

| Item | Outcome | Record |
|---|---|---|
| P1 root-owned artefacts | Fixed in `dc7164e` (`crates/snapshot/src/git.rs`, `crates/agent-loop/src/run.rs`). | commit |
| P2 three published-easy misses | Harness-caused; all three never wrote the named deliverable (`vm.js`, `primers.fasta` ×2) and died on the token cap. Allowlist denials were real (22) but removing them flipped no test. | [three-misses](../../experiments/2026-11-11-three-misses.md), [allowlist-and-named-deliverable](../../experiments/2026-11-11-allowlist-and-named-deliverable.md) |
| P3 `sam-cell-seg` | Not a solve: 8/9 then 6/9 at 1M. Same partial-credit ceiling. Slice A stays 5/18. | [envelope arm](../../experiments/2026-11-11-slice-a-envelope-arm.md) |
| Zero-write gate (added) | Killed: on `hf-model-inference` (4/5 solved) every run wrote by step 3, including the failure. | [hf-within-task](../../experiments/2026-11-11-hf-within-task.md) |
| P4 rival rows | Option 2 taken (2026-11-12): `slice-a-vs-published` stands as unmatched external context; internal n=2 is not called a comparison. No rival runs. | below |

Surviving lead: the **named-deliverable predicate** (no banked run that lacked its named
deliverable at step 15 solved, 0/13; a step-15 gate would have stopped 0 of 2 solves).
Unmeasured as a treatment — it needs a two-arm, token-matched forcing test on a fresh
development task before it is enabled.

Balance: $1.56 was read before the two `--exec-policy any` runs, so the live balance is
somewhat lower. Re-read it before any spend.

The original plan follows unchanged.

Everything below is ordered by information per dollar, and the first two items cost nothing.

## P1 (free) — stop losing rows to root-owned artefacts

2 of 29 rows this session died to harness handling of files created as root inside the
container, not to the model:

- `pypi-server`: `snapshot rollback: git: ... failed to remove build/bdist.linux-x86_64:
  Permission denied` — the batch rollback aborted the run.
- `build-pov-ray` at the larger envelope (66 steps): `snapshot patch: git diff --no-index`
  failed on the same class.

Both are graded failures wearing a capability label. Fix: a rollback or patch that cannot
remove a root-owned path must degrade to a recorded caveat on the row (or an explicit infra
flag), never abort the attempt and never be silently counted as Unresolved.
Acceptance: a test that plants a root-owned file in the workdir mid-run, rolls back, and
asserts the run still completes with the caveat recorded.

Why first: it is free, it is a correctness bug in the measurement, and it will keep biting
every future campaign.

## P2 (free) — read the three p>=0.88 traces we lose completely

`make-mips-interpreter` (frontier 0.96), `dna-assembly` (0.96), `dna-insert` (0.88). We
score **0** on all three, at every envelope tried, and all three burned the whole 1M-token
cap at 68-75 steps. These are well-specified tasks the frontier solves almost always, so
the failure is ours to explain.

Read the banked traces and answer, per task: did the agent produce an artefact at all; was
it the wrong form; was it blocked by the allowlist or a missing toolchain; or did it spend
1M tokens on the wrong approach. No model calls, no spend. This is the highest-information
work available in the repo and it directly targets 3 of the 13 misses.

## P3 (small spend, in flight) — `sam-cell-seg` at 8/9

One rep at 1M/120/240 passed **8 of 9** tests, against 0/9 at the frozen recipe. One or two
confirmation reps (~$0.18 each) decide whether that converts to a solve (6/18) or is the
same partial-credit ceiling the other three hit. Do not spend beyond that: it is one test
short and the marginal value is confirmation, not discovery.

## P4 (blocked on an owner decision) — the matched-model rival row

The 39-point gap is mostly model class and cannot be deconfounded with available models:
`deepseek-v4-pro` is banned by standing owner directive, and the frontier rows are
frontier-class. So the product question still needs **more shared tasks with rival rows**,
and there are exactly **2** on disk (`session-window-debug`, `layout-config-recreation2`).

Two options, both the owner's call:
1. Run the rival harnesses (hermes, pi) on more of the 18 tasks — their compute, our
   coordination, and it makes the comparison matched for the first time at n > 2.
2. Downgrade the claim: present `2026-11-11-slice-a-vs-published.md` as unmatched external
   context and stop calling internal n=2 a comparison.

## Do not do (each already measured)

- **More envelope.** 0 of 11 converted at 3.3x tokens / 2x steps; 5 of 11 were still
  token-terminated; the 2026 result says budget and information substitute.
- **The completion checklist.** Arm 2, 16 rows, no named test flipped, registered KILL.
- **A `--completion-check` predicate.** No post-fix row reaches `done` and fails, so it has
  no trigger; the failing rows die on the cap mid-work.
- **Raising caps on the three p>=0.88 tasks.** They already had 1M and still scored 0.
