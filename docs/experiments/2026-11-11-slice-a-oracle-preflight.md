# Slice A oracle pre-flight — 18/21 gold PASS, three rows excluded (2026-11-11)

Harbor job `2026-11-11__17-31-42`, agent `oracle`, dataset `terminal-bench/terminal-bench-2-1`
(local, 89 tasks), Slice A's 21 ids, `-n 2`, in-container reward-file estimand.
**No model inference: $0 API spend.** Raw: `/tmp/harbor-pf21/2026-11-11__17-31-42/result.json`.

This is the campaign's precondition, not a capability result: a task whose own gold solution
does not pass cannot produce a capability observation, so the non-passing rows are EXCLUDED
from any agent arm. Job stats: 21 trials completed, **1 errored**, mean reward **0.857**.

| gold | tasks |
|---|---|
| PASS (18) | `break-filter-js-from-html`, `build-pov-ray`, `custom-memory-heap-crash`, `dna-assembly`, `dna-insert`, `headless-terminal`, `hf-model-inference`, `install-windows-3.11`, `kv-store-grpc`, `log-summary-date-ranges`, `make-mips-interpreter`, `model-extraction-relu-logits`, `pypi-server`, `query-optimize`, `raman-fitting`, `sam-cell-seg`, `sanitize-git-repo`, `video-processing` |
| FAIL (2) | `mcmc-sampling-stan` (reward 0) — `test_rstan_package_installed` fails, the image has no rstan; already the recorded "no rstan in-image", re-confirmed here from the verifier's own stderr rather than inherited. `protein-assembly` (reward 0) — `File /app/gblock.txt does not exist`: the oracle does not produce the deliverable its own tests require, i.e. defective gold (recorded gold 0/1). |
| ERROR (1) | `mteb-retrieve` (no reward file; verifier network error at grading) |

All three were already recorded before this run: defective gold (`protein-assembly`) and
no rstan in-image (`mcmc-sampling-stan`) at [decisions](../decisions.md), the
network hazard in the same file.

## Conditions this pre-flight depended on

Two oracle runs scored 0 before this one: container traffic was unmarked, so the uv tarball
download entered a full-tunnel default and failed with `curl: (35) Recv failure` (18 of 21
graders bootstrap uv that way). One root rule restored parity — `ip rule add to
20.205.243.0/24 lookup main pref 31000` — after which a container fetched the tarball
byte-identically (21,370,871 bytes) and every graded task passed. **Any future campaign on
this host depends on that rule; it does not survive a reboot.** Cause and route details:
[decisions](../decisions.md).

> `/tmp/harbor-pf21` is gone, so these counts cannot be re-derived in this checkout.
