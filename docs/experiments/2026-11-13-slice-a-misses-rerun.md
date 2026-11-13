# Slice A misses re-run on the fixed binary — 2 of 13 solve, neither attributable to the exec fixes (2026-11-13)

Question: do the two exec fixes ([exec-observation-bugs](2026-11-13-exec-observation-bugs.md))
flip any of the 13 rows the faithful Slice A run missed?

Setup: one rep of each of the 13 Unresolved rows from
[slice-a-faithful](2026-11-11-slice-a-faithful.md), same flags (300k gross / 60 steps / 120
actions, `--exec-policy allowlist` with the 50-entry list read back from the banked report
provenance), binary at `db10e63`. Rows:
`~/.local/share/rof-tb/work/slice-a-execfix-2026-11-13/`. The 5 originally solved rows were
not re-run.

## Result (measured)

**2 of 13 Resolved**; the other 11 Unresolved, all halted on the token cap.

| Flipped row | Original failure | Attribution |
|---|---|---|
| `pypi-server` | infra: `snapshot rollback ... Permission denied` aborted the run at request 20 | the root-owned-artefact fix `dc7164e`, which postdates the original row; the new run went 42 requests and finished |
| `model-extraction-relu-logits` | voluntary stop after 10 requests, wrong answer | none: 0 backslash-in-quotes commands and 0 `[exit N]` results in either run; run-to-run variance |

`sam-cell-seg` saw 13 `[exit N]` results and still failed. Cost: report sum $0.87 peak
bound; live balance $0.80 → $0.36.

## Reading

- The exec fixes have **no attributable solve** on this slice at one rep. They stay in as
  correctness fixes, not as a capability claim.
- Do not report 7/18: the 5 solved rows were not re-run, and one flip is variance. What is
  supported: `pypi-server` was an infra loss and now solves (1 rep), so the infra-corrected
  count is 6/18 with the same single-rep caveat.
- Single-rep verdicts on this slice move both ways without a treatment
  (`model-extraction-relu-logits` here, `dna-insert` 1 in 6 now). Any future arm needs
  reps on both sides.
- 11 of 13 still die on the token cap.

## The 8,000-token response cap is not the cause (measured, zero model calls)

Census over 58 banked traces, 2,213 requests, 23.6M tokens:

- 91 requests (4.1%) reach the cap, in 29 of 58 runs; 86 of them are reasoning that
  stops at exactly 8,000. All 91 are still followed by a tool call, so no response is lost.
- Capped responses are **3.4% of all tokens spent**. All output is 8.9% of spend (83% of
  it reasoning); input is the other 91%.
- Reading: the cap bounds reasoning spend rather than wasting it, and raising it would
  cost more. The token deaths are input accumulation (about 10k per request under the
  `Gross` shape), as the [reread-census](2026-11-12-reread-census.md) already found.
  Whether a reasoning burst cut at 8,000 leads to a worse next action is unmeasured.
