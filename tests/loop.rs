use async_trait::async_trait;
use rof::config::{AppConfig, PermissionPolicy};
use rof::engine::{Orchestrator, Session};
use rof::llm::{ContextService, ExecutorService, LlmClient, LlmError, LlmReq, LlmResp};
use rof::obs::{TraceEvent, TraceSink};
use rof::tools::{FsListTool, FsPatchTool, FsReadTool, ProcRunTool, ToolRegistry};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Dispatches canned JSON by role keyword in the system prompt.
struct FakeClient {
    direct_calls: AtomicUsize,
    direct_saw_feedback: AtomicBool,
    review_saw_verified_file: AtomicBool,
    planner_calls: AtomicUsize,
    review_calls: AtomicUsize,
    fail_first_review: bool,
    plan_tasks: Vec<&'static str>,
    /// Guess a patch anchor never read, then fix it once the retry hands over
    /// the file (exercises the refused-patch evidence path).
    guess_then_fix: bool,
    /// Patch a file successfully in round 1, then answer round 2 from the file
    /// state it was handed (exercises the applied-change evidence path).
    applied_retry: bool,
    /// Ask to read a file, then write with it (exercises the read-request turn).
    read_then_write: bool,
    /// Ask for a path outside the workdir, then answer.
    read_escape: bool,
}

/// Set when a reviewer prompt carries the CHECKS section.
static SAW_CHECKS: AtomicBool = AtomicBool::new(false);
/// Set when a retry's implementer prompt carries the previous CHECKS.
static IMPL_SAW_CHECKS: AtomicBool = AtomicBool::new(false);
/// Set when a reviewer prompt carries the writes expectation + count.
static SAW_WRITE_EXPECT: AtomicBool = AtomicBool::new(false);
/// Set when the cheap model was asked to compress overflowing context.
static SAW_SUMMARY: AtomicBool = AtomicBool::new(false);
/// The implementer prompt of a retry round, kept for assertions.
static IMPL_RETRY_PROMPT: Mutex<String> = Mutex::new(String::new());
/// Same, for the applied-change test (separate slot: tests run in parallel).
static IMPL_RETRY_PROMPT_APPLIED: Mutex<String> = Mutex::new(String::new());
/// The prompt of the implementer turn that carried requested files.
static IMPL_READ_PROMPT: Mutex<String> = Mutex::new(String::new());
/// The last reviewer prompt that carried evidence, kept for assertions.
static REVIEW_PROMPT: Mutex<String> = Mutex::new(String::new());
/// Implementer calls seen in the read-request test.
static IMPL_READ_CALLS: AtomicUsize = AtomicUsize::new(0);
/// Implementer calls + last prompt for the escaping-read test.
static IMPL_ESCAPE_CALLS: AtomicUsize = AtomicUsize::new(0);
static IMPL_ESCAPE_PROMPT: Mutex<String> = Mutex::new(String::new());

