# Arch-review items 1–6 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement arch-review memo items 1–6 in rof-harness with byte-identical defaults.

**Architecture:** Seven small, independent tasks (item 2 splits in two). Each touches one subsystem, ships with red-green tests, and keeps every default run bit-for-bit identical. No new dependencies. Pure fns where possible; wiring only where the loop demands it.

**Tech Stack:** Rust (edition in Cargo.toml), serde/serde_json, tokio, existing FakeClient/StubClient test harnesses.

## Global Constraints

- `cargo fmt` outranks any code block below; run `cargo fmt` before every commit.
- Never `push_str(&format!(...))` multiline — bind `let s = format!(...);` first (rustc delimiter quirk).
- Tests must run offline (no network, no model). Use FakeClient/StubClient/`true` shell commands only.
- Every new config/suite field gets `#[serde(default)]` so old files load unchanged.
- Parallel-test safety: no new process-global captures; follow the existing mutex pattern if unavoidable.
- Commit per task after green. Do not run rof with the repo as cwd.

---

### Task 1: Planner auto-skip heuristic

**Files:**
- Modify: `src/eval/goal_quality.rs` (add `goal_is_task_shaped` + tests)
- Modify: `src/engine/orchestrator.rs` (planner decision, ~lines 144–190)
- Modify: `src/main.rs` (`ROF_PLANNER` allowlist)

**Interfaces:**
- Consumes: existing `has_anchor(goal: &str) -> bool` (private, same module).
- Produces: `pub fn goal_is_task_shaped(goal: &str) -> bool`.

- [ ] **Step 1: Write the failing tests** (append to `mod tests` in `src/eval/goal_quality.rs`):

```rust
#[test]
fn task_shaped_goals_skip_the_planner() {
    assert!(goal_is_task_shaped("Fix the login redirect in src/auth.rs"));
    assert!(goal_is_task_shaped(
        "Add retry with backoff to src/llm/openrouter.rs"
    ));
}

#[test]
fn vague_or_anchorfLess_goals_still_plan() {
    assert!(!goal_is_task_shaped("Review the architecture of rof"));
    assert!(!goal_is_task_shaped("fix it"));
    assert!(!goal_is_task_shaped("How does the retriever work?"));
    assert!(!goal_is_task_shaped("Verify the build is green"));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib goal_is_task_shaped`
Expected: FAIL with "cannot find function `goal_is_task_shaped`"

- [ ] **Step 3: Write minimal implementation** (in `src/eval/goal_quality.rs`, next to `has_anchor`):

```rust
/// Deterministic planner-skip heuristic ("auto" mode): skip planning when the
/// goal is already task-shaped — it names a file/symbol anchor AND opens with
/// an imperative code-action verb. Conservative by construction: no anchor or
/// no verb means plan, so ambiguity always costs one planner call, never a
/// missing plan.
const TASK_VERBS: [&str; 19] = [
    "fix", "add", "remove", "refactor", "implement", "update", "change", "create",
    "delete", "move", "rename", "extract", "replace", "migrate", "bump", "wire",
    "hoist", "collapse", "split",
];

pub fn goal_is_task_shaped(goal: &str) -> bool {
    let first = goal.split_whitespace().next().unwrap_or("");
    let verb = first
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_ascii_lowercase();
    TASK_VERBS.contains(&verb.as_str()) && has_anchor(goal)
}
```

- [ ] **Step 4: Wire the orchestrator.** Replace both `self.cfg.planner == "skip"` uses (planner_reuse gate and plan_out branch) with one computed decision placed before the reuse gate:

```rust
let skip_planner = self.cfg.planner == "skip"
    || (self.cfg.planner == "auto"
        && crate::eval::goal_quality::goal_is_task_shaped(&session.goal));
if self.cfg.planner == "auto" {
    self.trace.emit(TraceEvent::StateTransition {
        from: "planner_auto".to_string(),
        to: if skip_planner { "skip".to_string() } else { "plan".to_string() },
    });
}
```

