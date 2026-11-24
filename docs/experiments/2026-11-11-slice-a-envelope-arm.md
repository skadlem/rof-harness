# Slice A envelope arm — 3.3x tokens, 2x steps, zero new solves (2026-11-11)

**Cost convention (corrected 2026-11-12).** `dollars` is the price-book **PEAK upper bound** (`crates/provider-openai/src/pricing.rs`); DeepSeek bills off-peak at exactly half. Sums below are therefore upper bounds, not the bill: the owner's balance showed **$1.56 remaining** of $3.12, roughly half the reported sum, as off-peak predicts. Quote the balance, not the sum.

Follow-up to [2026-11-11-slice-a-faithful.md](2026-11-11-slice-a-faithful.md): same 18-task slice, model (`deepseek-flash`, priced direct endpoint), seed and allowlist. The **11 tasks that failed on the token cap** were re-run at **1,000,000 gross tokens / 120 steps / 240 actions** (3.3x tokens, 2x steps), 1 rep each; the 5 baseline solves, the `snapshot` infra row and the one no-halt failure were not.

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
| sanitize-git-repo | 0/3 | **2/3** | tokens | tokens (48) |
| video-processing | 2/5 | 2/5 | tokens | tokens (75) |

Reported cost: baseline **$0.9697** + envelope **$1.8271** = **$2.7968 at PEAK rates** (upper bound).

## What it actually shows

**The envelope is a partial-progress lever, not a solve lever, at this model tier on these tasks.** Four of eleven moved their test counts, `sam-cell-seg` **0/9 to 8/9**, `sanitize-git-repo` 0/3 to 2/3, `query-optimize` 2/6 to 4/6, `build-pov-ray` 0/3 to 1/3, and all four still failed. Seven were unchanged; ten of the eleven were *still* token-terminated at 1M (48-93 steps), so 3.3x budget did not buy the whole working envelope the model wanted.

Sharper than either earlier result: the void joint arm saw "no tests move", the triage called the failures accuracy-bound; both fit, since budget moves partial credit substantially and the last test not at all. It matches the 2026 substitution finding (budget and information substitute, BRA Table 5) and explains why every budget-shaped arm so far has failed to convert: the residual needs the missing information or the correct artefact, not more turns to produce the wrong one.

## What it does not show

- Not a capability claim: 1 rep, 11 tasks, one model, one endpoint.
- Not that a *further* raise would do nothing. At 1M tokens and 93 steps the cap still terminated 10 of 11, so the model was still working, and the four improved rows improved without crossing. The marginal return of another 3x is now a guess, and the two tests it would need (`sam-cell-seg`'s ninth, `sanitize-git-repo`'s third) are not obviously turn-limited.
- Not comparable with pre-fix rows, `miss-plus-out` rows, or the dphn families.

## Cheapest next target, if the balance is topped up

`sam-cell-seg` sits at **8/9**: one named test is the gap on a task that was 0/9 before this arm. Nothing else in the corpus is that close, and only here could a small amount of budget plausibly produce a *solve* rather than a partial.

## `sam-cell-seg` reproduction (2 reps at 1M, added after the first pass)

Reproduced, not luck:

| rep | recipe | tests passed | halt | $ |
|---|---|---|---:|---|---:|
| baseline | 300k/60/120 | **0/9** | tokens | 0.1046 |
| envelope r1 | 1M/120/240 | **8/9** | tokens (53) | 0.1838 |
| envelope r2 | 1M/120/240 | **6/9** | tokens (51) | 0.1380 |

The partial-credit effect is **reproducible** (0/9 -> 8/9 and 6/9 in two independent reps) and **variable** (6 to 8 of 9), and still never a solve; not a turn-count problem, since one rep ran 1,002,269 tokens and 51 steps and still stopped short.

Session cost: **$2.9348 at PEAK rates** (baseline $0.9697 + envelope arm $1.8271 + this rep $0.1380), an upper bound; the owner's balance showed **$1.56 left of $3.12**, about $1.56 actually billed.

Incidental: the agent-phase boundary probe recorded **1 live process** at the hand-over for rep 2 — exactly the contamination class the triage could previously only speculate about.
