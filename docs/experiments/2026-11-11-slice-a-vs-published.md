# Slice A: our 5/18 against the published per-task frontier rates (2026-11-11)

Free analysis, no model calls. Source of the frontier column:
`crates/eval/slices/tb-slice-a.json` → `entries[<task>].historical_pass_rate`, whose own
provenance is "measured hub job-tasks 2026-10-04 (14 jobs, 89 tasks, weighted by trials)".
Ours: `2026-11-11-slice-a-faithful.md` (300k/60/120, one rep, image-app seed).

## Result

**Ours 5/18 = 27.8%. Frontier-expected 12.08/18 = 67.1%.** A 39-point gap.

| task | frontier p | ours |
|---|---:|---:|
| make-mips-interpreter | 0.96 | 0 |
| log-summary-date-ranges | 0.96 | 1 |
| dna-assembly | 0.96 | 0 |
| kv-store-grpc | 0.92 | 1 |
| sam-cell-seg | 0.92 | 0 |
| hf-model-inference | 0.88 | 1 |
| dna-insert | 0.88 | 0 |
| query-optimize | 0.80 | 0 |
| pypi-server | 0.80 | 0 |
| model-extraction-relu-logits | 0.64 | 0 |
| build-pov-ray | 0.60 | 0 |
| install-windows-3.11 | 0.60 | 0 |
| raman-fitting | 0.56 | 0 |
| video-processing | 0.52 | 0 |
| break-filter-js-from-html | 0.36 | 1 |
| headless-terminal | 0.28 | 1 |
| custom-memory-heap-crash | 0.24 | 0 |
| sanitize-git-repo | 0.20 | 0 |

## What the profile says

- We solve every task the frontier solves at **p >= 0.88 except three** — `make-mips-interpreter`
  (0.96), `dna-assembly` (0.96), `dna-insert` (0.88) — and we solve two the frontier finds
  **hard** (`headless-terminal` 0.28, `break-filter-js-from-html` 0.36). That is not a uniform
  deficit; it is three specific, well-specified tasks we lose completely.
- All three of those misses were re-run at **1M tokens / 120 steps** and still scored 0
  (`2026-11-11-slice-a-envelope-arm.md`): make-mips 0/3 at 68 steps, dna-assembly 0/1 at 70,
  dna-insert 0/1 at 75 — every one token-terminated. So we are burning the whole 1M and still
  not producing the artefact. They are the most informative failures in the corpus.

## What it does NOT say

**The gap cannot be attributed.** `historical_pass_rate` is the rate of published submission
rows (frontier-class agents and models); ours is a flash-class model. So 27.8% vs 67.1% is
mostly a model-class statement, and this project's whole question — does the harness add
value at a MATCHED model — is untouched by it. Treat this table as context for choosing
where to look, never as a capability verdict on the harness.

It is, however, the best context table the project has: it is the same slice, per task, with
the frontier rate attached, and it cost nothing.