And in the skip branch, mark auto decisions in the data (traceable, not prompt noise for static skip):

```rust
let plan_out = if skip_planner {
    let mut data = serde_json::json!({ "tasks": [], "acceptance": [], "skipped": true });
    if self.cfg.planner == "auto" {
        data["auto"] = serde_json::json!(true);
    }
    crate::agents::AgentOutput {
        summary: "planner skipped".to_string(),
        data,
    }
} else { ... unchanged ... };
```

Check `AgentOutput` field names against `src/agents/mod.rs` before writing (summary/data used at line ~169 — verify).

- [ ] **Step 5: Accept `auto` in env.** In `src/main.rs`, change the `ROF_PLANNER` guard to `matches!(p.trim(), "skip" | "always" | "auto")`.

- [ ] **Step 6: Run tests**

Run: `cargo test --lib goal_quality && cargo test --test loop`
Expected: PASS, no regressions.

- [ ] **Step 7: Commit**

```bash
git add src/eval/goal_quality.rs src/engine/orchestrator.rs src/main.rs
git commit -m "feat: planner auto-skip heuristic (conservative, logged)"
```

---

### Task 2a: Fail-to-pass / pass-to-pass oracle split

**Files:**
- Modify: `src/eval/suite.rs` (task fields + `oracle_ok` pure fn + tests)
- Modify: `src/eval/runner.rs` (baseline run, verdict fold, result fields)
- Test: `tests/eval_suite.rs` (parse test for new fields)

**Interfaces:**
- Consumes: `RoundServices::run_checks` (`src/engine/session.rs`), `CheckResult`.
- Produces: `pub fn oracle_ok(baseline: &[CheckResult], finals: &[CheckResult], fail_to_pass: &[String], pass_to_pass: &[String]) -> bool` in `src/eval/suite.rs`.

- [ ] **Step 1: Write the failing tests** (new `#[cfg(test)] mod` in `src/eval/suite.rs`):

```rust
use super::{oracle_ok, EvalTask};
use crate::engine::CheckResult;

fn cr(name: &str, passed: bool) -> CheckResult {
    CheckResult { name: name.to_string(), passed, output: String::new() }
}

#[test]
fn f2p_needs_a_baseline_fail_and_a_final_pass() {
    let base = vec![cr("cargo test foo", false)];
    let fin = vec![cr("cargo test foo", true)];
    assert!(oracle_ok(&base, &fin, &["cargo test foo".to_string()], &[]));
    assert!(!oracle_ok(&fin, &fin, &["cargo test foo".to_string()], &[]));
}

#[test]
fn p2p_needs_pass_on_both_sides() {
    let base = vec![cr("cargo test bar", true)];
    let fin = vec![cr("cargo test bar", true)];
    assert!(oracle_ok(&base, &fin, &[], &["cargo test bar".to_string()]));
    assert!(!oracle_ok(&base, &vec![cr("cargo test bar", false)], &[], &["cargo test bar".to_string()]));
}

#[test]
fn a_missing_final_entry_fails_the_oracle() {
    let base = vec![cr("cargo test foo", false)];
    assert!(!oracle_ok(&base, &[], &["cargo test foo".to_string()], &[]));
}

#[test]
fn no_oracle_fields_is_vacuously_ok() {
    assert!(oracle_ok(&[], &[], &[], &[]));
}

#[test]
fn new_task_fields_default_empty() {
    let t: EvalTask = serde_json::from_str(r#"{"name":"x","goal":"y"}"#).unwrap();
    assert!(t.fail_to_pass.is_empty() && t.pass_to_pass.is_empty());
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval::suite`
Expected: FAIL, missing items.

- [ ] **Step 3: Minimal implementation** in `src/eval/suite.rs`:

