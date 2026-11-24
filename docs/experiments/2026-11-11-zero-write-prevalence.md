# Zero-write prevalence by step, on our own 29 banked runs (2026-11-11)

Step 1 of [lit-loop-termination](2026-11-11-lit-loop-termination.md) §7, required before any termination gate. Zero model calls: 29 banked `events.jsonl` traces (18 baseline + 11 envelope), first `write`/`edit`/`multi_edit` call keyed to the assistant-turn index.

## Result (measured)

n=29, solved=5. First write step — **solved:** 2, 3, 3, 5, 12; **failed:** 2, 2, 2, 2, 3, 3, 3, 3, 5, 5, 6, 7, 10, 11, 11, 13, 18, 20, 21, 24, 26, 28, 44; **never:** one run (`make-mips-interpreter`).

| checkpoint k | runs still write-free | of those, solved | P(success \| no write by k) | pooled P(success) |
|---:|---:|---:|---:|---:|
| 3 | 18 | 2 | 0.11 | 0.17 |
| 5 | 15 | 1 | 0.07 | 0.17 |
| 8 | 13 | 1 | 0.08 | 0.17 |
| 10 | 12 | 1 | 0.08 | 0.17 |
| 15 | 8 | 0 | 0.00 | 0.17 |

## Reading

- **Falsification not met.** The stop rule was "if `P(success | zero writes by k) >= P(success)`, build no termination machinery"; the conditional is **below** the pooled rate at every checkpoint (0.07-0.11 vs 0.17), so the signal is not vacuous.
- **Weak and pooled.** Usable as a *gate* (force a write, keep the run alive), never a *killer*: at k=5 it would take one of the five successes. The literature's measured winner is the inverse — a gate on premature action.
- **Confounded.** Five successes on five disproportionately easy tasks, so the effect may be task difficulty in trajectory costume: the 0.48-0.52 within-task AUC trap (pooled apparent signal, within-task chance).
- **Needs a mixed-outcome task; we have none.** `sam-cell-seg` 3 runs/0 solves, `hf-model-inference` 4 runs/4 solves. `hf-model-inference` costs **$0.0155/run**, cheapest on the slice by an order of magnitude; 3-4 reps (~$0.05-0.06) either mix it or establish a stable solve, else `sam-cell-seg` at 3M. No gate before that number exists.
