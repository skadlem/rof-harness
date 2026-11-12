# Zero-write prevalence by step, on our own 29 banked runs (2026-11-11)

Step 1 of the measurement order that `2026-11-11-lit-loop-termination.md` §7 demands
before any termination gate is built. Zero model calls: recomputed from the 29 banked
`events.jsonl` traces (18 baseline + 11 envelope), first `write`/`edit`/`multi_edit`
tool call keyed to the assistant-turn index.

## Result

n=29, solved=5. First write step:

- **solved runs:** 2, 3, 3, 5, 12
- **failed runs:** 2, 2, 2, 2, 3, 3, 3, 3, 5, 5, 6, 7, 10, 11, 11, 13, 18, 20, 21, 24, 26, 28, 44, **never** (one run, `make-mips-interpreter`, never wrote at all)

| checkpoint k | runs still write-free | of those, solved | P(success \| no write by k) | pooled P(success) |
|---:|---:|---:|---:|---:|
| 3 | 18 | 2 | 0.11 | 0.17 |
| 5 | 15 | 1 | 0.07 | 0.17 |
| 8 | 13 | 1 | 0.08 | 0.17 |
| 10 | 12 | 1 | 0.08 | 0.17 |
| 15 | 8 | 0 | 0.00 | 0.17 |

## What this establishes, and what it does not

**The falsification condition is not met.** The literature's stop rule was
"if `P(success | zero writes by k) >= P(success)`, build no termination machinery".
Here the conditional probability is **below** the pooled rate at every checkpoint
(0.07-0.11 vs 0.17), so the signal is not vacuous.

**But it is weak, and pooled.** It survives only as a *gate* (force a write, keep the run
alive), never as a *killer*: at k=5 a killer would take one of the five successes with it.
The literature's measured winner is the inverse anyway — a gate that blocks a premature
action, not a detector that predicts failure.

**The confound is fatal to a strong claim.** Our five successes sit on five
disproportionately easy tasks, so "write-free runs succeed less" may be task difficulty
wearing a trajectory costume. That is exactly what the 0.48-0.52 within-task AUC results
warn about: pooled apparent signal, within-task chance.

**So the next measurement needs one mixed-outcome task**, and we currently have none:
`sam-cell-seg` has 3 runs and 0 solves; `hf-model-inference` has 4 runs and 4 solves.

## Cheapest path to a mixed-outcome task

`hf-model-inference` solves at **$0.0155/run** — the cheapest task on the slice by an
order of magnitude. Three to four more reps (~$0.05-0.06) either produce a mixed outcome
(and the within-task estimate the literature says is required) or establish it as a stable
solve, in which case `sam-cell-seg` at 3M is the other candidate.

No gate should be built or enabled before that number exists.
