# September 2026 benchmark history

Historical results, corrections, and interpretation from the original README.
The current measured summary remains in the [README](../../README.md).

`docs/STATUS.md` records the measured arms and the bugs they found, including the two that were
not fixes: an eager-files rewrite that turned out to duplicate retrieval already in the prompt, and
a third review round that cost tokens without moving a task. The headline result is `ROF_PLANNER=skip`
on this suite, where every task is a single change — three reps at 12.7/20 against 7.3/20 with the
planner on, at 200k fewer input tokens.

The suite total is only quotable with its split, because five analysis tasks carry no deterministic
oracle and the reviewer was their only judge. As of the last arms the honest numbers are:

| arm | judge | gated (15) | analysis (5) |
|---|---|---|---|
| planner on | self | 6-8 | 0-1 |
| `ROF_PLANNER=skip` | self | 11-12 | 1-2 |
| `skip` + independent | `stealth/union-alpha` | **11.3 (76%)** | 0.7 |

Multi-file suite (6 tasks × 3 reps, Atria): **rof 18/18, hermes 18/18, pi 16/18.**
These are the verified numbers, and they replaced an earlier claim that was wrong — see
below. On Terminal-Bench 4.0 all three score 0.0 on 8 of 8 CPU tasks at 15- and 30-minute
caps. **That floor is no longer attributed to the model** — see the correction below, but
in short: Atria is a 744B GLM-5 MoE that scores 78.3 on Terminal-Bench 2.1, so a model
competent on this benchmark family does not fall to zero across eight tasks because it is
weak. The gap implicates the harness path, and the exhaustion quirk we already found
(`content: null`, `reasoning_content=37683`) on large prompts is the prime suspect. It is
recorded as an open problem, not a settled ceiling.

**The suite is saturated on score — but not on cost.** With all three at or
within two of the ceiling, the multi-file suite has nothing left to discriminate
on *success*: a future arm that moves nothing will still score ~18. Widening it
(larger repos, indirect call chains, cross-file consistency) is now a
prerequisite for a score claim, not a nice-to-have.

Cost is the axis that still separates rof from hermes at equal success. Both
pass 18/18 on the same model, seeds, goals, and oracle, measured through one
wire proxy that bills what the endpoint bills: **rof averages 7,906 billed
tokens per task, hermes 40,528 — a 5.1x gap that holds on every task and every
rep** (worst case 2.0x, best 14x). Billed means uncached input plus output:
hermes re-sends a ~31k-character system prompt per call, but 99% of it is a
cache hit once warm, so raw input would have overstated its cost by 10x. This
confirms the Arena.ai HarnessTax prediction — harness choice moves cost by
multiples while success stays flat — and it is the first axis on which rof
measurably leads since the score saturated.

**Correction, 2026-09-21.** This section previously read "rof 17/18 vs hermes 15/18 and pi
14/18, the separation is `mf-dead-code`, which rof passes 3/3 and both others 2/3."
**That claim did not survive verification against the run's own artifacts.** Re-scoring all
54 stored work dirs — with source files untouched since the original run window — agrees
with the stored result files on every single cell, and both say hermes 18/18 and pi 16/18,
with `mf-dead-code` passed 3/3 by *all three* agents. There was no separation on that task;
the lead it appeared to give rof was a recording error, never checked against the artifacts
it came from. The corrected numbers above are what the stored evidence actually supports.

The 18/18 comes from the red-suite arm: a seeded test asserted the *buggy* value
and was rejecting correct fixes; the harness now surfaces the failing assertion to
the model (`803cd03`).

Three findings changed what the numbers mean, and each was caught by decomposing the measurement
rather than trusting the aggregate: a direct-mode run that scored 16/20 was passing the five
oracle-less tasks vacuously (`checks_pass(&[])` is true); the analysis class was unsatisfiable
because the model's prose answer travelled nested where the judge never read it; and the endpoint
returns `content: null` on 7 of 8 probes, which the harness had been scoring as a model that chose
to write nothing. The first is now a fast `no oracle` failure, the second an `ANSWER:` line, the
third a retry.
