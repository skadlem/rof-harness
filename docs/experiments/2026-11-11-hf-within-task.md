# Within-task test of the zero-write gate — it fails on the one task we can test (2026-11-11)

Step 2 of the measurement order in `2026-11-11-lit-loop-termination.md` §7. Three
additional reps of `hf-model-inference` at the frozen recipe (300k/60/120) — the cheapest
task on the slice at $0.0155/run — to obtain a **mixed-outcome** task, which is the
precondition the literature imposes for any within-task estimate.

## Result: the task is mixed, and the write signal has no power on it

| run | steps | first write | reads | writes | outcome |
|---|---:|---:|---:|---:|---|
| baseline | 23 | 3 | 18 | 7 | Resolved |
| r2 | 23 | 3 | 24 | 4 | Resolved |
| r3 | 18 | 3 | 17 | 4 | Resolved |
| r4 | 25 | **2** | 19 | **11** | Unresolved — **agent wall timeout** |

So `hf-model-inference` is **4 of 5 solved**. And every run, including the failure, wrote
at step 2-3. The one failure wrote **earliest and most often** (11 writes vs 4-7).

**A "block reads until an artefact exists" gate would have fired on zero of these four
runs.** On the only task where we now have a mixed outcome, write-timing does not separate
success from failure — the opposite, weakly. Combined with the pooled result
(`2026-11-11-zero-write-prevalence.md`: 0.07-0.11 vs 0.17, confounded by task difficulty),
the write-gate is not supported.

This is the falsification the literature asked for, and it took $0.026 to obtain.

## The failure that did occur is a wall clock, not a loop

r4 halted on the task's own `[agent] timeout_sec` after 25 steps with 11 writes in hand —
it was working when the wall arrived, and its usage was not metered (`dollars: 0`,
cancelled). This is the **second** recorded instance on this cell: the README notes an
earlier hf row "lost to the task's own 900s agent wall at 28 steps".

Per the repo's convention a configured-limit termination is still a system attempt with
its native reward, so it counts as Unresolved. But two of roughly six known hf rows dying
to a 900 s wall while actively writing is an environment-class loss, not a capability
signal, and it should be reported as such rather than folded into a solve rate.

## What this closes

No loop detector and no write-gate should be built on this evidence. The literature's
supported direction remains: a gate on *premature action* (ECLoop, −10 pp when ablated),
budget allocation **across** tasks rather than within a run, and removing structural losses
like the wall-clock kill above.
