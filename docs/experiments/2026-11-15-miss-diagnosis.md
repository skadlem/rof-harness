# Miss diagnosis with reasoning text — the model derives the answer and runs out before writing it; it paces by steps and misreads the token budget line (2026-11-15)

Zero model calls. The write-ahead logs now carry reasoning text the
[three-misses](2026-11-11-three-misses.md) read lacked. Three agents each read one stable
miss against its oracle `solve.sh`; their reports are local-only in
`research/miss-diagnosis-2026-11-15-<task>.md`. Census numbers below were recomputed by the
main session (`audit8`–`audit10` in `~/.local/share/rof-tb/tools/audit-2026-11-14/`).

**Contamination:** the oracle solutions were read. Any change motivated by this note is
tuned to these three tasks; they cannot serve as evidence that it works.

## Per task (agent reads; "would have passed" is inferred, nothing was re-run)

| Task | What the model had | What happened |
|---|---|---|
| `dna-insert` | Valid primer pair in 6 of 7 runs | Budget ended before `primers.fasta`: the solve wrote a first file at request 27, the passing one at request 36 (last before the cut). The primary run decided at request 7 that primer3 was absent and rebuilt its Tm model by hand. |
| `dna-assembly` | Correct layout and overhang rule; rep 1 had every oracle fragment boundary by request 26 | No run wrote a byte of the 16-line deliverable or planned to. 16 of 32 primary requests hunted an interpreter or Tm tool behind allowlist denials. |
| `make-mips-interpreter` | Knowledge to start `vm.js` by request 9 (gross 95k) | Planned `vm.js` at requests 10, 12 and 13 and wrote reconnaissance scripts instead. Requests cost 17–21k input, so the budget bought about 20. |

No knowledge gap in any of the three; all three are pace: the answer is reached, or
reachable, and the file is left for last.

## The model does not pace by tokens (measured)

- The budget line is `budgets remaining: steps 54/60; actions 103/120; tokens 280322/300000`
  (`crates/agent-loop/src/request.rs`, `budget_line`).
- Of 27 runs with reasoning in the log, 25 cite the step or action budget and 11 the token
  budget.
- `N/M` is read both ways: "tokens budget is large (29693/300000 used)" when 29,693 remained;
  "Tokens 264387/300000: ~35k tokens left" when 264,387 remained; "44 steps left" with about
  three requests of tokens left.
- No run converts tokens into requests. Tokens bind every run; steps never do.

Inferred: this is why the step-15 deliverable directive was read and ignored, and why the
completion checklist did nothing. The model believes it has 35 to 50 turns left.

## The batch rollback rule (measured)

Any errored call in a message rolls the workdir back to the batch baseline and tells the
model "your successful changes in it are gone; re-apply them" (`ROLLBACK_NOTICE`).

- 492 of 2,320 tool batches (21%) trip it; 294 of 621 multi-call batches (47%).
- Errors are mostly exploratory: 285 allowlist denials, 91 non-zero exits or tracebacks, 79
  empty failures (pre-fix), 55 missing executables.
- Real loss is small: 9 batches reverted a successful write; 19 requests are byte-identical
  re-writes.
- It also fires on batches with nothing to revert (198 single-call failures).
- For the lone-script-write finding in [failed-trace-audit](2026-11-14-failed-trace-audit.md):
  telling the model to send `write` and `exec` together would delete the new script whenever
  it exits non-zero.

## Other points from the reads (not re-verified)

- The oracle `vm.js` is about 1,700 lines, roughly 15k tokens, above the 8,000 response cap,
  so it cannot be written in one call.
- The agent's write-ahead log is readable in the workdir; two runs read it (primary request 3,
  rep 3 request 2).
- Primary and repeat runs differ in exec policy (allowlist against any), recorded in the
  report provenance, not the trace directory.

## Reading

Three candidate changes, each a prompt or loop change needing a paid arm on **other** tasks
and its own ablation:

1. **Budget line in the binding unit.** State tokens unambiguously, and as requests left at
   the current request size. Targets the one cause common to all three tasks.
2. **Rollback notice only when something was reverted**, and no revert of a batch's writes on
   a plain `exec` failure. `--preserve-failed-checks on` covers verification calls only.
3. **Send `write` and `exec` together**, which depends on 2.

**Built (2026-11-15): change 1, as `--budget-line requests`, off by default and unmeasured.**
The line becomes `budgets remaining: at most 3 replies left, counting this one (49700 tokens
left of 50000; the last reply cost 20000); steps left 16 of 20; actions left 24 of 30`. The
count is tokens left divided by the last settled reply's charge, rounded up, capped by steps
left, and includes the reply the line asks for; before the first reply settles and after
`--resume` the line gives tokens, steps and actions with no count. No instruction is added,
so the arm changes one variable. No live model run was made.

Estimator replayed on the banked traces (measured;
`~/.local/share/rof-tb/tools/audit-2026-11-14/replay_budget.py`; 38 token-death runs, 1,188
request points; 11 token-death runs totalling over 350k excluded as the larger-envelope arm,
whose cap is inferred from the set name):

| Requests actually left | Points | Median error | Exact | Within 1 |
| --- | --- | --- | --- | --- |
| 1-3 | 114 | 0 | 68% | 95% |
| 4-10 | 266 | +1 | 23% | 52% |
| 11 or more | 808 | +10 | 4% | 11% |

It mostly overestimates early because requests grow, and "at most" is not a strict bound: the
count is too low on some points where requests shrink. Rounding down instead is exact on 25%
of the 1-3 points. The replay checks the arithmetic only, not whether the model acts on the
line.

Not established: that any of these produces a solve at 300k. The near misses were one or two
requests short; `make-mips-interpreter` was not near.