impl FakeClient {
    fn pass() -> Self {
        Self {
            direct_calls: AtomicUsize::new(0),
            direct_saw_feedback: AtomicBool::new(false),
            review_saw_verified_file: AtomicBool::new(false),
            planner_calls: AtomicUsize::new(0),
            review_calls: AtomicUsize::new(0),
            fail_first_review: false,
            plan_tasks: vec!["t1"],
            guess_then_fix: false,
            applied_retry: false,
            read_then_write: false,
            read_escape: false,
        }
    }
    fn fail_then_pass() -> Self {
        Self {
            direct_calls: AtomicUsize::new(0),
            direct_saw_feedback: AtomicBool::new(false),
            review_saw_verified_file: AtomicBool::new(false),
            planner_calls: AtomicUsize::new(0),
            review_calls: AtomicUsize::new(0),
            fail_first_review: true,
            plan_tasks: vec!["t1"],
            guess_then_fix: false,
            applied_retry: false,
            read_then_write: false,
            read_escape: false,
        }
    }
    fn two_tasks() -> Self {
        Self {
            direct_calls: AtomicUsize::new(0),
            direct_saw_feedback: AtomicBool::new(false),
            review_saw_verified_file: AtomicBool::new(false),
            planner_calls: AtomicUsize::new(0),
            review_calls: AtomicUsize::new(0),
            fail_first_review: false,
            plan_tasks: vec!["t1", "t2"],
            guess_then_fix: false,
            applied_retry: false,
            read_then_write: false,
            read_escape: false,
        }
    }
    fn guess_then_fix() -> Self {
        Self {
            direct_calls: AtomicUsize::new(0),
            direct_saw_feedback: AtomicBool::new(false),
            review_saw_verified_file: AtomicBool::new(false),
            planner_calls: AtomicUsize::new(0),
            review_calls: AtomicUsize::new(0),
            fail_first_review: false,
            plan_tasks: vec!["t1"],
            guess_then_fix: true,
            applied_retry: false,
            read_then_write: false,
            read_escape: false,
        }
    }
    fn applied_retry() -> Self {
        Self {
            direct_calls: AtomicUsize::new(0),
            direct_saw_feedback: AtomicBool::new(false),
            review_saw_verified_file: AtomicBool::new(false),
            planner_calls: AtomicUsize::new(0),
            review_calls: AtomicUsize::new(0),
            // the change lands in round 1, so the retry only happens if the
            // reviewer fails it
            fail_first_review: true,
            plan_tasks: vec!["t1"],
            guess_then_fix: false,
            applied_retry: true,
            read_then_write: false,
            read_escape: false,
        }
    }
    fn read_then_write() -> Self {
        Self {
            direct_calls: AtomicUsize::new(0),
            direct_saw_feedback: AtomicBool::new(false),
            review_saw_verified_file: AtomicBool::new(false),
            planner_calls: AtomicUsize::new(0),
            review_calls: AtomicUsize::new(0),
            fail_first_review: false,
            plan_tasks: vec!["t1"],
            guess_then_fix: false,
            applied_retry: false,
            read_then_write: true,
            read_escape: false,
        }
    }
    fn read_escape() -> Self {
        Self {
            direct_calls: AtomicUsize::new(0),
            direct_saw_feedback: AtomicBool::new(false),
            review_saw_verified_file: AtomicBool::new(false),
            planner_calls: AtomicUsize::new(0),
            review_calls: AtomicUsize::new(0),
            fail_first_review: false,
            plan_tasks: vec!["t1"],
            guess_then_fix: false,
            applied_retry: false,
            read_then_write: false,
            read_escape: true,
        }
    }
    fn resp(text: &str) -> Result<LlmResp, LlmError> {
        Ok(LlmResp {
            text: text.to_string(),
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 1,
            cost_usd: None,
            cached_input_tokens: 0,
            attempts: 1,
        })
    }
}

