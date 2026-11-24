# Within-task test of the zero-write gate — it fails on the one task we can test (2026-11-11)

Step 2 of [lit-loop-termination](2026-11-11-lit-loop-termination.md) §7. Three additional reps of `hf-model-inference` at the frozen recipe (300k/60/120), cheapest on the slice at $0.0155/run, to obtain the **mixed-outcome** task the literature requires for a within-task estimate.

## Result: the task is mixed, and the write signal has no power on it

| run | steps | first write | reads | writes | outcome |
|---|---:|---:|---:|---:|---|
| baseline | 23 | 3 | 18 | 7 | Resolved |
| r2 | 23 | 3 | 24 | 4 | Resolved |
| r3 | 18 | 3 | 17 | 4 | Resolved |
| r4 | 25 | **2** | 19 | **11** | Unresolved — **agent wall timeout** |

So `hf-model-inference` is **3 of the 4 runs below solved** (measured: the four reports under `~/.local/share/rof-tb/evidence/slice-a-2026-11-12/` (`slice-a-faithful`, `slice-a-hf/r2-r4`); the original text said "4 of 5", which no recorded set reproduces). Every run, including the failure, wrote at step 2-3; the failure wrote **earliest and most often** (11 writes vs 4-7).

**A "block reads until an artefact exists" gate would have fired on zero of these four runs.** On the only task with a mixed outcome, write-timing does not separate success from failure — the opposite, weakly. Combined with the pooled result ([zero-write-prevalence](2026-11-11-zero-write-prevalence.md): 0.07-0.11 vs 0.17, confounded by task difficulty), the write-gate is not supported.

This is the falsification the literature asked for, and it took $0.026 to obtain.

## The failure that did occur is a wall clock, not a loop

r4 halted on the task's own `[agent] timeout_sec` after 25 steps with 11 writes in hand — still working when the wall arrived, usage unmetered (`dollars: 0`, cancelled). **Second** recorded instance on this cell: the 2026-11-06 capability map in [decisions](../decisions.md) records an earlier hf row that hit the task's own 900s agent wall (at 28 steps).

A configured-limit termination is still a system attempt with its native reward, so r4 counts as Unresolved. But two of roughly six known hf rows dying to a 900 s wall while actively writing is an environment-class loss, not a capability signal, and should be reported as such rather than folded into a solve rate.

## What this closes

No loop detector and no write-gate should be built on this evidence. The literature supports instead a gate on *premature action* (ECLoop, −10 pp when ablated), budget allocation **across** tasks, and removing structural losses like the wall-clock kill.