```rust
pub struct EvalTask {
    pub name: String,
    pub goal: String,
    #[serde(default = "pass")]
    pub expect_pass: bool,
    #[serde(default)]
    pub checks: Vec<String>,
    /// Fail-to-pass oracle: each must fail on the pristine copy and pass after.
    #[serde(default)]
    pub fail_to_pass: Vec<String>,
    /// Pass-to-pass oracle: each must pass on the pristine copy and still pass after.
    #[serde(default)]
    pub pass_to_pass: Vec<String>,
    ... rest unchanged ...
}

/// Pure oracle fold: F2P entries need baseline-fail + final-pass, P2P entries
/// need pass on both sides, matched by exact command string. A declared check
/// with no final entry fails (unproven is not passed). Empty lists are
/// vacuously ok, so tasks that never heard of the split score exactly as before.
pub fn oracle_ok(
    baseline: &[CheckResult],
    finals: &[CheckResult],
    fail_to_pass: &[String],
    pass_to_pass: &[String],
) -> bool {
    let outcome = |name: &str, list: &[CheckResult]| {
        list.iter().find(|c| c.name == name).map(|c| c.passed)
    };
    for name in fail_to_pass {
        if outcome(name, baseline) != Some(false) || outcome(name, finals) != Some(true) {
            return false;
        }
    }
    for name in pass_to_pass {
        if outcome(name, baseline) != Some(true) || outcome(name, finals) != Some(true) {
            return false;
        }
    }
    true
}
```

Check `CheckResult` import path in `suite.rs` (currently imports only serde) — add `use crate::engine::CheckResult;` after verifying the re-export in `src/engine/mod.rs`.

- [ ] **Step 4: Wire `run_task_in`.** Add to `TaskResult`: `#[serde(default)] pub check_baseline: Vec<CheckResult>` and `#[serde(default)] pub oracle_ok: Option<bool>`. In `run_task_in`, after `reg` is built and BEFORE `orch.run_loop`:

```rust
let oracle_names: Vec<String> = task
    .fail_to_pass
    .iter()
    .chain(task.pass_to_pass.iter())
    .cloned()
    .collect();
let baseline = if oracle_names.is_empty() {
    Vec::new()
} else {
    let probe = Session::new(task.goal.clone()).with_checks(oracle_names);
    let svc0 = crate::engine::session::RoundServices {
        cfg: &cfg,
        trace: &sink,
        context: &self.context,
        executor: &self.executor,
        verify: &self.verify,
        tools: &reg,
    };
    svc0.run_checks(&probe, &workdir).await
};
```

After the loop, where `passed` is computed from `out["passed"]`:

```rust
let oracle = if task.fail_to_pass.is_empty() && task.pass_to_pass.is_empty() {
    None
} else {
    Some(super::suite::oracle_ok(&baseline, &checks, &task.fail_to_pass, &task.pass_to_pass))
};
let passed = out["passed"].as_bool().unwrap_or(false) && oracle.unwrap_or(true);
```

Store `check_baseline: baseline, oracle_ok: oracle` in both `TaskResult` constructions in `run_task_in`/`run_task_isolated` (the isolated error-early path gets `check_baseline: Vec::new(), oracle_ok: None`).

Verify `RoundServices` field visibility (all `pub` — confirmed in session.rs) and the `run_checks` signature (`&self, session: &Session, workdir: &Path` — confirmed).

- [ ] **Step 5: Run tests**