#[async_trait]
impl LlmClient for FakeClient {
    async fn complete(&self, _model: &str, req: LlmReq) -> Result<LlmResp, LlmError> {
        if req.system.contains("Compress the input") {
            SAW_SUMMARY.store(true, Ordering::SeqCst);
            return Self::resp("condensed: goal + task + prior feedback");
        }
        if req.system.contains("direct coding agent") {
            let call = self.direct_calls.fetch_add(1, Ordering::SeqCst);
            if call > 0
                && req.prompt.contains("CHECK OUTPUT:")
                && req.prompt.contains("FILE a.txt")
                && req.prompt.contains("ROLLED BACK")
            {
                // §4.2: a failed attempt is rolled back, so the retry's
                // feedback must say the change is gone rather than show text
                // the tree no longer has.
                self.direct_saw_feedback.store(true, Ordering::SeqCst);
            }
            return Self::resp(
                "{\"patches\":[{\"path\":\"a.txt\",\"search\":\"before\",\"replace\":\"after\"}],\"notes\":\"edited\"}",
            );
        }
        if req.system.contains("planner") {
            self.planner_calls.fetch_add(1, Ordering::SeqCst);
            let tasks = self
                .plan_tasks
                .iter()
                .map(|t| format!("\"{t}\""))
                .collect::<Vec<_>>()
                .join(", ");
            return Self::resp(&format!(
                "{{\"tasks\": [{tasks}], \"acceptance\": [\"a1\"]}}"
            ));
        }
        if req.system.contains("reviewer") {
            if req.prompt.contains("VERIFIED FILES:") {
                *REVIEW_PROMPT.lock().unwrap() = req.prompt.clone();
            }
            if req.prompt.contains("VERIFIED FILES:") && req.prompt.contains("beta_fixed") {
                self.review_saw_verified_file.store(true, Ordering::SeqCst);
            }
            if req.prompt.contains("CHECKS:") && req.prompt.contains("checked") {
                SAW_CHECKS.store(true, Ordering::SeqCst);
            }
            // The reviewer must always see the write expectation AND the count,
            // whatever the expectation is ("yes"/"no").
            if req.prompt.contains("EXPECT WRITES: ") && req.prompt.contains("WRITES MADE: ") {
                SAW_WRITE_EXPECT.store(true, Ordering::SeqCst);
            }
            let n = self.review_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_first_review && n == 0 {
                return Self::resp("{\"pass\": false, \"feedback\": \"missing tests\"}");
            }
            return Self::resp("{\"pass\": true, \"feedback\": \"looks good\"}");
        }
        if req.system.contains("implementer")
            && req.prompt.contains("PREVIOUS CHECKS:")
            && req.prompt.contains("checked")
        {
            IMPL_SAW_CHECKS.store(true, Ordering::SeqCst);
        }
        if req.system.contains("implementer") && self.guess_then_fix {
            if req.prompt.contains("round 2/2") {
                *IMPL_RETRY_PROMPT.lock().unwrap() = req.prompt.clone();
                // Round 2, file in hand: anchor on text that is actually there.
                return Self::resp(
                    "{\"patches\":[{\"path\":\"a.txt\",\"search\":\"beta_real_line\",\"replace\":\"beta_fixed\"}],\"notes\":\"ok\"}",
                );
            }
            // Round 1: a patch anchor this model never read.
            return Self::resp(
                "{\"patches\":[{\"path\":\"a.txt\",\"search\":\"line nobody read\",\"replace\":\"x\"}],\"notes\":\"guessing\"}",
            );
        }
        if req.system.contains("implementer") && self.read_then_write {
            IMPL_READ_CALLS.fetch_add(1, Ordering::SeqCst);
            // §4.1: a requested file reaches the second turn as an assembled
            // item labeled with its own path, not as a section header.
            if req.prompt.contains("--- schema.txt") {
                *IMPL_READ_PROMPT.lock().unwrap() = req.prompt.clone();
                return Self::resp(
                    "{\"patches\":[{\"path\":\"a.txt\",\"search\":\"beta_real_line\",\"replace\":\"beta_fixed\"}],\"notes\":\"now that I have seen it\"}",
                );
            }
            // Would-be guesser: ask for the file instead of inventing a field list.
            return Self::resp("{\"reads\":[\"schema.txt\"],\"artifact\":\"need the field list\"}");
        }
        if req.system.contains("implementer") && self.read_escape {
            IMPL_ESCAPE_CALLS.fetch_add(1, Ordering::SeqCst);
            *IMPL_ESCAPE_PROMPT.lock().unwrap() = req.prompt.clone();
            return Self::resp(
                "{\"reads\":[\"../../etc/passwd\",\"../outside.txt\"],\"artifact\":\"gimme\"}",
            );
        }
        if req.system.contains("implementer") && self.applied_retry {
            if req.prompt.contains("round 2/2") {
                *IMPL_RETRY_PROMPT_APPLIED.lock().unwrap() = req.prompt.clone();
                // §4.2: round 1's change was rolled back to the baseline, so
                // the retry applies it again against the file the prompt shows.
                return Self::resp(
                    "{\"patches\":[{\"path\":\"a.txt\",\"search\":\"beta_real_line\",\"replace\":\"beta_fixed\"}],\"notes\":\"re-applied\"}",
                );
            }
            return Self::resp(
                "{\"patches\":[{\"path\":\"a.txt\",\"search\":\"beta_real_line\",\"replace\":\"beta_fixed\"}],\"notes\":\"first attempt\"}",
            );
        }
        Self::resp("{\"artifact\": \"did t1\", \"notes\": \"ok\"}")
    }
}

fn harness(
    client: Arc<FakeClient>,
    tag: &str,
    max_rounds: u32,
) -> (Orchestrator, ToolRegistry, std::path::PathBuf) {
    harness_budget(client, tag, max_rounds, 6000)
}

fn harness_budget(
    client: Arc<FakeClient>,
    tag: &str,
    max_rounds: u32,
    short_term_budget: usize,
) -> (Orchestrator, ToolRegistry, std::path::PathBuf) {
    harness_with(client, tag, max_rounds, move |cfg| {
        cfg.budgets.short_term = short_term_budget;
    })
}

