use async_trait::async_trait;
use rof::config::{AppConfig, PermissionPolicy};
use rof::engine::{Orchestrator, Session};
use rof::llm::{ContextService, ExecutorService, LlmClient, LlmError, LlmReq, LlmResp};
use rof::obs::TraceSink;
use rof::tools::{FsListTool, FsReadTool, ProcRunTool, ToolRegistry};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Dispatches canned JSON by role keyword in the system prompt.
struct FakeClient {
    review_calls: AtomicUsize,
    fail_first_review: bool,
    plan_tasks: Vec<&'static str>,
}

/// Set when a reviewer prompt carries the CHECKS section.
static SAW_CHECKS: AtomicBool = AtomicBool::new(false);
/// Set when a retry's implementer prompt carries the previous CHECKS.
static IMPL_SAW_CHECKS: AtomicBool = AtomicBool::new(false);
/// Set when a reviewer prompt carries the writes expectation + count.
static SAW_WRITE_EXPECT: AtomicBool = AtomicBool::new(false);
/// Set when the cheap model was asked to compress overflowing context.
static SAW_SUMMARY: AtomicBool = AtomicBool::new(false);

impl FakeClient {
    fn pass() -> Self {
        Self {
            review_calls: AtomicUsize::new(0),
            fail_first_review: false,
            plan_tasks: vec!["t1"],
        }
    }
    fn fail_then_pass() -> Self {
        Self {
            review_calls: AtomicUsize::new(0),
            fail_first_review: true,
            plan_tasks: vec!["t1"],
        }
    }
    fn two_tasks() -> Self {
        Self {
            review_calls: AtomicUsize::new(0),
            fail_first_review: false,
            plan_tasks: vec!["t1", "t2"],
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
        if req.system.contains("planner") {
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
    let root = std::env::temp_dir().join(format!("rof-loop-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
    // Real default grants + one allowlisted check for evidence flow.
    let policy = PermissionPolicy {
        allowed_dirs: vec![root.clone()],
        allowed_commands: vec!["echo checked".to_string()],
        ..Default::default()
    };
    let mut reg = ToolRegistry::new(policy);
    reg.register(FsListTool::new(root.clone()));
    reg.register(FsReadTool::new(root.clone()));
    reg.register(ProcRunTool::new(
        root.clone(),
        vec!["echo checked".to_string()],
    ));

    let cfg = AppConfig {
        max_review_rounds: max_rounds,
        budgets: rof::config::TokenBudgets {
            long_term: 2000,
            mid_term: 4000,
            short_term: short_term_budget,
        },
        ..Default::default()
    };
    let trace = Arc::new(TraceSink::new());
    let context = ContextService::new(client.clone(), "fake-ctx".to_string());
    let executor = ExecutorService::new(client, "fake-exec".to_string(), None);
    (Orchestrator::new(cfg, trace, context, executor), reg, root)
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
    // A 1-token short-term budget forces the builder to truncate, which must
    // route through ContextService::summarize instead of blind chopping.
    let (orch, reg, root) = harness_budget(Arc::new(FakeClient::pass()), "summ", 2, 1);
    let out = orch
        .run_loop(
            &Session::new("g".into())
                .expecting_writes(false)
                .with_checks(vec!["echo checked".to_string()]),
            &reg,
            &root,
        )
        .await;
    assert!(
        SAW_SUMMARY.load(Ordering::SeqCst),
        "summarizer never ran on truncated context"
    );
    // the run still completes normally after compression
    assert_eq!(out["passed"], true);
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
