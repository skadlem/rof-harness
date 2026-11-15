# Failed-trace audit — failures are token deaths, not false "verified" claims; 20% of requests are a lone helper-script write, which the eval prompt instructs (2026-11-14)

Zero model calls. Prompted by the 2026-11-14 literature sweep
(`research/lit-sweep-2026-11-14.md`): a TB4 study (arXiv 2610.00917) found failed runs
mostly end by claiming verification, and that unusable tool feedback decides outcomes.
This note checks both patterns on our own traces and measures what fills the input.

## Trace set (measured)

62 runs with an `events.jsonl` under `~/.local/share/rof-tb/`:
`evidence/slice-a-2026-11-12` (35), `work/deliverable-ab1` (5),
`work/variance-reps-2026-11-13` (9), `work/slice-a-execfix-2026-11-13` (13). Only the last
set ran with the two exec fixes. The set mixes budgets (the envelope arm ran at about 1M
tokens). 2,330 requests, 2,986 tool calls, 22.73M input tokens (61% cache reads), 2.17M
output tokens. Scripts: `~/.local/share/rof-tb/tools/audit-2026-11-14/`.

## How runs end (measured)

| Reward | Run outcome | Runs |
|---|---|---|
| 0 | `Failed: tokens` | 47 |
| 0 | `Passed` (the agent declared done) | 2 |
| 0 | snapshot failure (fixed in `dc7164e`) | 2 |
| 0 | `Aborted` | 1 |
| 1 | `Passed` | 8 |
| 1 | `Failed: tokens` | 2 |

- The "closing report claims verified" pattern applies to **2 of 52** failed runs
  (`model-extraction-relu-logits`, 10 requests; `dna-insert-rep3`, 25 requests). Both
  closing messages state the result was verified; both fail the single verifier test. Read
  by hand, not by regex.
- 47 of 52 failures never reach a closing message. A better completion check cannot help a
  run that does not finish, which matches the closed completion-checklist result.

## Tool feedback (measured)

After the 2026-11-13 fixes (13 runs, 416 `exec` calls) the remaining error classes all
carry a readable message: missing executable (17), Python traceback (13), `[exit N]` (6),
"exec has no shell" (5), plus 71 allowlist denials spread over many commands (that re-run
used `--exec-policy allowlist`). Pre-fix traces add `timeout after 300s` (8). All readable.

- 38 post-fix `exec` calls return empty output with a zero exit: `find` 18, `ls` 6,
  `python3` 6, `g++` 3, `cp` 3, `mkdir` 1, `git` 1. By command these look like legitimate
  empty results; the individual calls were not read.
- 75 of 79 `exec` results at the 8,000-character cap carry the harness's own omission
  marker.
- No new silent-failure class was found by this census. It is a census of result strings,
  not a reading of every call.

## What fills the input (estimate)

Per-request composition estimated from event sizes at 4 characters per token. The estimate
covers only **70% of billed input tokens**; the unexplained 30% is larger than any row
below, so these are shares of the estimate, not of the bill:

| Part | Share of the estimate |
|---|---|
| System message and tool definitions (first-request size) | 27% |
| Tool-call arguments, never trimmed | 24% |
| Echoed reasoning (last 2 kept; from `reasoning_tokens`, not read off the wire) | 21% |
| Live tool results (last 5) | 19% |
| Collapsed result stubs | 9% |

- `request.rs` passes `tool_calls` through whole; only results and reasoning are trimmed.
- 502 of 518 `write` calls write a script (`.py`, `.sh`, ...): 587k characters, re-sent on
  every later request. Stubbing arguments older than 5 tool calls (keeping 120 characters)
  would save a **median 5% of billed input** per run, 10% pooled, about one extra request
  per run. It reaches 30–43% on `sam-cell-seg`. `exec` arguments are 0.3%.

## The round-trip cost (measured)

- **472 of 2,330 requests (20.3%) consist of a single tool call that writes a script** not
  named in the task goal (11 more write a script the goal names). Of 502 script writes,
  436 are first run in a later request and 0 in the same request (median gap 1).
- The eval prompt asks for this. `GOAL_TAIL` (`crates/rof/src/eval_agent.rs:23`) says exec
  has no shell and stdin is closed, and "write scripts to files and run them when you need
  composition". The workflow contract adds "never write files via exec".
- A reachable shell does not remove it: in the 12 runs where the model used `sh -c`, lone
  script writes are 15.6% of requests, against 21.5% in the other 50.
- `--shell on` adds a `shell` tool but leaves `GOAL_TAIL` and the contract unchanged, so
  the existing shell arm would probably not change this (inferred from the two points
  above, not run).
- Upper bound if every lone script write were merged with its run: 472 requests at a mean
  9,756 input tokens, about 20% of input, or 6 requests in a 30-request run (inferred).

## Cache dollars (measured bound)

At PEAK rates the 62 runs cost $5.37: $2.68 cache misses, $0.08 cache hits, $2.61 output.
Perfect prefix caching would save at most the miss half, about $0.04 per run at PEAK.
Inferred, not replayed: `--collapse-hysteresis` cannot reach that bound, and it keeps more
results whole, so input per request rises and a `Gross`-budget run gets fewer requests. No
replay was built: the saving is bounded at half the bill and it trades against requests.

## Reading

- Closed: the false-verification pattern (2 of 52) does not explain the misses, and the
  census found no further silent-feedback class.
- Low headroom: argument stubbing, about 10% pooled. Collapsing whole finished scopes
  (Spine, arXiv 2610.37224) was **not** estimated.
- The lone-script-write round trip is an **efficiency** finding, not a capability lead.
  Merging it frees at most 20% of the budget, and the envelope arm already gave 3.3x the
  tokens and converted 0 of 11 misses. It would lower tokens per solved task; there is no
  evidence it flips a miss. No paid arm is proposed on it.
- If it is fixed anyway, the fix is in the prompt and the exec surface (one call that
  writes and runs), which changes the baseline recipe; any comparison must hold
  `--exec-policy` fixed on both sides.
- Still unexplained: what a token-death run does wrong in its first requests. The
  trajectory study in the sweep (arXiv 2607.09510) puts the decisive error at a median of
  step 7. A hand reading of the stable misses against their oracle solutions is the free
  next step.