Run: `cargo test --lib eval && cargo test --test eval_suite`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/eval/suite.rs src/eval/runner.rs
git commit -m "feat: fail-to-pass/pass-to-pass oracle split (compat default)"
```

---

### Task 2b: `--reps N` eval replication

**Files:**
- Modify: `src/eval/runner.rs` (`run_suite_with_reps`, aggregation, result fields)
- Modify: `src/main.rs` (`reps_arg`, `ROF_REPS`, wiring, print line)
- Test: `tests/eval_suite.rs` (reps=1 equivalence + reps=2 aggregation on the fake)

**Interfaces:**
- Consumes: `run_task_isolated`, `TaskResult` (+ `reps`, `passes` fields).
- Produces: `pub async fn run_suite_with_reps(&self, suite: &EvalSuite, jobs: usize, reps: usize) -> SuiteReport`.

- [ ] **Step 1: Write the failing tests** in `tests/eval_suite.rs` (copy the file's existing fake-runner setup; inspect it first):

```rust
#[tokio::test]
async fn reps_one_matches_single_run() {
    let (runner, suite) = fake_setup();
    let a = runner.run_suite_with_jobs(&suite, 1).await;
    let b = runner.run_suite_with_reps(&suite, 1, 1).await;
    assert_eq!(a.tasks.len(), b.tasks.len());
    for (x, y) in a.tasks.iter().zip(b.tasks.iter()) {
        assert_eq!(x.matched, y.matched);
        assert_eq!(y.reps, 1);
        assert_eq!(y.passes, if y.passed { 1 } else { 0 });
    }
}
```

(a second test with reps=2 asserting `reps == 2`, `passes <= 2`, and `matched == ((passes == 2) == expected)` follows the same shape — write it against the actual fake_setup signature.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --test eval_suite reps_one`
Expected: FAIL, no method `run_suite_with_reps`, no fields `reps`/`passes`.

- [ ] **Step 3: Minimal implementation.** Add fields to `TaskResult`:

```rust
/// Replication count this row aggregates (0 = report predates reps).
#[serde(default)]
pub reps: u32,
/// Reps that passed (0 = report predates reps).
#[serde(default)]
pub passes: u32,
```

Add a private helper in `runner.rs`:

```rust
async fn run_task_reps(&self, task: EvalTask, reps: usize) -> TaskResult {
    let reps = reps.max(1);
    let mut first: Option<TaskResult> = None;
    let mut passes = 0u32;
    let mut rounds = 0u32;
    for _ in 0..reps {
        let r = self.run_task_isolated(task.clone()).await;
        if r.passed {
            passes += 1;
        }
        rounds += r.rounds;
        if first.is_none() {
            first = Some(r);
        }
        // Record every rep at rep level so success_rate is a rep rate.
        self.trace_record_rep(r.passed);
    }
    ...
}
```

Note: `EvaluationRunner.report()` folds `self.trace`, but per-task runs use forked sinks extended back — check how `report()` sees task events (`self.trace.extend(sink.events())` in run_task_in — confirmed). For rep-level aggregate counting, the existing code calls `rep.aggregate.record_task(task.passed)` per task row. With reps, call `record_task` once per rep instead of once per row: in the reps paths, replace the per-row `record_task(task.passed)` with a per-rep loop over `r.passes`. Implement inside `run_suite_with_reps` by counting: after building each aggregated row with `passes`, call `rep.aggregate.record_task(true)` passes-times and `record_task(false)` (reps-passes)-times. Simplest: loop `for _ in 0..passes { record_task(true) }` etc. Keep `run_suite_with_jobs` calling `run_suite_with_reps(suite, jobs, 1)` so behavior is shared, and set row fields `reps = 1, passes = passed as u32` there too.

`matched` for reps>1: `(passes == reps as u32) == task.expect_pass`. For reps==1 keep `passed == expected` (identical).

Detail row: keep the FIRST rep's feedback/checks/context/baseline (documented in the fn comment); `rounds` = sum across reps.

- [ ] **Step 4: Wire CLI.** Add `reps_arg` next to `jobs_arg` in `main.rs`, `ROF_REPS` in `apply_env` (a new `cfg` field is NOT needed — pass straight through: `let reps = reps_arg(...).or(env).unwrap_or(1).max(1)`), call `run_suite_with_reps(&suite, jobs, reps)`, extend the suite print line with `reps={reps}`.

- [ ] **Step 5: Run tests**