/// The same harness with the config open for tweaks (stage 2's per-layer tests
/// need to move one layer's budget without touching the others).
fn harness_with(
    client: Arc<FakeClient>,
    tag: &str,
    max_rounds: u32,
    tweak: impl FnOnce(&mut AppConfig),
) -> (Orchestrator, ToolRegistry, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("rof-loop-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
    // Real default grants + one allowlisted check for evidence flow.
    let policy = PermissionPolicy {
        allowed_dirs: vec![root.clone()],
        allowed_commands: vec![
            "echo checked".to_string(),
            "false".to_string(),
            // §4.3: a passing check whose output quotes the failure marker —
            // the verdict must read the field, not the log text.
            "echo \"prior STATUS: FAILED, now fixed\"".to_string(),
        ],
        ..Default::default()
    };
    let mut reg = ToolRegistry::new(policy);
    reg.register(FsListTool::new(root.clone()));
    reg.register(FsReadTool::new(root.clone()));
    reg.register(FsPatchTool::new(root.clone()));
    reg.register(ProcRunTool::new(
        root.clone(),
        vec![
            "echo checked".to_string(),
            "false".to_string(),
            "echo \"prior STATUS: FAILED, now fixed\"".to_string(),
        ],
    ));

    let mut cfg = AppConfig {
        max_review_rounds: max_rounds,
        budgets: rof::config::TokenBudgets {
            long_term: 2000,
            mid_term: 4000,
            short_term: 6000,
        },
        ..Default::default()
    };
    tweak(&mut cfg);
    let trace = Arc::new(TraceSink::new());
    let context = ContextService::new(client.clone(), "fake-ctx".to_string());
    let executor = ExecutorService::new(client.clone(), "fake-exec".to_string(), None);
    // Verify slot: unset in the router means the executor model, so the
    // test helper clones it too.
    let verify = ExecutorService::new(client, "fake-verify".to_string(), None);
    (
        Orchestrator::new(cfg, trace, context, executor, verify),
        reg,
        root,
    )
}

#[tokio::test]
async fn loop_passes_first_round() {
    let (orch, reg, root) = harness(Arc::new(FakeClient::pass()), "pass", 2);
    let out = orch
        .run_loop(
            &Session::new("g".into()).expecting_writes(false),
            &reg,
            &root,
        )
        .await;
    assert_eq!(out["passed"], true);
    assert_eq!(out["rounds"], 1);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn direct_mode_uses_one_executor_and_requires_passing_checks() {
    let client = Arc::new(FakeClient::pass());
    let (orch, reg, root) = harness_with(client.clone(), "direct", 2, |cfg| {
        cfg.execution = "direct".to_string();
    });
    std::fs::write(root.join("a.txt"), "before\n").unwrap();
    let out = orch
        .run_loop(
            &Session::new("edit a.txt".into()).with_checks(vec!["echo checked".to_string()]),
            &reg,
            &root,
        )
        .await;

    assert_eq!(out["passed"], true);
    assert_eq!(out["rounds"], 1);
    assert_eq!(out["tasks"][0]["writes_made"], 1);
    assert_eq!(out["tasks"][0]["feedback"], "");
    assert_eq!(client.direct_calls.load(Ordering::SeqCst), 1);
    assert_eq!(client.planner_calls.load(Ordering::SeqCst), 0);
    assert_eq!(client.review_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "after\n"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn direct_mode_retries_with_failed_check_feedback() {
    let client = Arc::new(FakeClient::pass());
    let (orch, reg, root) = harness_with(client.clone(), "direct-fail", 2, |cfg| {
        cfg.execution = "direct".to_string();
    });
    std::fs::write(root.join("a.txt"), "before\n").unwrap();
    let out = orch
        .run_loop(
            &Session::new("edit a.txt".into()).with_checks(vec!["false".to_string()]),
            &reg,
            &root,
        )
        .await;

    assert_eq!(out["passed"], false);
    assert_eq!(out["rounds"], 2);
    assert!(out["checks"]
        .as_str()
        .unwrap_or_default()
        .contains("STATUS: FAILED"));
    assert!(client.direct_saw_feedback.load(Ordering::SeqCst));
    assert_eq!(client.planner_calls.load(Ordering::SeqCst), 0);
    assert_eq!(client.review_calls.load(Ordering::SeqCst), 0);
    std::fs::remove_dir_all(&root).ok();
}

/// §4.3: the verdict is a field read on `CheckResult`, not a substring of the
/// rendered log. A check that passes may quote "STATUS: FAILED" from a
/// previous error it fixed; the old `checks_log.contains(...)` would flip the
/// whole task to failed on that quote.
#[tokio::test]
async fn direct_mode_does_not_read_the_verdict_off_the_log_text() {
    let client = Arc::new(FakeClient::pass());
    let (orch, reg, root) = harness_with(client.clone(), "direct-verdict", 2, |cfg| {
        cfg.execution = "direct".to_string();
    });
    std::fs::write(root.join("a.txt"), "before\n").unwrap();
    // A passing check whose output happens to contain the failure marker.
    let out = orch
        .run_loop(
            &Session::new("edit a.txt".into())
                .with_checks(vec!["echo \"prior STATUS: FAILED, now fixed\"".to_string()]),
            &reg,
            &root,
        )
        .await;

    assert_eq!(
        out["passed"], true,
        "a passed check quoting the marker must not fail the task"
    );
    assert_eq!(out["rounds"], 1);
    std::fs::remove_dir_all(&root).ok();
}

/// §4.3: direct mode now takes the same skill index and goal-quality note the
/// pipeline loop takes — both were written into `run_loop` only, so a direct
/// run used to see neither. Both are observable in the trace.
#[tokio::test]
async fn direct_mode_gets_the_skill_index_and_goal_note() {
    let client = Arc::new(FakeClient::pass());
    let trace = Arc::new(TraceSink::new());
    let root = std::env::temp_dir().join(format!("rof-direct-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), "before\n").unwrap();
    // A real skill in an out-of-workdir store, so the index is non-empty and
    // the shared `skill_index` has something to deliver.
    let skills_store = root.join("skills-store");
    let skill_dir = skills_store.join("a-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: a-skill\ndescription: one-line when to use it\n---\nbody\n",
    )
    .unwrap();
    let manager = Arc::new(rof::skills::SkillManager::new(
        skills_store,
        None,
        rof::skills::SkillPolicy::ReadOnly,
    ));
    let mut reg = ToolRegistry::new(PermissionPolicy {
        allowed_dirs: vec![root.clone()],
        allowed_commands: vec!["echo checked".to_string()],
        ..Default::default()
    });
    reg.register(FsPatchTool::new(root.clone()));
    reg.register(rof::tools::SkillsListTool::new(manager));

    let cfg = AppConfig {
        execution: "direct".to_string(),
        goal_quality: true,
        ..Default::default()
    };
    let context = ContextService::new(client.clone(), "fake-ctx".to_string());
    let executor = ExecutorService::new(client.clone(), "fake-exec".to_string(), None);
    let verify = ExecutorService::new(client, "fake-verify".to_string(), None);
    let orch = Orchestrator::new(cfg, trace.clone(), context, executor, verify);

    let out = orch
        .run_loop(&Session::new("edit a.txt in the repo".into()), &reg, &root)
        .await;
    assert_eq!(out["passed"], true);

    let events = trace.events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            TraceEvent::SkillOp { agent, op, .. } if agent == "implementer" && op == "list"
        )),
        "direct mode must deliver the skill index: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TraceEvent::GoalQuality { .. })),
        "direct mode must run the goal-quality note"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// §4.3: direct mode takes the same bounded auto-poke. A run that hits the cap
/// without an accepted result gets exactly one extra round, not a hard stop.
#[tokio::test]
async fn direct_mode_auto_pokes_once_at_the_cap() {
    let client = Arc::new(FakeClient::pass());
    let trace = Arc::new(TraceSink::new());
    let root = std::env::temp_dir().join(format!("rof-direct-poke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), "before\n").unwrap();
    let mut reg = ToolRegistry::new(PermissionPolicy {
        allowed_dirs: vec![root.clone()],
        allowed_commands: vec!["false".to_string()],
        ..Default::default()
    });
    reg.register(FsPatchTool::new(root.clone()));
    reg.register(ProcRunTool::new(root.clone(), vec!["false".to_string()]));

    let cfg = AppConfig {
        execution: "direct".to_string(),
        auto_poke: true,
        max_review_rounds: 2,
        ..Default::default()
    };
    let context = ContextService::new(client.clone(), "fake-ctx".to_string());
    let executor = ExecutorService::new(client.clone(), "fake-exec".to_string(), None);
    let verify = ExecutorService::new(client, "fake-verify".to_string(), None);
    let orch = Orchestrator::new(cfg, trace.clone(), context, executor, verify);

    let out = orch
        .run_loop(
            &Session::new("edit a.txt in the repo".into()).with_checks(vec!["false".to_string()]),
            &reg,
            &root,
        )
        .await;

    // The cap was 2 and the poke added exactly one; the check still fails, so
    // the run ends at 3 rather than passing.
    assert_eq!(
        out["rounds"], 3,
        "the poke must extend the cap by exactly one"
    );
    assert_eq!(out["passed"], false);
    assert!(
        trace
            .events()
            .iter()
            .any(|e| matches!(e, TraceEvent::AutoPoke { .. })),
        "an auto-poke must be traced"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn loop_retries_on_feedback_then_passes() {
    let (orch, reg, root) = harness(Arc::new(FakeClient::fail_then_pass()), "retry", 2);
    let out = orch
        .run_loop(
            &Session::new("g".into())
                .expecting_writes(false)
                .with_checks(vec!["echo checked".to_string()]),
            &reg,
            &root,
        )
        .await;
    assert_eq!(out["passed"], true);
    assert_eq!(out["rounds"], 2);
    // round 2's implementer must have seen round 1's check output
    assert!(
        IMPL_SAW_CHECKS.load(Ordering::SeqCst),
        "implementer retry never saw previous checks"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn checks_reach_reviewer_as_evidence() {
    let (orch, reg, root) = harness(Arc::new(FakeClient::pass()), "checks", 2);
    let out = orch
        .run_loop(
            &Session::new("g".into())
                .expecting_writes(false)
                .with_checks(vec!["echo checked".to_string()]),
            &reg,
            &root,
        )
        .await;
    assert_eq!(out["passed"], true);
    let log = out["checks"].as_str().unwrap_or_default();
    assert!(log.contains("checked"), "check log: {log}");
    // A passing check must state its outcome: the reviewer cannot be asked to
    // read a build's silence as success.
    assert!(
        log.contains("STATUS: PASSED"),
        "check log lacks an explicit status: {log}"
    );
    assert!(
        SAW_CHECKS.load(Ordering::SeqCst),
        "reviewer never saw CHECKS"
    );
    // the empty-write gate must be visible to the reviewer
    assert!(
        SAW_WRITE_EXPECT.load(Ordering::SeqCst),
        "reviewer never saw EXPECT WRITES / WRITES MADE"
    );
    assert_eq!(out["tasks"][0]["writes_made"], 0);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn reviewer_gets_independent_file_evidence() {
    let client = Arc::new(FakeClient::guess_then_fix());
    let (orch, reg, root) = harness_with(client.clone(), "review-file", 2, |cfg| {
        cfg.expect_writes = true;
    });
    std::fs::write(root.join("a.txt"), "alpha\nbeta_real_line\ngamma\n").unwrap();
    let out = orch
        .run_loop(&Session::new("edit a.txt".into()), &reg, &root)
        .await;

    assert_eq!(out["passed"], true);
    assert!(client.review_saw_verified_file.load(Ordering::SeqCst));
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn reviewer_evidence_is_windowed_and_carried_once() {
    // §4.1's two measured failures in one prompt: the evidence was read whole
    // (up to 262 KB a file) and then head+tail-collapsed by the short layer's
    // budget, which can drop the only region under judgement; and the same
    // bytes travelled twice, as `file_state` JSON and as `[VERIFIED FILES]`.
    let client = Arc::new(FakeClient::guess_then_fix());
    let (orch, reg, root) = harness_with(client.clone(), "window", 2, |cfg| {
        cfg.expect_writes = true;
    });
    // 80_000 chars with the change at the middle line: a whole-file view
    // cannot fit the evidence window, and a head+tail cut loses the change.
    let mut big = String::new();
    for i in 0..2000 {
        if i == 1000 {
            big.push_str("beta_real_line\n");
        } else {
            big.push_str(&format!("filler line number {i:04} padded out\n"));
        }
    }
    std::fs::write(root.join("a.txt"), big).unwrap();
    let out = orch
        .run_loop(&Session::new("edit a.txt".into()), &reg, &root)
        .await;
    assert_eq!(out["passed"], true);
    let reviewer = REVIEW_PROMPT.lock().unwrap().clone();
    assert!(!reviewer.is_empty(), "no reviewer prompt carried evidence");
    // The judged region survives the window...
    assert!(
        reviewer.contains("beta_fixed"),
        "the window lost the region under judgement: {reviewer}"
    );
    // ...and the head and tail of an 80 KB file do not.
    assert!(
        !reviewer.contains("filler line number 0000")
            && !reviewer.contains("filler line number 1999"),
        "evidence was delivered whole instead of windowed: {reviewer}"
    );
    // The body travels once: `file_state` is stripped to its path and anchor,
    // so `[VERIFIED FILES]` is the only copy of the 80 KB. A patch's `replace`
    // is intent, not duplication, and stays.
    assert!(
        !reviewer.contains("current_content"),
        "the file body travelled twice, as JSON and as evidence: {reviewer}"
    );
    assert!(
        out["eliminated_chars"].as_u64().unwrap_or(0) > 0,
        "nothing was counted as eliminated: {}",
        out["eliminated_chars"]
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn plan_tasks_each_get_their_own_cycle() {
    let (orch, reg, root) = harness(Arc::new(FakeClient::two_tasks()), "tasks", 2);
    let out = orch
        .run_loop(
            &Session::new("g".into()).expecting_writes(false),
            &reg,
            &root,
        )
        .await;
    assert_eq!(out["passed"], true);
    let tasks = out["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2, "both plan tasks must run: {tasks:?}");
    assert_eq!(tasks[0]["task"], "t1");
    assert_eq!(tasks[1]["task"], "t2");
    assert_eq!(out["rounds"], 2, "one round each");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn token_budget_stops_a_task_before_the_next_round() {
    // fail_first_review forces a retry; a 1-token ceiling must block it.
    let (orch, reg, root) = harness(Arc::new(FakeClient::fail_then_pass()), "budget", 2);
    let cfg_probe = orch.token_limit_for_test();
    assert_eq!(cfg_probe, 50_000, "default ceiling sanity");

    let out = orch
        .run_loop(
            &Session::new("g".into())
                .expecting_writes(false)
                .with_token_limit(Some(1)),
            &reg,
            &root,
        )
        .await;
    assert_eq!(out["passed"], false);
    assert_eq!(out["rounds"], 1, "second round must not run");
    assert_eq!(out["tasks"][0]["aborted"], true);
    let fb = out["tasks"][0]["feedback"].as_str().unwrap_or_default();
    assert!(fb.contains("token budget exceeded"), "feedback: {fb}");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn overflowing_context_is_compressed_by_the_cheap_model() {
    // A long goal against a 30-token mid-term budget puts the mid layer (goal +
    // retrieval + plan) far over its threshold, so it must route through
    // ContextService::summarize instead of blind chopping. (v1 compressed the
    // whole prompt *after* it had overflowed; stage 2 compresses one layer
    // *before* the cut, and truncation is only the fallback.)
    let (orch, reg, root) = harness_with(Arc::new(FakeClient::pass()), "summ", 2, |cfg| {
        cfg.budgets.mid_term = 30;
    });
    let out = orch
        .run_loop(
            &Session::new(format!("g {}", "x".repeat(5000)))
                .expecting_writes(false)
                .with_checks(vec!["echo checked".to_string()]),
            &reg,
            &root,
        )
        .await;
    assert!(
        SAW_SUMMARY.load(Ordering::SeqCst),
        "summarizer never ran on an over-threshold layer"
    );
    // the run still completes normally after compression
    assert_eq!(out["passed"], true);
    // and the layer traffic is attributable: mid summarized, none truncated.
    assert!(
        out["layer_summaries"][1].as_u64().unwrap_or(0) >= 1,
        "layer_summaries: {}",
        out["layer_summaries"]
    );
    assert_eq!(out["layer_truncations"][1], 0);
    assert!(out["summarize_calls"].as_u64().unwrap_or(0) >= 1);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn loop_exhausts_rounds_on_persistent_fail() {
    let (orch, reg, root) = harness(Arc::new(FakeClient::fail_then_pass()), "exhaust", 1);
    let out = orch
        .run_loop(
            &Session::new("g".into()).expecting_writes(false),
            &reg,
            &root,
        )
        .await;
    // fail_then_pass fails round 1; max 1 round -> fail overall
    assert_eq!(out["passed"], false);
    assert_eq!(out["rounds"], 1);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn a_refused_patch_hands_the_file_to_the_retry() {
    // Round 1 patches an anchor the model never read. The harness must put the
    // file's real text into round 2's prompt — a bare "search string not found"
    // is what makes a retry repeat the same guess.
    let (orch, reg, root) = harness(Arc::new(FakeClient::guess_then_fix()), "refused", 2);
    std::fs::write(root.join("a.txt"), "alpha\nbeta_real_line\ngamma\n").unwrap();
    let out = orch.run_loop(&Session::new("g".into()), &reg, &root).await;
    let retry = IMPL_RETRY_PROMPT.lock().unwrap().clone();
    assert!(
        retry.contains("PATCH REFUSED for a.txt"),
        "retry prompt lacks the refusal: {retry}"
    );
    assert!(
        retry.contains("beta_real_line"),
        "retry prompt lacks the file's current text: {retry}"
    );
    // the retry's patch lands on the real file, so the task completes
    assert_eq!(out["rounds"], 2);
    assert_eq!(out["passed"], true);
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "alpha\nbeta_fixed\ngamma\n"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn an_applied_change_is_rolled_back_and_re_applied_by_the_retry() {
    // Round 1's patch lands and the reviewer fails it, so §4.2 restores the
    // baseline: the retry's prompt must say the change is gone rather than
    // hand it the post-attempt text as if it were still on disk. With that
    // stale text a retry re-applies an edit that is already there (measured:
    // E0592/E0428 duplicate definitions); told nothing, it applies nothing
    // and the task's change is silently lost.
    let (orch, reg, root) = harness(Arc::new(FakeClient::applied_retry()), "applied", 2);
    std::fs::write(root.join("a.txt"), "alpha\nbeta_real_line\ngamma\n").unwrap();
    let out = orch
        .run_loop(
            &Session::new("g".into()).expecting_writes(false),
            &reg,
            &root,
        )
        .await;
    let retry = IMPL_RETRY_PROMPT_APPLIED.lock().unwrap().clone();
    assert!(
        retry.contains("ROLLED BACK"),
        "retry prompt must say the change was rolled back: {retry}"
    );
    assert!(
        !retry.contains("beta_fixed"),
        "retry prompt carries the post-attempt text the tree no longer has: {retry}"
    );
    assert_eq!(out["rounds"], 2);
    assert_eq!(out["passed"], true);
    // The retry re-applied the change against the baseline, so it landed.
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "alpha\nbeta_fixed\ngamma\n"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn a_read_request_is_honored_once_and_then_the_model_writes() {
    // The measured blocker: the model needs a fact it never read (a struct's
    // field list) and has no way to look. It can now ask, gets one extra turn
    // with the file, and the patch lands in the same round.
    let (orch, reg, root) = harness(Arc::new(FakeClient::read_then_write()), "reads", 2);
    std::fs::write(root.join("a.txt"), "alpha\nbeta_real_line\ngamma\n").unwrap();
    std::fs::write(root.join("schema.txt"), "struct Thing { field_a: u8 }\n").unwrap();
    let out = orch.run_loop(&Session::new("g".into()), &reg, &root).await;
    let seen = IMPL_READ_PROMPT.lock().unwrap().clone();
    assert!(
        seen.contains("schema.txt") && seen.contains("field_a"),
        "the requested file never reached the second turn: {seen}"
    );
    // the same file asked for twice is delivered once: the dedupe is by key
    assert!(
        seen.matches("field_a: u8").count() <= 1,
        "the requested file's bytes reached the prompt twice: {seen}"
    );
    // the path map tells the model what it may ask for
    assert!(
        seen.contains("[REPO FILES]") && seen.contains("schema.txt"),
        "implementer prompt lacks the path map: {seen}"
    );
    assert_eq!(
        IMPL_READ_CALLS.load(Ordering::SeqCst),
        2,
        "one request, one extra call"
    );
    assert_eq!(
        out["rounds"], 1,
        "the read turn must not burn a review round"
    );
    assert_eq!(out["passed"], true);
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "alpha\nbeta_fixed\ngamma\n"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn a_read_request_outside_the_root_is_refused() {
    // The path is model-chosen, so it goes through the same gate as everything
    // else: denied, no content leaked into the prompt, and a repeated request
    // does not loop.
    let (orch, reg, root) = harness(Arc::new(FakeClient::read_escape()), "escape", 2);
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    // `../outside.txt` from the workdir is a real file next to it, so a leak is
    // detectable rather than assumed.
    let outside = std::env::temp_dir().join("outside.txt");
    std::fs::write(&outside, "OUTSIDE-SECRET-CONTENT\n").unwrap();
    let out = orch.run_loop(&Session::new("g".into()), &reg, &root).await;
    let seen = IMPL_ESCAPE_PROMPT.lock().unwrap().clone();
    assert!(
        !seen.contains("OUTSIDE-SECRET-CONTENT") && !seen.contains("root:x:"),
        "an out-of-root file's content leaked into the prompt: {seen}"
    );
    assert!(
        seen.contains("unreadable"),
        "denial must be legible to the model: {seen}"
    );
    assert!(
        IMPL_ESCAPE_CALLS.load(Ordering::SeqCst) <= 4,
        "a request may add one call per round (2 rounds here), never a loop: {}",
        IMPL_ESCAPE_CALLS.load(Ordering::SeqCst)
    );
    assert_eq!(out["passed"], false, "no writes means no pass");
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_file(&outside).ok();
}
