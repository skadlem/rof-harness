# Slice A oracle pre-flight — 2026-11-11

Harbor job `2026-11-11__17-31-42`, agent `oracle`, dataset `terminal-bench/terminal-bench-2-1`
(local, 89 tasks), Slice A's 21 ids, `-n 2`, in-container reward-file estimand.
**No model inference: $0 API spend.** Raw: `/tmp/harbor-pf21/2026-11-11__17-31-42/result.json`.

Read this as the campaign's precondition, not a capability result. A task whose own
gold solution does not pass cannot produce a capability observation, so the
non-passing rows are EXCLUDED from any agent arm.

## Result — 18/21 gold PASS

| task | gold |
|---|---|
| `break-filter-js-from-html` | PASS |
| `build-pov-ray` | PASS |
| `custom-memory-heap-crash` | PASS |
| `dna-assembly` | PASS |
| `dna-insert` | PASS |
| `headless-terminal` | PASS |
| `hf-model-inference` | PASS |
| `install-windows-3.11` | PASS |
| `kv-store-grpc` | PASS |
| `log-summary-date-ranges` | PASS |
| `make-mips-interpreter` | PASS |
| `mcmc-sampling-stan` | FAIL (reward 0) |
| `model-extraction-relu-logits` | PASS |
| `mteb-retrieve` | ERROR (no reward file) |
| `protein-assembly` | FAIL (reward 0) |
| `pypi-server` | PASS |
| `query-optimize` | PASS |
| `raman-fitting` | PASS |
| `sam-cell-seg` | PASS |
| `sanitize-git-repo` | PASS |
| `video-processing` | PASS |

Job stats: 21 trials completed, **1 errored**, mean reward **0.857**.
All three non-passes are pre-existing and independently confirmed here:

- `mcmc-sampling-stan` — `test_rstan_package_installed` fails: the image has no
  rstan. Already excluded in README ("no rstan in-image"); this run re-confirms it
  from the verifier's own stderr rather than inheriting the claim.
- `protein-assembly` — `File /app/gblock.txt does not exist`: the oracle does not
  produce the deliverable its own tests require. Defective gold, matching the
  README's "gold 0/1".
- `mteb-retrieve` — errored with no reward file (verifier network error at
  grading), matching the README's recorded hazard.

## Conditions this pre-flight depended on

Two oracle runs scored 0 before this one, both on `curl: (35) Recv failure` while
downloading the uv tarball, because container traffic was unmarked and entered a
full-tunnel default (`ip rule 31269 ... lookup 52075`, table 52075 =
`default dev laptop`) that cannot serve `github.com/20.205.243.166`. 18 of 21
graders bootstrap uv that way. One root rule restored parity:
`ip rule add to 20.205.243.0/24 lookup main pref 31000`. After it, a container
fetched the tarball byte-identically (21,370,871 bytes) and every graded task
passed. **Any future campaign on this host depends on that rule.** It does not
survive a reboot.
