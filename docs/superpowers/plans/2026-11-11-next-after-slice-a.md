# Next after the Slice A session — P1–P4 closed; the named-deliverable predicate stays shadow-only (2026-11-11)

**Cost convention (corrected 2026-11-12).** The `dollars` field in every report is the
price-book **PEAK upper bound** (`crates/provider-openai/src/pricing.rs`: DeepSeek bills
off-peak at exactly half). Sums below are therefore upper bounds, not the bill: the owner's
live balance shows **$1.56 remaining** of $3.12, i.e. roughly half the reported sum, which is
what off-peak billing predicts. Quote the balance, not the sum.

State at hand-off: **5/18 on the faithful Slice A slice**, **$1.56 left of the $3.12
balance** (owner's live balance, 2026-11-12). Frontier-expected on the same 18 tasks is 12.08
(67.1%), a 39-point gap that is **not attributable** because the frontier column is
frontier-class models and ours is flash-class.

## Status (2026-11-12): P1–P4 closed

| Item | Outcome | Record |
|---|---|---|
| P1 root-owned artefacts | 2 of 29 rows lost to them; fixed in `dc7164e` (`crates/snapshot/src/git.rs`, `crates/agent-loop/src/run.rs`). | commit |
| P2 three published-easy misses | Harness-caused; all three never wrote the named deliverable (`vm.js`, `primers.fasta` ×2) and died on the token cap. Allowlist denials were real (22) but removing them flipped no test. | [three-misses](../../experiments/2026-11-11-three-misses.md), [allowlist-and-named-deliverable](../../experiments/2026-11-11-allowlist-and-named-deliverable.md) |
| P3 `sam-cell-seg` | Not a solve: 8/9 then 6/9 at 1M, against 0/9 at the frozen recipe — the same partial-credit ceiling. Slice A stays 5/18. | [envelope arm](../../experiments/2026-11-11-slice-a-envelope-arm.md) |
| Zero-write gate (added) | Killed: on `hf-model-inference` (3 of 4 recorded runs solved) every run wrote by step 3, including the failure. | [hf-within-task](../../experiments/2026-11-11-hf-within-task.md) |
| P4 rival rows | Option 2 taken (2026-11-12): `slice-a-vs-published` stands as unmatched external context; internal n=2 is not called a comparison. No rival runs. | below |

Surviving lead: the **named-deliverable predicate** (no banked run that lacked its named
deliverable at step 15 solved, 0/13; a step-15 gate would have stopped 0 of 2 solves).
Forcing treatment measured 2026-11-12 on the DNA pair: 0 of 4, directive read and ignored
([deliverable-forcing-arm](../../experiments/2026-11-12-deliverable-forcing-arm.md)). It stays
off. A 2026-11-13 re-run of the `dna-insert` control row solved after the shadow line fired,
so the predicate is shadow-only, not a stop signal (same note).

Balance: $1.56 was read before the two `--exec-policy any` runs, so the live balance is
somewhat lower. Re-read it before any spend.

## The rival-row question — decided, not planned

The 39-point gap is mostly model class and cannot be deconfounded with available models:
`deepseek-v4-pro` is banned by standing owner directive, and the frontier rows are
frontier-class. Deconfounding needs **more shared tasks with rival rows**, and there are
exactly **2** on disk (`session-window-debug`, `layout-config-recreation2`). Not part of this
plan: option 2 was taken — present `2026-11-11-slice-a-vs-published.md` as unmatched external
context and stop calling internal n=2 a comparison.

## Do not do (each already measured)

- **More envelope.** 0 of 11 converted at 3.3x tokens / 2x steps; 10 of 11 were still
  token-terminated; the 2026 result says budget and information substitute.
- **The completion checklist.** Arm 2, 16 rows, no named test flipped, registered KILL.
- **A `--completion-check` predicate.** No post-fix row reaches `done` and fails, so it has
  no trigger; the failing rows die on the cap mid-work.
- **Raising caps on the three p>=0.88 tasks.** They already had 1M and still scored 0.