Run: `cargo test --test eval_suite && cargo test --lib eval`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/eval/runner.rs src/main.rs tests/eval_suite.rs
git commit -m "feat: eval --reps N replication (default 1, byte-identical)"
```

---

### Task 3: Endpoint capability profile

**Files:**
- Create: `src/llm/profile.rs` (types + tests)
- Modify: `src/llm/mod.rs` (re-export)
- Modify: `src/llm/openrouter.rs` (profile field, ladder walk)
- Modify: `src/config/mod.rs` (`AppConfig.endpoint`)
- Modify: `src/engine/session.rs` (profile-aware budget)
- Modify: `src/main.rs` (`ROF_EMISSION_THRESHOLD`, `with_profile` wiring)

**Interfaces:**
- Consumes: `LlmReq` flags.
- Produces: `pub struct EndpointProfile`, `pub enum LadderRung`, `pub fn reshape_with_profile(req: &mut LlmReq, content_chars: usize, profile: &EndpointProfile) -> bool` (re-export for tests via `*_for_test` like the existing ladder helpers).

- [ ] **Step 1: Write the failing tests** (in `src/llm/profile.rs`):

```rust
use super::{EndpointProfile, LadderRung};

#[test]
fn default_profile_reproduces_the_shipped_ladder_order() {
    assert_eq!(
        EndpointProfile::default().ladder,
        vec![
            LadderRung::ReasoningLow,
            LadderRung::ThinkingOff,
            LadderRung::ReasoningOff,
            LadderRung::Roomier,
            LadderRung::Shrink,
        ]
    );
    assert_eq!(EndpointProfile::default().emission_threshold_chars, 12_000);
}

#[test]
fn a_custom_order_is_honoured() {
    let p = EndpointProfile {
        emission_threshold_chars: 12_000,
        ladder: vec![LadderRung::ReasoningOff, LadderRung::ReasoningLow],
    };
    let mut req = crate::llm::LlmReq { ... blank ... };
    assert!(super::apply_ladder(&mut req, 100, &p));
    assert!(req.reasoning_off && !req.reasoning_low);
}

#[test]
fn an_empty_ladder_is_spent() {
    let p = EndpointProfile { emission_threshold_chars: 12_000, ladder: vec![] };
    let mut req = crate::llm::LlmReq { ... blank ... };
    assert!(!super::apply_ladder(&mut req, 100, &p));
}
```

(Construct the blank `LlmReq` exactly as `base_req_for_test` does — copy its fields.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib llm::profile`
Expected: FAIL, no module `profile`.

- [ ] **Step 3: Minimal implementation** (`src/llm/profile.rs`):

```rust
use serde::{Deserialize, Serialize};

/// One retry-ladder rung, in the vocabulary the request already speaks:
/// each rung sets a flag on `LlmReq` (roomier also doubles the budget,
/// shrink halves it). Kept distinct because endpoints honour different
/// knobs at different prompt sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LadderRung {
    ReasoningLow,
    ThinkingOff,
    ReasoningOff,
    Roomier,
    Shrink,
}

/// Per-endpoint capability: the prompt ceiling above which this endpoint
/// stops emitting content, and the order in which the retry ladder climbs.
/// Defaults reproduce the shipped DeepSeek/vLLM behavior bit for bit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EndpointProfile {
    pub emission_threshold_chars: usize,
    pub ladder: Vec<LadderRung>,
}

impl Default for EndpointProfile {
    fn default() -> Self {
        Self {
            emission_threshold_chars: 12_000,
            ladder: vec![
                LadderRung::ReasoningLow,
                LadderRung::ThinkingOff,
                LadderRung::ReasoningOff,
                LadderRung::Roomier,
                LadderRung::Shrink,
            ],
        }
    }
}
```

Then rewrite `reshape_for_truncation` in `openrouter.rs` as a walk over `profile.ladder`, preserving the exact guards (roomier: `!roomier && content>0`, doubling + flag reset; shrink: `content==0 && !shrunk && max_tokens>1024`, halving floor 1024, reasoning stays off). Keep `reshape_for_truncation(req, content)` delegating with `&EndpointProfile::default()`. Skip-if-set gives order-independence for custom profiles. Run the existing ladder tests (`v4_shrink.rs` et al.) — they must pass unchanged.

