# Slice A on the faithful pipeline — first number (2026-11-11)

Agent leg (`rof eval --agent --in-container`), faithful seed (`workdir_seed = ImageApp`
on every row), frozen recipe **300k gross / 60 steps / 120 actions**, model
`deepseek-flash` on `api.deepseek.com` (priced, so the per-instance `$` cap is live),
task `--exec-policy allowlist` with the standard recipe list.

Slice: the 18 of Slice A's 21 ids whose gold passes the oracle pre-flight
(`2026-11-11-slice-a-oracle-preflight.md`). The three excluded: `mcmc-sampling-stan`
(no rstan in-image), `protein-assembly` (defective gold), `mteb-retrieve` (verifier
network error). One rep each.

## Result

**5/18 solved = 0.278 [0.125, 0.509] (Wilson 95%)**, total spend **$0.9697**,
median wall 205s.
Excluding the one infra row: **5/17 = 0.294 [0.133, 0.531]**.

| task | verdict | steps | tokens in | tokens out | $ | wall s |
|---|---|---:|---:|---:|---:|---:|
| `break-filter-js-from-html` | **Resolved** | 43 | 250,639 | 17,416 | $0.0507 | 207 |
| `headless-terminal` | **Resolved** | 32 | 278,139 | 34,394 | $0.0766 | 308 |
| `hf-model-inference` | **Resolved** | 23 | 93,009 | 4,266 | $0.0155 | 235 |
| `kv-store-grpc` | **Resolved** | 17 | 55,327 | 4,160 | $0.0127 | 77 |
| `log-summary-date-ranges` | **Resolved** | 12 | 56,817 | 2,528 | $0.0082 | 32 |
| `build-pov-ray` | Unresolved | 40 | 291,864 | 16,884 | $0.0575 | 177 |
| `custom-memory-heap-crash` | Unresolved | 43 | 284,514 | 18,283 | $0.0646 | 192 |
| `dna-assembly` | Unresolved | 36 | 266,847 | 42,818 | $0.0926 | 334 |
| `dna-insert` | Unresolved | 32 | 244,976 | 55,885 | $0.0998 | 400 |
| `install-windows-3.11` | Unresolved | 47 | 297,450 | 10,519 | $0.0470 | 212 |
| `make-mips-interpreter` | Unresolved | 28 | 292,745 | 11,036 | $0.0535 | 123 |
| `model-extraction-relu-logits` | Unresolved | 10 | 65,431 | 18,736 | $0.0333 | 135 |
| `pypi-server` | infra (snapshot) | 20 | 62,979 | 3,507 | $0.0109 | 52 |
| `query-optimize` | Unresolved | 39 | 291,679 | 20,420 | $0.0693 | 205 |
| `raman-fitting` | Unresolved | 36 | 271,097 | 32,876 | $0.0844 | 397 |
| `sam-cell-seg` | Unresolved | 30 | 261,962 | 51,887 | $0.1046 | 948 |
| `sanitize-git-repo` | Unresolved | 18 | 297,632 | 6,317 | $0.0349 | 78 |
| `video-processing` | Unresolved | 37 | 298,675 | 10,615 | $0.0535 | 141 |

## Halt distribution — the finding that matters

- **5** `done` (all solved): break-filter, hf-model-inference, kv-store-grpc,
  log-summary-date-ranges — and `headless-terminal`, which halted `tokens` but was
  still **Resolved**: the work passed before the cap ended the run.
- **11 of the 13 failures** halted `tokens` — the cap ended them mid-work:
  build-pov-ray, custom-memory-heap-crash, dna-assembly, dna-insert,
  install-windows-3.11, make-mips-interpreter, query-optimize, raman-fitting,
  sam-cell-seg, sanitize-git-repo, video-processing.
- **1** `snapshot` — `pypi-server`: rollback could not remove root-owned `build/`
  files created inside the container ("Permission denied"). The documented
  root-ownership hazard, **not** a capability observation; excluded above.
- **1** with **no halt** — `model-extraction-relu-logits` stopped on its own after
  10 steps and failed. That is a capability failure with the envelope not binding,
  and it is the one row that already tells us a bigger cap cannot help.

So **11 of 13** failures are token-terminated rather than converged-and-failed. That is
the first faithful-pipeline evidence that the cap binds, and it is the evidence the three
earlier arms could not supply: two were pre-fix, and the joint arm's own deepest row
halted `done` with 73,508 tokens of cap unspent.

It does NOT by itself show that more budget converts — the void joint arm (0/4 cell-reps,
guardrail 1.79x/3.05x) says it did not on its cells, and the triage shows these tasks have
named tests no arm ever passed at any envelope. Those results now sit in direct tension
for these rows, and the tension is cheap to resolve: this whole 18-task run cost under a
dollar, and the 11-row envelope arm costs about two.

## Caveats

- **1 rep.** No variance estimate; a single seed per task. Wilson CI only.
- The model is `deepseek-flash`, which this project records as the same served model as
  the rivals' `deepseek-v4.1-flash`; the direct endpoint rejects the latter id (400), so
  the bare id is used. Endpoint differs from the rival rows' `opencode-go`.
- These rows do not pool with any pre-fix row, any `miss-plus-out` row, or the void 18.
- `sam-cell-seg`'s 948 s wall is inside its verifier budget but far above the median;
  it is a real row, not an infra one.
