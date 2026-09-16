use async_trait::async_trait;
use rof::config::{AppConfig, PermissionPolicy};
use rof::engine::{Orchestrator, Session};
use rof::llm::{ContextService, ExecutorService, LlmClient, LlmError, LlmReq, LlmResp};
use rof::obs::TraceSink;
use rof::tools::{FsListTool, FsPatchTool, FsReadTool, ProcRunTool, ToolRegistry};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Dispatches canned JSON by role keyword in the system prompt.
struct FakeClient {
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
/// Implementer calls seen in the read-request test.
static IMPL_READ_CALLS: AtomicUsize = AtomicUsize::new(0);
/// Implementer calls + last prompt for the escaping-read test.
static IMPL_ESCAPE_CALLS: AtomicUsize = AtomicUsize::new(0);
static IMPL_ESCAPE_PROMPT: Mutex<String> = Mutex::new(String::new());

impl FakeClient {
    fn pass() -> Self {
        Self {
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
            if req.prompt.contains("[REQUESTED FILES]") {
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
                // No second edit: the file already carries round 1's change.
                return Self::resp(
                    "{\"artifact\": \"no further change\", \"notes\": \"already applied\"}",
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
    reg.register(FsPatchTool::new(root.clone()));
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
async fn an_applied_change_is_handed_to_the_retry_as_it_is_now() {
    // Round 1's patch lands; the mid-term retrieval is a pre-round snapshot, so
    // without this evidence round 2 sees the file as it was and applies the
    // same change again (measured: E0592/E0428 duplicate definitions).
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
        retry.contains("FILE a.txt (applied by your previous round"),
        "retry prompt lacks the applied-change state: {retry}"
    );
    assert!(
        retry.contains("beta_fixed"),
        "retry prompt does not show the file as it is now: {retry}"
    );
    assert_eq!(out["rounds"], 2);
    assert_eq!(out["passed"], true);
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