- [ ] **Step 4: Plumb the threshold.** In `session.rs`: keep `EMISSION_THRESHOLD` (documented measurement, now the default value) and `volatile_budget_for(tokens)` (delegates to the default profile); add `volatile_budget_for_profile(tokens, &EndpointProfile)`. `RoundServices::volatile_budget` and the `reviewer_file_evidence` cap use `self.cfg.endpoint`. Add `pub endpoint: EndpointProfile` to `AppConfig` with `#[serde(default)]`.

- [ ] **Step 5: Wire the client.** Add `profile: EndpointProfile` to `OpenRouterClient` (default in every constructor: `from_env`, `from_compat_env`, `from_verify_env`, `effort_client_for_test`, and any struct literal — grep first), a `with_profile` builder, and use `self.profile` in the `complete()` reshape call. In `main.rs::build_services`, apply `cfg.endpoint` to both real clients. Add `ROF_EMISSION_THRESHOLD` (usize) to `apply_env`.

- [ ] **Step 6: Run tests**

Run: `cargo test --lib && cargo test --test v4_shrink --test v4_caps --test v4_effort`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/llm/profile.rs src/llm/mod.rs src/llm/openrouter.rs src/config/mod.rs src/engine/session.rs src/main.rs
git commit -m "feat: endpoint capability profile (threshold + ladder order)"
```

---

### Task 4: verify_guard in the pipeline loop

**Files:**
- Modify: `src/engine/orchestrator.rs` (guard call after write gate)
- Modify: `tests/loop.rs` (veto test with FakeClient)

**Interfaces:**
- Consumes: `RoundServices::verify_guard` (exists, tested in direct mode).

- [ ] **Step 1: Write the failing test** in `tests/loop.rs` (inspect FakeClient's reviewer dispatch + Orchestrator construction first; add a `veto_guard: bool` flag; when the prompt starts with `"VERIFY-GUARD"` return `{"pass": false, ...}`, else behave as today):

```rust
#[tokio::test]
async fn a_pipeline_veto_becomes_a_failed_task() {
    // reviewer passes, guard vetoes, cfg.verify_guard = true
    // run_loop on a temp workdir (copy the file's existing harness setup)
    // assert out["passed"] == false and feedback contains "verify veto"
}
```

Mirror the file's existing simplest passing test; only the flag, the env/cfg bit, and the two asserts differ.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test loop pipeline_veto`
Expected: FAIL — pipeline passes (guard never consulted).

- [ ] **Step 3: Minimal implementation** in `run_loop`, between the write gate and `if verdict.pass { break; }`:

```rust
// v4 verify guard in the pipeline: a reviewer pass is not the final
// word when the outer judge is on. A veto fails the round like any
// other verdict, so the retry sees the veto note as feedback.
if verdict.pass {
    if let Some(veto) = svc.verify_guard(task, &artifact, &check_results).await {
        self.trace.emit(TraceEvent::StateTransition {
            from: "verifying".to_string(),
            to: "implementing".to_string(),
        });
        verdict = Verdict {
            pass: false,
            feedback: veto,
        };
    }
}
```

(`task` is `&String` in the loop — `verify_guard` takes `&str`; deref works.)

- [ ] **Step 4: Run tests**

