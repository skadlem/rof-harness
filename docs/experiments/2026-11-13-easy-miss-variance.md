# Easy-miss variance reps — 0 of 9; the three published-easy misses are stable misses (2026-11-13)

Question: after one `dna-insert` re-run solved
([deliverable-forcing-arm](2026-11-12-deliverable-forcing-arm.md)), are the three
published-easy misses coin flips, making the single-rep 5/18 baseline unreadable?

Setup: 3 reps each of `dna-insert`, `dna-assembly`, `make-mips-interpreter`, frozen recipe
(300k gross tokens / 60 steps / 120 actions), `--exec-policy any`, steers off, deliverable
shadow on, binary at `af0002b` (`binary_git_sha` in all nine reports). Rows:
`~/.local/share/rof-tb/work/variance-reps-2026-11-13/`, reports in `reports/`.

## Result (measured)

| Task | Solved | Halt | Named deliverable present at end |
|---|---|---|---|
| `make-mips-interpreter` | 0/3 | tokens ×3 | not checked |
| `dna-assembly` | 0/3 | tokens ×3 | 0/3 |
| `dna-insert` | 0/3 | tokens ×2, voluntary stop at step 25 ×1 | 1/3 (the voluntary stop; failed the verifier) |

No `Permission denied` in any DNA row, so the `write`/`edit` replace path was not exercised.
Cost: report sum $0.79 peak bound; live balance went $1.20 → $0.80.

## Reading

- `dna-insert` at this configuration is now 1 solve in 5 runs (2026-11-12 control fail,
  2026-11-13 re-run solve, these 3). The solve is the outlier, not evidence of a fix.
- `dna-assembly` and `make-mips-interpreter` are 0 in every run banked at any envelope.
- Inferred: the 5/18 baseline is not being dragged down by bad luck on these three rows.
  Variance on the 5 solved rows and the other 10 misses is still unmeasured.
- 8 of 9 runs died on the token cap without the deliverable, the same pattern as before.
