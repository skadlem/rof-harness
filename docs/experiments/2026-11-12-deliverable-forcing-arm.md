# Named-deliverable forcing arm — the directive is read and ignored; 0 of 4 solved (2026-11-12)

Four paid rows, frozen recipe 300k/60/120, `--exec-policy any`, `deepseek-flash`,
`--deliverable primers.fasta`, steer off vs on. One rep per cell. Driver
`tools/deliverable_ab1.sh`; rows `~/.local/share/rof-tb/work/deliverable-ab1/`; binary
`34f2bf0`. Cost $0.43 at PEAK rates (upper bound).

## Result (measured)

| task | steer | verdict | steps | halt | missing at step 15 | directive delivered | `primers.fasta` at end |
|---|---|---|---:|---|---|---|---|
| dna-assembly | off | Unresolved | 33 | tokens | yes | no | absent |
| dna-assembly | on | Unresolved | 31 | tokens | yes | yes | absent |
| dna-insert | off | Unresolved | 29 | tokens | yes | no | absent |
| dna-insert | on | Unresolved | 26 | tokens | yes | yes | absent |

The mechanism fired as built: `deliverable-missing step=15` is in all four run logs and the
directive text in the two steer-on logs only.

## What the steer-on runs did with it

The directive arrived appended to the step-15 tool result. In `dna-insert` the next assistant
turn quotes it and defers: "Let me run ctx.pl first (it's a check, not creating output)".
Neither steer-on run wrote `primers.fasta`; both kept writing analysis scripts (`design.pl`,
`opt.pl`, `scan.pl`; `map.py`, `cand.py`) until the token cap. Tool mix after step 15 has the
same shape with and without the directive. A single appended sentence does not change the
plan; this does not test a stronger form (deliverable-first from step 1, or a gate that
withholds other tools).

## A harness-caused loss in the control row

`dna-insert` steer-off did reach for the deliverable: at step 27 it called
`write primers.fasta` with a complete primer pair and got `failed: Permission denied
(os error 13)`. The file already existed, created as root by an earlier in-container script,
so the host-side `write` tool could not open it. The run then `rm -f`'d it at step 29 and hit
the cap. Whether that pair would have passed the verifier is unknown.

Same error class in the banked rows: 3 failed `write` calls to the same root-owned file in
`slice-a-faithful/pypi-server` (`events.jsonl:22,39,61`), none elsewhere in the 29 Slice A
traces. Fixed in the same change as this note: `write`/`edit` now replace a file they cannot
open when they own its directory (`crates/tools-std/src/common.rs` `overwrite`, test
`write_and_edit_replace_a_file_they_cannot_open_for_writing`).

## Verdict

- Forcing directive at step 15: **no effect on 2 tasks, 1 rep each**; stays off.
- Shadow predicate: 4 of 4 failing runs flagged, consistent with the earlier 0/13. It stays
  usable as a zero-inference stop/reallocation signal, still with only 2 solved runs behind
  its false-stop count.
- The next free step is not another directive: re-run nothing until the write fix is in the
  binary, then the cheapest informative row is `dna-insert` steer-off again (about $0.05
  off-peak), because that run was two steps from a submitted deliverable.

## Follow-up (2026-11-13): the control row re-run solves and falsifies the stop predicate

One rep, `dna-insert` steer-off, same flags, binary with the write fix
(`~/.local/share/rof-tb/work/deliverable-ab1/dna-insert-steer-off-writefix/`). Measured:
**Resolved**, halt `tokens`, $0.082 peak bound. `primers.fasta` first written at tool call
35 of 44 and rewritten at call 44.

- First `dna-insert` solve at any envelope. It is one rep outside the frozen Slice A run, so
  Slice A stays 5/18.
- Not attributable to the write fix: no tool call before the first write created the file,
  so the replace path probably never ran. Run-to-run variance is the default reading.
- The shadow line `deliverable-missing step=15` fired in this run and it solved. The
  predicate's record is now 1 solve in 18 flagged runs, so a hard stop at step 15 would have
  killed this solve. It stays a shadow signal only; do not build a stop on it.

> Now: `--deliverable` logs the crossing; `--deliverable-steer` still defaults off
> (`crates/rof/src/eval_cmd.rs:17-18`).
