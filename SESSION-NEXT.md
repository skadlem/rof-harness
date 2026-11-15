# Session state (updated 2026-11-15)

Short entry point for a new session. It points at the notes that hold the detail; read
those only when the task needs them. `AGENTS.md` rules apply throughout.

## State

- Branch `knowledge-vault-trial`. Commits after `6eaefaa` are not pushed; push only when
  the owner asks.
- Slice A baseline is 5/18 at the frozen recipe (300k gross tokens, 60 steps, 120
  actions, DeepSeek flash). Failures are token deaths: 47 of 52 failed runs end
  `Failed: tokens`.
- The three stable misses (`dna-insert`, `dna-assembly`, `make-mips-interpreter`) are
  pacing failures, not knowledge gaps. The model paces by the step counter and misreads
  the `N/M` token figure.
- `--budget-line requests` is built, off by default and **unmeasured**. No live model run
  has used it. It works in `rof run`, `rof eval` and the Harbor adapter.
- Paid runs are stopped. The handoff doc recorded about $0.36 of DeepSeek balance; re-read
  the live balance before any spend.

## Decisions

- Agent tree (`--agents`) is demoted: no paper shows subagents lifting a weak model at
  matched tokens. It stays opt-in and unwired from `rof eval`.
- Collapse hysteresis replay was not built; argument stubbing has about 10% pooled
  headroom and is not built either.
- Do not write "the model is the ceiling". The owner rates DeepSeek V4.1 Flash 1017 as a
  strong model; the notes blame pacing and harness behaviour instead.
- The three diagnosed tasks are contaminated (their oracle solutions were read). They
  cannot validate a fix.

## Next task

Run the budget-line arm, once the owner tops up and agrees the task list:

1. Pick Slice A token-death tasks other than the three contaminated ones.
2. Run control and `--budget-line requests` on the same binary through
   `rof eval --agent --in-container` (pattern: `tools/deliverable_ab1.sh`). The binary is
   `target/release/rof`; rebuild it if the tree has moved and check the report's
   `binary_git_sha` has no `-dirty` suffix.
3. Confirm `provenance.run_config.budget_requests` differs between the two reports.
4. Judge on solves and on tokens per solved task (guardrail 1.3x).

Unbuilt, needing design agreement with the owner before any code:

- Rollback notice only when something was actually reverted, and no revert of a batch's
  writes on a plain `exec` failure. It fires on 21% of tool batches today.
- Allowing `write` and `exec` in one message. This depends on the rollback change.

Offered and not requested: README rewrite; moving evidence files out of `target/`.

## Where to look

| Need | Read |
| --- | --- |
| Map of every note and its status | `docs/INDEX.md` |
| Active and deferred work, paid-campaign stop | `docs/research-next-steps.md` ("Latest state") |
| Why the misses fail; budget-line design and replay table | `docs/experiments/2026-11-15-miss-diagnosis.md` |
| Trace census, token shares, script round trip | `docs/experiments/2026-11-14-failed-trace-audit.md` |
| Per-run detail on each diagnosed task | `research/miss-diagnosis-2026-11-15-<task>.md` (local only) |
| Paper sweep | `research/lit-sweep-2026-11-14.md` (local only) |
| Experimental flags and adapter options | `docs/harness-experiments.md` |
| Budget line code | `budget_line` in `crates/agent-loop/src/request.rs` |
| Batch rollback code | `batch_failed` in `crates/agent-loop/src/run.rs`; `ROLLBACK_NOTICE` in `proof.rs` |
| Eval prompt tail | `GOAL_TAIL` in `crates/rof/src/eval_agent.rs` |

Outside the repo, under `~/.local/share/rof-tb/`:

- Banked traces: `evidence/slice-a-2026-11-12`, `work/deliverable-ab1`,
  `work/variance-reps-2026-11-13`, `work/slice-a-execfix-2026-11-13`. Each run has
  `events.jsonl`, `work/.rof-events.jsonl` (with reasoning text) and a verifier reward.
- Analysis scripts: `tools/audit-2026-11-14/` (`census.py`, `audit2.py`–`audit10.py`,
  `replay_budget.py`). They read a `runs.pkl` built by `census.py`.
- Task definitions and oracle solutions: `datasets/tb21-dataset/terminal-bench-2-1/`.
  Reading a task's `solution/` contaminates it; record that if you do.

## Gotchas

- A pre-commit hook fails unless `docs/INDEX.md` lists every note under `docs/` and
  `research/`, including local-only research files.
- The Harbor adapter tests need Harbor's Python:
  `cd tools && ~/.local/share/uv/tools/harbor/bin/python -m unittest test_harbor_rof`.
- The papers MCP does not expand `~` in `save_path`; pass an absolute path.
- `research/` is ignored by git and is read-only input, apart from adding new reports.
