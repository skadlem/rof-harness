# Planner Removal + Rethink Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the planner stage; every goal runs as one task. Record what (if anything) replaces decomposition.

**Architecture:** Cut the planner call + its prompt scaffolding from the orchestrator; tasks = [goal] always. Remove the config knob and env lever. Keep `goal_is_task_shaped` as the documented router primitive for the rethink. Rethink decision (locked below): no replacement decomposition — multi-step work is multiple goals.

**Tech Stack:** Rust only.

## Global Constraints

- `cargo fmt` before every commit; full `cargo test` green twice before finishing.
- No prompt-shape churn beyond deleting the planner: `plan_json` becomes the canned single-task plan so implementer prompts stay stable.
- No dead code left behind (pub items that lose all callers get removed too, except the one primitive named below).

## Rethink (decided, not deferred)

Evidence: planner=skip beat planning (+4 tasks, 2.5× fewer tokens); the multi-task path never earned its keep live. Decomposition, when needed, lives OUTSIDE the run: the TUI queues goals, eval suites list tasks. `goal_is_task_shaped` stays as the single classifier if we ever auto-split again — everything else planner goes.

---

### Task B1: Remove the planner stage

**Files:**
- Modify: `src/engine/orchestrator.rs` (delete planner block, reuse gate, head/index prep tied to planner)
- Modify: `src/config/mod.rs` (remove `planner` field + Default entry)
- Modify: `src/main.rs` (remove ROF_PLANNER handling)
- Modify: `src/eval/goal_quality.rs` (remove `goal_is_task_shaped`? NO — keep, re-document as router primitive; remove nothing else)
- Modify: `configs/default.json` (remove `"planner": "always"`), `configs/cheap.json`, `configs/strict.json` if present
- Test: `tests/loop.rs` (remove/convert planner-mode tests; keep planner_calls==0 assertions where still meaningful)

**Interfaces:** None produced. Deleted: `cfg.planner`, ROF_PLANNER, planner call branch.

- [ ] **Step 1: Failing tests first — convert, don't just delete.** In `tests/loop.rs`:
  - Delete `planner_auto_skips_task_shaped_goals` + `planner_auto_plans_vague_goals` (the mode is gone).
  - Add: `no_planner_stage_runs` — run_loop with default cfg asserts `client.planner_calls == 0` AND `out["plan"]["skipped"] == true` (the canned single-task plan shape is the contract now).
  - `two_tasks` Fake constructor + any test relying on planner-emitted multi-task plans: convert to single-task (planner never runs; `plan_tasks` field stays for the Fake's compat but only `["t1"]` shapes remain — delete `two_tasks()` if its test goes).
  - Run: FAIL (canned plan has no `skipped` marker yet / planner still runs).
- [ ] **Step 2: Implement.** In `run_loop`: replace the whole planner section (head/index/reuse/decision/plan_out) with the canned plan:

```rust
// No planner: every goal is one task. Decomposition lives outside the
// run (TUI goal queue, eval suite task lists) — the measured win was
// skipping, so the stage is gone, not defaulted off.
let plan_out = crate::agents::AgentOutput {
    summary: "no planner".to_string(),
    data: serde_json::json!({ "tasks": [], "acceptance": [] }),
};
```

  Keep `plan_json`/`plan_state` downstream untouched (tasks=[goal] fallback already handles empty plans — verify this path, don't duplicate it). Delete `planner_head`, `planner_skills`, `planner_reuse`, the `skip_planner` block and its trace event. Remove `PlannerAgent` import if unused elsewhere in the file (check direct loop — it never planned). Remove `planner` from AppConfig + Default + ROF_PLANNER + configs/*.json. Keep `PlannerAgent` struct? It becomes dead pub code — the lib exports agents; pub items don't warn. Decision: DELETE `PlannerAgent` + `Plan` from `src/agents/planner.rs`... wait, check other users first (`Plan` used in специалистами? grep). If anything else uses them, keep; else delete the file + mod entry. Also delete the `planner` grant in PermissionPolicy::default agents_tools (check default.json's explicit list too — it overrides, leave files that state it? default.json lists planner grants; harmless but stale — remove for honesty).
- [ ] **Step 3: `cargo test --test loop`** → PASS. Full suite → PASS. Commit.

```bash
git add src/engine/orchestrator.rs src/config/mod.rs src/main.rs src/agents/ configs/ tests/loop.rs
git commit -m "feat!: remove the planner stage (single-task runs)"
```

Note the `!`: config files with `"planner"` still load (serde ignores unknown fields — verify with a quick load test, add one if missing).

## Self-Review

- Coverage: stage, knob, env, configs, grants, tests.
- `goal_quality.rs` keeps `goal_is_task_shaped` + its tests (router primitive, documented as such in its doc comment — update the comment).
- Type consistency: plan_json shape unchanged downstream.

## Execution

Inline, single task. STATUS entry on landing.