Run: `cargo test --test loop && cargo test --test v4_verify`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/engine/orchestrator.rs tests/loop.rs
git commit -m "feat: verify_guard in pipeline loop (veto fails the round)"
```

---

### Task 5: Configured, shared explorer stage

**Files:**
- Modify: `src/agents/explorer.rs` (`RetrievalConfig` param, stats fields, `explorer_block`)
- Modify: `src/engine/orchestrator.rs` (direct call site ~line 769; pipeline insertion after retrieval ~line 100)
- Modify: `tests/v4_explorer.rs` (signature + new assertions)

**Interfaces:**
- Consumes: `RetrievalConfig`, `Retriever`.
- Produces: `pub fn explorer_block(rep: &ExplorerReport) -> String`.

- [ ] **Step 1: Write the failing tests** in `tests/v4_explorer.rs` (inspect current calls first; every `explore(goal, workdir, tools, trace)` gains `&cfg.retrieval`):

```rust
#[tokio::test]
async fn explorer_honours_the_configured_caps() {
    // retrieval with max_total_chars = 10 on a fixture tree:
    // report.chars_seen <= 10-ish bound through the same path the loop uses.
}

#[test]
fn explorer_block_names_files_with_stats() {
    let rep = explorer_report_for_test("g", &["a.rs", "b.rs"]);
    let line = explorer_block(&rep);
    assert!(line.contains("a.rs") && line.contains("b.rs"));
}

#[test]
fn explorer_block_is_empty_when_nothing_found() {
    assert_eq!(explorer_block(&ExplorerReport::default()), "");
}
```

(`explorer_report_for_test` must set the new stat fields to 0 — update it; the stats test then asserts the line still renders.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --test v4_explorer`
Expected: FAIL (signature + missing items).

- [ ] **Step 3: Minimal implementation.** `ExplorerReport` gains `#[serde(default)] pub snippets_seen: usize` and `#[serde(default)] pub chars_seen: usize` (check for persisted uses first — grep `ExplorerReport` outside explorer.rs/tests; if only in-memory, plain fields are fine but defaults cost nothing). `explore` gains `retrieval: &crate::config::RetrievalConfig` and uses it instead of `RetrievalConfig::default()` (including `cfg.max_total_chars` in the `retrieve` call and the `take(5)` cap — keep `take(5)` as the key-file cap, it is the report shape, not the retrieval cap). Fill the stats from the full `snips` (retrieval-level numbers, pre-take).

```rust
/// The shared exploration line both loops append: key-file names plus the
/// retrieval stats behind them, so a recall/token A/B reads off the prompt
/// instead of needing a new trace schema.
pub fn explorer_block(rep: &ExplorerReport) -> String {
    if rep.key_files.is_empty() {
        return String::new();
    }
    let names: Vec<String> = rep.key_files.iter().map(|k| k.path.clone()).collect();
    let line = format!(
        "\nexplorer key files ({} seen, {} chars): {}",
        rep.snippets_seen,
        rep.chars_seen,
        names.join(", ")
    );
    line
}
```

Direct loop: replace the inline `push_str` with `retrieved.push_str(&explorer_block(&rep))` and pass `&self.cfg.retrieval`. Pipeline loop: after `let retrieved = render(&snips);`, insert the identical gated block (`let mut retrieved = retrieved; if self.cfg.explorer { ... }`) appending to `impl_retrieved` — wait, pipeline splits named/unnamed (`impl_retrieved` feeds the implementer). The explorer line belongs with the implementer's volatile tail: append to `impl_retrieved` (declare it `mut`). Direct mode appends to its single `retrieved` — keep each loop's local shape, share only the `explore` + `explorer_block` calls.

- [ ] **Step 4: Run tests**

Run: `cargo test --test v4_explorer && cargo test --test loop`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/agents/explorer.rs src/engine/orchestrator.rs tests/v4_explorer.rs
git commit -m "feat: explorer uses configured retrieval, runs in both modes"
```

---

### Task 6: Config hygiene + renderer purity

**Files:**
- Modify: `src/tools/mod.rs` (`anchor_allowed_dirs` + `with_defaults` union + tests)
- Modify: `src/main.rs` (`setup` union, `rof config` note line)
- Modify: `src/eval/runner.rs` (isolation comment on the overwrite)
- Modify: `src/config/mod.rs` (field docs)
- Modify: `src/tui/app.rs` (`thinking` field), `src/tui/run.rs` (2 setters), `src/tui/ui.rs` (use field, fix comment)
- Test: `tests/tui_render.rs` (composer accent from `App`, if the file asserts on it — inspect first)

**Interfaces:**
- Produces: `pub fn anchor_allowed_dirs(policy: &mut PermissionPolicy, root: &PathBuf)`.

- [ ] **Step 1: Write the failing tests** (in `src/tools/mod.rs` `#[cfg(test)]` or the file's existing test spot):

