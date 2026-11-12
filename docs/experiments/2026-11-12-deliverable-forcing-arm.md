# Named-deliverable forcing arm — the directive is read and ignored; 0 of 4 solved (2026-11-12)

Four paid rows, frozen recipe 300k/60/120, `--exec-policy any`, `deepseek-flash`,
`--deliverable primers.fasta`, steer off vs on. Driver: `tools/deliverable_ab1.sh`.
Rows: `~/.local/share/rof-tb/work/deliverable-ab1/`. Binary built from `34f2bf0`.
Reported cost $0.43 at PEAK rates (upper bound). One rep per cell.

## Result (measured)

| task | steer | verdict | steps | halt | missing at step 15 | directive delivered | `primers.fasta` at end |
|---|---|---|---:|---|---|---|---|
| dna-assembly | off | Unresolved | 33 | tokens | yes | no | absent |
| dna-assembly | on | Unresolved | 31 | tokens | yes | yes | absent |
| dna-insert | off | Unresolved | 29 | tokens | yes | no | absent |
| dna-insert | on | Unresolved | 26 | tokens | yes | yes | absent |

The mechanism fired as built: `deliverable-missing step=15` is in all four run logs and the
directive text is in the two steer-on logs only.

## What the steer-on runs did with it

The directive arrived appended to the step-15 tool result. In `dna-insert` the next
assistant turn's reasoning quotes it and defers: "Let me run ctx.pl first (it's a check,
not creating output)". Neither steer-on run ever issued a write to `primers.fasta`; both
kept writing analysis scripts (`design.pl`, `opt.pl`, `scan.pl`; `map.py`, `cand.py`) until
the token cap. Tool mix after step 15 is the same shape with and without the directive.

So a single appended sentence does not change the plan. This does not test a stronger
form (deliverable-first from step 1, or a gate that withholds other tools).

## A harness-caused loss in the control row

`dna-insert` steer-off did reach for the deliverable: at step 27 it called
`write primers.fasta` with a complete primer pair and got `failed: Permission denied
(os error 13)`. The path already existed, created as root by an earlier in-container
script, and the host-side `write` tool could not open it. The run then `rm -f`'d it and hit
the cap two steps later. Whether that pair would have passed the verifier is unknown.

Same error class in the banked rows: 2 occurrences in `slice-a-faithful/pypi-server`, none
elsewhere in the 29 Slice A traces. Fixed in the same change as this note: `write`/`edit`
now replace a file they cannot open when they own its directory
(`crates/tools-std/src/common.rs` `overwrite`, test
`write_and_edit_replace_a_file_they_cannot_open_for_writing`).

## Verdict

- Forcing directive at step 15: **no effect on 2 tasks, 1 rep each**; stays off.
- Shadow predicate: 4 of 4 failing runs flagged, consistent with the earlier 0/13. It
  remains usable as a zero-inference stop/reallocation signal, still with only 2 solved
  runs behind its false-stop count.
- Next free step is not another directive: re-run nothing until the write fix is in the
  binary, then the cheapest informative row is `dna-insert` steer-off again (about $0.05
  off-peak), because that run was two steps from a submitted deliverable.
