# Budget-line A/B yielded one solve in four treatment runs and zero in four controls

Two repetitions each of `query-optimize` and `raman-fitting` on the banked TB2.1
Slice A recipe. Both tasks passed the earlier oracle preflight and still hit the
token cap after the exec fixes. The three tasks used for the budget-pacing diagnosis were excluded because their oracle solutions had been read.

Both arms used `rof eval --agent --in-container` at `97075cf`, DeepSeek
`deepseek-flash`, 300k gross tokens, 60 steps, 120 actions, an 8,000-token
response cap, and the [faithful run's allowlist](https://github.com/skadlem/rof-harness/blob/knowledge-vault-trial/docs/experiments/2026-11-11-slice-a-faithful.md).
The runner checked binary revision, banked task and image identities, matching
configuration except for `budget_requests`, and native verifier reward files.
Control ran first in repetition 1 and treatment ran first in repetition 2.

| Task | Rep | Legacy reward | Requests reward | Legacy gross tokens | Requests gross tokens |
|---|---:|---:|---:|---:|---:|
| query-optimize | 1 | 0 | 1 | 316,916 | 303,832 |
| query-optimize | 2 | 0 | 0 | 301,127 | 305,347 |
| raman-fitting | 1 | 0 | 0 | 304,196 | 317,814 |
| raman-fitting | 2 | 0 | 0 | 323,308 | 309,229 |
| **Total** | | **0** | **1** | **1,245,547** | **1,236,222** |

All eight rows halted on tokens. The treatment's one pass did not repeat.
Treatment spent 1,236,222 gross tokens per solve. The control had zero solves,
so the 1.3× relative guardrail is undefined. Keep the flag opt-in. Any further
test should use untouched tasks, including tasks the control can solve, to
measure preservation as well as rescue.

Reports, reward files, runner, and manifest are local under
`target/budget-line-ab-20261123/`. Binary SHA-256 is
`5c416117d2a041343cd8d3cf4840f3e861180063c74a398581ea74e854657507`.
The eight reports total $0.667861 at the harness price-book rates. The provider
account's CNY balance moved from ¥183.19 to ¥179.58 over the run window.
