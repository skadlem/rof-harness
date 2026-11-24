# Failed-trace audit — failures are token deaths, not false "verified" claims; 20% of requests are a lone helper-script write, which the eval prompt instructs (2026-11-14)

Zero model calls. Prompted by the 2026-11-14 sweep (`research/lit-sweep-2026-11-14.md`): TB4
study arXiv 2610.00917 reports failed runs mostly ending by claiming verification, and
unusable tool feedback deciding outcomes. Both checked on our traces, plus what fills the
input.

## Trace set (measured)

62 runs with `events.jsonl` under `~/.local/share/rof-tb/`: `evidence/slice-a-2026-11-12`
(35), `work/deliverable-ab1` (5), `work/variance-reps-2026-11-13` (9),
`work/slice-a-execfix-2026-11-13` (13); only the last ran with the two exec fixes. Mixed
budgets (the envelope arm near 1M tokens). 2,330 requests, 2,986 tool calls, 22.73M input
tokens (61% cache reads), 2.17M output. Scripts:
`~/.local/share/rof-tb/tools/audit-2026-11-14/`.

## How runs end (measured)

| Reward | Run outcome | Runs |
|---|---|---|
| 0 | `Failed: tokens` | 47 |
| 0 | `Passed` (the agent declared done) | 2 |
| 0 | snapshot failure (fixed in `dc7164e`) | 2 |
| 0 | `Aborted` | 1 |
| 1 | `Passed` | 8 |
| 1 | `Failed: tokens` | 2 |

- "Closing report claims verified": **2 of 52** failed runs (`model-extraction-relu-logits`,
  10 requests; `dna-insert-rep3`, 25 requests). Both messages state the result was verified;
  both fail the single verifier test. Read by hand, not by regex.
- 47 of 52 failures never reach a closing message, so a better completion check cannot help
  them (matches the closed completion-checklist result).

## Tool feedback (measured)

After the 2026-11-13 fixes (13 runs, 416 `exec` calls) every remaining error class carries a
readable message: missing executable 17, Python traceback 13, `[exit N]` 6, "exec has no
shell" 5, plus 71 allowlist denials (`--exec-policy allowlist`). Pre-fix traces add
`timeout after 300s` (8).

- 38 post-fix `exec` calls return empty output at zero exit (`find` 18, `ls` 6, `python3` 6,
  `g++` 3, `cp` 3, `mkdir` 1, `git` 1); plausible by command, individual calls unread.
- 75 of 79 `exec` results at the 8,000-character cap carry the harness omission marker.
- No new silent-failure class found, but this is a census of result strings, not a reading of
  every call.

## What fills the input (estimate)

From event sizes at 4 characters per token. Covers **70% of billed input**; the unexplained
30% exceeds any row, so these are shares of the estimate, not the bill.

| Part | Share of the estimate |
|---|---|
| System message and tool definitions (first-request size) | 27% |
| Tool-call arguments, never trimmed | 24% |
| Echoed reasoning (last 2 kept; from `reasoning_tokens`, not read off the wire) | 21% |
| Live tool results (last 5) | 19% |
| Collapsed result stubs | 9% |

- `request.rs` passes `tool_calls` through whole; only results and reasoning are trimmed.
- 502 of 518 `write` calls write a script (`.py`, `.sh`, ...): 587k characters, re-sent on
  every later request. Stubbing arguments older than 5 tool calls (keep 120 characters)
  saves a **median 5% of billed input** per run, 10% pooled, about one more request per run;
  30–43% on `sam-cell-seg`. `exec` arguments: 0.3%.

## Round-trip cost (measured)

- **472 of 2,330 requests (20.3%)** are one tool call writing a script not named in the goal
  (11 more write a goal-named script). Of 502 script writes, 436 first run in a later
  request, 0 in the same (median gap 1).
- The prompt asks for it: `GOAL_TAIL` (`crates/rof/src/eval_agent.rs:23`) says exec has no
  shell and stdin is closed, "write scripts to files and run them when you need
  composition"; the workflow contract adds "never write files via exec".
- A reachable shell does not remove it: 12 runs used `sh -c` and show lone script writes at
  15.6% of requests against 21.5% in the other 50.
- `--shell on` only adds a `shell` tool; `GOAL_TAIL` and the contract are unchanged, so the
  existing shell arm probably would not change this (inferred, not run).
- Upper bound if each lone script write merged with its run: 472 requests at a mean 9,756
  input tokens, about 20% of input, 6 requests in a 30-request run (inferred).

## Cache dollars (measured bound)

At PEAK rates the 62 runs cost $5.37 ($2.68 misses, $0.08 hits, $2.61 output). Perfect
prefix caching saves at most the miss half, about $0.04 per run at PEAK. Inferred, not
replayed: `--collapse-hysteresis` cannot reach that bound, and it keeps more results whole,
so input per request rises and a `Gross`-budget run gets fewer requests. No replay built:
the saving is bounded at half the bill and trades against requests.

## Conclusions

- Closed: false verification (2 of 52) does not explain the misses, and the census found no
  other silent-feedback class.
- Low headroom: stubbing about 10% pooled. Collapsing whole finished scopes (Spine, arXiv
  2610.37224) was **not** estimated.
- The round trip is **efficiency**, not a capability lead: merging frees at most 20% of the
  budget, and the envelope arm already gave 3.3x tokens and converted 0 of 11 misses. It
  would lower tokens per solved task; no evidence it flips a miss. No paid arm proposed.
- If fixed anyway: the fix is the prompt and the exec surface (one call that writes and
  runs), which changes the baseline recipe; hold `--exec-policy` fixed on both sides.
- Unexplained: what a token-death run does wrong in its first requests. The sweep's
  trajectory study (arXiv 2607.09510) puts the decisive error at a median of step 7; a hand
  reading of the stable misses against their oracle solutions is the free next step.
