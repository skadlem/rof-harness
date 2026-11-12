# Re-read census — collapse-forced re-reading does not explain the token deaths (2026-11-12)

Zero model calls. Tests the inference that the 5-row observation collapse forces the agent
to re-open files and that this is what exhausts the token cap.

Input: the 29 banked Slice A traces (18 faithful at 300k/60/120, 11 envelope at 1M/120/240)
under `~/.local/share/rof-tb/evidence/slice-a-2026-11-12/`. A re-read is a `view` with
arguments identical to an earlier `view` with no write to that path in between, or an
`exec`/`test` with arguments identical to an earlier one with no write in between.
Partial-range views with different offsets are not counted, so this is a lower bound.

## Result (measured)

- **61 of 1,698 tool calls are re-reads (3.6%)**; their results are **7.0%** of all
  tool-result characters.
- The three published-easy misses: `dna-insert` 0 re-reads in both arms, `dna-assembly`
  2 and 1, `make-mips-interpreter` 0 and 2.
- Highest share: `custom-memory-heap-crash` faithful, 11 of 52 calls (21%).
- Solved runs are not different: 0–4 re-reads each.

## What does exhaust the cap (measured)

Mean input per request is 3–17k tokens at 300k and 9–21k at 1M; the last request is
4–18k and 13–60k. With the `Gross` budget shape every request's whole input counts,
cached or not, so 300k is spent in roughly 30–45 requests regardless of the 60-step cap
(`dna-insert` faithful: 244,976 input tokens, of which 138,752 were cache reads).

## Reading

The collapse is not driving re-reads, so raising `observation_keep` is not supported by
these traces as a fix for the token deaths; it would raise input per request. The cap is
reached by ordinary accumulation over 30–45 requests. That leaves the question the
named-deliverable arm tests: whether those requests are spent producing the required file.

Script: one-off, not kept (counts `ToolStart` argument repeats per `events.jsonl`).
