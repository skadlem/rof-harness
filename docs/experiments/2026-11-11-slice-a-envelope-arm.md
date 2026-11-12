# Slice A envelope arm — 3.3x tokens, 2x steps, zero new solves (2026-11-11)

Follow-up to `2026-11-11-slice-a-faithful.md`. Same 18-task slice, same model
(`deepseek-flash`, priced direct endpoint), same faithful seed, same allowlist. The
**11 tasks that failed on the token cap** in the baseline were re-run at
**1,000,000 gross tokens / 120 steps / 240 actions** — 3.3x tokens and 2x steps —
1 rep each. The 5 baseline solves, the `snapshot` infra row, and the one
no-halt failure were not re-run.

## Result: 0 of 11 converted to solves

| task | baseline tests | envelope tests | baseline halt | envelope halt |
|---|---|---|---|---|
| build-pov-ray | 0/3 | **1/3** | tokens | snapshot patch |
| custom-memory-heap-crash | 5/6 | 5/6 | tokens | tokens (93 steps) |
| dna-assembly | 0/1 | 0/1 | tokens | tokens (70) |
| dna-insert | 0/1 | 0/1 | tokens | tokens (75) |
| install-windows-3.11 | 2/4 | 2/4 | tokens | tokens (93) |
| make-mips-interpreter | 0/3 | 0/3 | tokens | tokens (68) |
| query-optimize | 2/6 | **4/6** | tokens | tokens (85) |
| raman-fitting | 0/3 | 0/3 | tokens | tokens (61) |
| sam-cell-seg | 0/9 | **8/9** | tokens | tokens (53) |
| sam-cell-seg (rep 2) | 0/9 | **6/9** | tokens | tokens (51) |
| sanitize-git-repo | 0/3 | **2/3** | tokens | tokens (48) |
| video-processing | 2/5 | 2/5 | tokens | tokens (75) |

Spend: baseline **$0.9697** + envelope **$1.8271** = **$2.7968** of the $3.12 balance
(remaining $0.3232). No task crossed from Unresolved to Resolved.

## What it actually shows

**The envelope is a partial-progress lever, not a solve lever, at this model tier on
these tasks.** Four of eleven moved their test counts — `sam-cell-seg` from **0/9 to
8/9**, `sanitize-git-repo` 0/3 to 2/3, `query-optimize` 2/6 to 4/6, `build-pov-ray`
0/3 to 1/3 — and all four still failed. Six were unchanged, and five of the eleven were
*still* token-terminated at 1M (48-93 steps), so 3.3x budget did not even buy the whole
working envelope the model wanted.

That is a sharper statement than either earlier result. The void joint arm said "no
tests move"; the triage said the failures are accuracy-bound. Both are consistent with
this: budget moves partial credit substantially and moves the last test not at all.

It matches the 2026 substitution finding (budget and information substitute, BRA
Table 5) and it explains why every budget-shaped arm so far has failed to convert: the
residual on these tasks needs the missing information or the correct artefact, not more
turns to produce the wrong one.

## What it does not show

- Not a capability claim: 1 rep, 11 tasks, one model, one endpoint.
- Not that a *further* raise would do nothing — but at 1M tokens and 93 steps the cap was
  still the terminator on 5 of 11, so the model was still working, and the four improved
  rows improved without crossing. The marginal return of another 3x is now a guess, and
  the two tests it would need to buy (`sam-cell-seg`'s ninth, `sanitize-git-repo`'s third)
  are not obviously turn-limited.
- Not comparable with pre-fix rows, `miss-plus-out` rows, or the dphn families.

## Cheapest next target, if the balance is topped up

`sam-cell-seg` sits at **8/9**. One named test is the gap on a task that was 0/9 before
this arm. Nothing else in the corpus is that close, and it is the only place where a
small amount of budget could plausibly produce a *solve* rather than a partial.

## `sam-cell-seg` reproduction (2 reps at 1M, added after the first pass)

The 8/9 could have been one lucky rep. It was not:

| rep | recipe | tests passed | halt | $ |
|---|---|---|---:|---|---:|
| baseline | 300k/60/120 | **0/9** | tokens | 0.1046 |
| envelope r1 | 1M/120/240 | **8/9** | tokens (53) | 0.1838 |
| envelope r2 | 1M/120/240 | **6/9** | tokens (51) | 0.1380 |

So the partial-credit effect is **reproducible** (0/9 -> 8/9 and 6/9 in two independent
reps) and it is also **variable** (6 to 8 of 9), and it still never reaches a solve. That is
the cleanest statement of this session's finding: at this model tier the envelope reliably
buys most of a task and never the last test, and the last test is not a turn-count problem —
one rep ran 1,002,269 tokens and 51 steps and still stopped short.

Cost of the whole session: **$2.9348 of the $3.12 balance** (baseline $0.9697 +
envelope arm $1.8271 + this rep $0.1380), leaving **$0.1852**.

Incidental: the agent-phase boundary probe recorded **1 live process** at the hand-over for
this run — the first time that field has fired, and exactly the contamination class the
triage could previously only speculate about.
