# Exec observation bugs — the model was shown wrong results; both fixed, effect unmeasured (2026-11-13)

Zero model calls. Found by reading the nine
[easy-miss-variance](2026-11-13-easy-miss-variance.md) traces for where the 300k tokens go.
Census basis: 49 banked traces under
`~/.local/share/rof-tb/` — 35 Slice A evidence (`evidence/slice-a-2026-11-12/`), 5 forcing
arm (`work/deliverable-ab1`), 9 variance reps (`work/variance-reps-2026-11-13`) — **1,698
`exec`/`test` calls**.

## Two harness bugs (measured)

| bug | mechanism | prevalence |
|---|---|---|
| Backslashes stripped inside double quotes | `split_cmd` dropped the backslash before any character in a double-quoted argument (POSIX keeps it unless it precedes `` $ ` " \ ``); `grep -n "malloc\|0x\|size" f` ran as the literal `malloc\|0x\|size`, printing nothing. | **78 calls in 13 traces**; in `make-mips-interpreter-rep3`, 5 greps over the source tree returned empty this way |
| Silent non-zero exit shown as empty output | `ToolOutcome.success` is never rendered to the model, so `which python3` on an image without it, or a no-match `grep`, returned `""`. | **79 of 475 failed `exec` calls**; in `dna-assembly-rep2` the first six requests re-ask which tools exist after an empty `which` |

Fix: POSIX rule in `split_cmd` and `[exit N]` appended on non-zero exit
(`crates/tools-std/src/runner.rs:20`, `:441`); tests `split_cmd_honors_quotes` and
`exec_reports_nonzero_exit_when_output_is_empty` (`crates/tools-std/src/lib.rs:1348`, `:1366`),
both written failing first.

## Where the tokens go (measured, nine reps plus one solve)

- Runs end after **24–36 requests**, not 60 steps: mean input is about 10k tokens per
  request and the `Gross` shape counts all of it.
- DNA runs spend **44–61k tokens on reasoning** (15–20% of the cap), mostly in bursts of
  exactly 8,000 before a one-line tool call. `make-mips-interpreter` spends 4–15k.
- `make-mips-interpreter` never starts `vm.js`: all three runs are still reading source at
  request 24–25.

## Reading

- Inferred: both bugs cost requests on any task that greps with alternation or probes for
  tools, and a 300k run only has about 30 requests. Whether fixing them moves any row is
  **unmeasured**; no paid run was made (balance $0.80).
- Every banked row, including the 5/18 baseline, predates the fix.