```rust
#[test]
fn config_dirs_survive_with_root_first() {
    let mut p = PermissionPolicy { allowed_dirs: vec![PathBuf::from("/data")], ..Default::default() };
    anchor_allowed_dirs(&mut p, &PathBuf::from("/work"));
    assert_eq!(p.allowed_dirs, vec![PathBuf::from("/work"), PathBuf::from("/data")]);
}

#[test]
fn root_is_not_duplicated() {
    let mut p = PermissionPolicy { allowed_dirs: vec![PathBuf::from("/work")], ..Default::default() };
    anchor_allowed_dirs(&mut p, &PathBuf::from("/work"));
    assert_eq!(p.allowed_dirs.len(), 1);
}
```

Check `PermissionPolicy` field construction style in existing tool tests first (`..Default::default()` needs all fields defaulted — the struct derives it, confirmed).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib tools::anchor`
Expected: FAIL, no such function.

- [ ] **Step 3: Minimal implementation:**

```rust
/// Anchor the allowlist to the run root without dropping config-declared
/// dirs: root first (the common case reads first), extras preserved in
/// order, never duplicated. Eval task isolation still overwrites with the
/// task copy (see `run_task_in`) — that is a security boundary, not config.
pub fn anchor_allowed_dirs(policy: &mut PermissionPolicy, root: &PathBuf) {
    if !policy.allowed_dirs.iter().any(|d| d == root) {
        policy.allowed_dirs.insert(0, root.clone());
    }
}
```

`with_defaults` replaces `p.allowed_dirs = vec![root.clone()];` with the call. `setup()` in main.rs replaces its overwrite with the call. Update the field docs on `allowed_dirs` (absolute paths; eval restricts to the task copy) and the `rof config` note line to: `"# allowed_dirs is anchored to the workdir at run time (plus config extras; eval restricts to the task copy)."`.

- [ ] **Step 4: Renderer purity.** `App` gains `pub thinking: String` (Default: empty). `run.rs` sets `app.thinking = std::env::var("ROF_THINKING").unwrap_or_default();` after both `App::new()` sites (lines ~53, ~398). `draw` uses `&app.thinking`; fix the module comment to "every string comes from `App`". Inspect `tests/tui_render.rs` for composer-title assertions and extend with a thinking-accent case:

```rust
#[test]
fn composer_title_uses_app_thinking() {
    let mut app = App::new();
    app.thinking = "low".to_string();
    // render draw() to a test backend and assert the composer title contains "low"
}
```

(Follow the file's existing backend harness exactly.)

- [ ] **Step 5: Run tests**

Run: `cargo test --lib tools && cargo test --test tui_render --test tui_app --test tui_cmd`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/tools/mod.rs src/main.rs src/eval/runner.rs src/config/mod.rs src/tui/app.rs src/tui/run.rs src/tui/ui.rs tests/tui_render.rs
git commit -m "feat: honor allowed_dirs; renderer purity (thinking in App)"
```

---

## Self-Review

- Spec coverage: items 1–6 each have exactly one task (§2 split into oracle/reps). Memo item 7 (deferred) correctly absent.
- No placeholders: every step names files, shows code, gives the run command and expected output.
- Type consistency: `CheckResult`/`RoundServices` paths verified against session.rs visibility; `AgentOutput` field check flagged inline; `composer_block(&str)` signature confirmed in theme.rs.

## Execution

Inline execution in this session (owner pre-authorized "get 1-6 done"). After all tasks: full `cargo test`, `cargo fmt --check`, STATUS.md entry. Owner-decision briefings follow in chat.
