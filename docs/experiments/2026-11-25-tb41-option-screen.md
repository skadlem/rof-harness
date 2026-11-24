# TB4.1 CPU option screen — 0/48 solved across all four arms at 300k tokens; mechanisms verified, defaults unchanged (2026-11-25)

Plan: [2026-11-24-tb41-option-screen](../superpowers/plans/2026-11-24-tb41-option-screen.md).
Raw jobs and ledger: `target/tb41-option-screen-20261125/` (`ledger.jsonl`, `summary-48.json`).

## Provenance

- **Binary:** clean detached build at `5159e17` (`target/option-screen-runtime/rof`), SHA-256 `5947b121322700c070c72c1c5984585d1f588fc345153355c2231aa5375cddc6`, Harbor version `runtime-bundle-5947b121322700c0` on all 48/48 runs.
- **Tasks:** four TB 4.1.0 CPU tasks pinned at `4070d6cef56a7d20cb5dc98d5e10ada88a6b9440` (`bun-sourcemap-leak`, `heat-pump-warranty`, `music-harmony`, `ontology-kg-querying`), all source digests verified before launch.
- **Control recipe:** `deepseek-flash` via `https://api.deepseek.com/v1`, 300,000 gross tokens / 60 steps / 120 actions / `max_tokens: 8000`, `--exec-policy any`, `--steer off`, `--streak-steer off`, `--keep-thinking`, observation/thinking keep 5/2, `workflow: "legacy"`, `exec_output: "legacy"`, `rollback_scope: "all"`.
- **Execution:** 4 tasks × 4 arms × 3 reps = 48 serial trials, arm order rotated per repetition (`r1`: control/output/rollback/response; `r2`: output/rollback/response/control; `r3`: rollback/response/control/output).
- **Pre-run infra incident ($0.00 spent):** initial launch hit two `AgentSetupTimeoutError`s (360 s) on `bun-sourcemap-leak` because container `apt-get install -y git` pulled 78 packages (including recommends) over ~333 kB/s tunnel egress. Archived to `ledger-infra-20261125-bun-timeout.jsonl`; fixed in `dd4f8a6` (`--no-install-recommends`, 78 → 49 packages) + 900 s setup timeout, after which all 48/48 runs completed with **0 infra errors** and **0 `usage_unknown` rows**.
- **Spend:** exact 48-run peak price-book total **$3.6813** (`$3.7510` including the `$0.0697` check trial, vs `$8.00` hard stop). Live `.env.deepseek` USD balance moved **`$5.36 → $5.33`** (check trial, `$0.03`) and **`$5.32 → $3.51`** (48 runs, **`$1.81`** actual billed).

## Results (measured)

All 48 runs exited `3` with native reward `0.0` (0/48 solved; tokens-per-solved-task guardrail undefined under zero control solves). 47 of 48 halted on the 300k gross token cap (`Failed("tokens")`); 1 of 48 (`r3-music-harmony-response`) halted on output truncation at `32,768` tokens.

| Arm | Option change | Solves | Mean gross tok | Mean turns | Mean wall | Peak USD (12 runs) | Rollback notices | Ladder retries (`attempts>1`) | `[exit N]` first lines |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `control` | none | 0/12 | 309,785 | 29.2 | 400.9 s | $0.9251 | 36 | 7 | 1 |
| `output` | `exec_output: "head-tail"` | 0/12 | 305,526 | 31.1 | 331.3 s | $0.9081 | 45 | 7 | 22 |
| `rollback` | `rollback_scope: "errors"` | 0/12 | 304,653 | 28.9 | 312.8 s | $0.9305 | 1 | 7 | 0 |
| `response` | `max_tokens: 32768` | 0/12 | 295,601 | 26.8 | 325.3 s | $0.9176 | 34 | 0 | 0 |

Per-task totals across all 4 arms (12 runs each, 0/12 solved on every task):

| Task | Expert est. | Mean turns | Mean output tok | Mean reasoning tok | Ladder retries | Peak USD (12 runs) |
|---|---:|---:|---:|---:|---:|---:|
| `bun-sourcemap-leak` | 1.5 h | 29.9 | 46,166 | 39,224 | 14 | $1.1034 |
| `heat-pump-warranty` | 4.0 h | 32.7 | 6,430 | 2,734 | 0 | $0.7617 |
| `music-harmony` | 1.0 h | 25.6 | 28,923 | 22,800 | 7 | $1.0355 |
| `ontology-kg-querying` | 20.0 h | 27.8 | 8,221 | 3,834 | 0 | $0.7806 |

## Mechanism checks & engineering findings

1. **`output` (`exec_output: "head-tail"`):** mechanism active — 22 `[exit N]`-first results vs 1 in control (29 capped execs; 0 command timeouts across all 48 runs, so partial-on-timeout was not triggered). No solve conversion at 300k tokens.
2. **`rollback` (`rollback_scope: "errors"`):** mechanism active — cut rollback notices from **36 / 45 / 34** (in control / output / response) down to **1** across 41 failed batches (**97.6% reduction** in false notices on read/exec-only batches). Only 2 of 156 failed batches across the entire 48-run screen even contained a successful write/edit. No solve conversion at 300k tokens.
3. **`response` (`max_tokens: 32768`):** mechanism active — eliminated all **21** 8k→16k/32k ladder retries seen across the three 8k arms (`attempts=1` on all 321 assistant turns vs 7 retries per 8k arm, all on `bun-sourcemap-leak` and `music-harmony`). However, on `r3-music-harmony-response` turn 14, the model spent all `32,768` tokens on reasoning (`content=0`, 90,647 chars) and halted immediately because `MAX_TOKENS_CEILING = 32_768` + Harbor's `--keep-thinking` leaves zero overflow ladder rungs (`crates/provider-openai/src/retry.rs:36`).
4. **Harbor adapter JSONL bug caught and fixed:** on 6 of 12 `music-harmony` trials, PDF text inside `events.jsonl` contained Unicode line separators (`\u2028`/`\u2029`), causing Python's `str.splitlines()` in `tools/harbor_rof.py:271` to split valid JSON lines and silently drop `n_input_tokens` / `cost_usd` to `null` in Harbor's `result.json` (all 6 rows had complete, valid `TurnEnd` usage when parsed with `.split("\n")`). Fixed in `tools/harbor_rof.py` and `tools/rof_candidates.py` with a regression test.

## Decision

- **No default changes:** all three options (`exec_output`, `rollback_scope`, `max_tokens: 32768`) remain opt-in; 0/48 solves establishes no capability lift on this slice.
- **Root limitation (measured):** 47 of 48 runs halted on the **300k gross token budget** at 25–33 turns (~280k input tokens = cumulative context re-reads) on 1–20 hour expert TB4.1 tasks, where the TB4.1 pilot previously used 1,200,000 tokens / 120 steps / 240 actions. Screening harness options on a 0/12 floor gives no signal; future screens need either easier TB4.1 CPU tasks with non-zero control solve rate or the 1.2M pilot envelope on a smaller subset.
