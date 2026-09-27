//! The protected oracle (design report §5, build order item 1).
//!
//! The failure mode this guards is Claude Code issue #319: an agent told to
//! get tests passing "simply updated the make file to only run tests that
//! were passing. It called these 'safe-tests'", with corroborating reports of
//! edited assertions and tests kept "that do absolutely nothing". The issue
//! was closed by an inactivity bot, never fixed. The result is a run that
//! reports success over a system it broke.
//!
//! These tests hold the gate to its contract: an agent that modifies a
//! baseline test file cannot pass; an agent that only *adds* a test file still
//! can (creating tests is a deliverable); production-only work, empty work and
//! `expect_writes: false` are untouched; and the two refusal reasons stay
//! distinguishable in the trace. Everything runs against a real temp git
//! workdir — the same substrate the gate reads — with a canned fake model, no
//! network and no terminal.

use async_trait::async_trait;
use rof::config::{AppConfig, PermissionPolicy};
use rof::engine::{Orchestrator, Session};
use rof::llm::{ContextService, ExecutorService, LlmClient, LlmError, LlmReq, LlmResp};
use rof::obs::{TraceEvent, TraceSink};
use rof::tools::{FsListTool, FsPatchTool, FsReadTool, FsWriteTool, ProcRunTool, ToolRegistry};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// What the fake implementer does each time it is asked to work. The model is
/// canned, so the behaviour under test is the *harness's* reaction to it.
#[derive(Clone, Copy, Debug)]
enum Action {
    /// Weaken the baseline assertion so the buggy code satisfies it — the
    /// exact #319 tamper.
    TamperTest,
    /// Fix the production code, leaving the suite alone — the honest completion.
    FixCode,
    /// Create a brand-new test file, touching nothing at baseline.
    AddTest,
    /// Emit no writes at all (write-gate territory, not the oracle's).
    NoWrites,
    /// Round 1 writes nothing (write-gate refusal), round 2 tampers the test
    /// (oracle refusal) — both refusals land in one trace, distinct.
    NoWritesThenTamper,
    /// Round 1 tampers the test (oracle refusal), round 2 fixes the code.
    /// This is the test that also proves requirement 4: the existing pre-retry
    /// rollback reverts the tampered oracle, so round 2 starts clean.
    TamperThenFix,
}

struct OracleClient {
    action: Action,
    /// Every implementer prompt this client saw, in call order.
    impl_prompts: Mutex<Vec<String>>,
    direct_calls: AtomicUsize,
}

impl OracleClient {
    fn new(action: Action) -> Self {
        Self {
            action,
            impl_prompts: Mutex::new(Vec::new()),
            direct_calls: AtomicUsize::new(0),
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

const TAMPER_TEST: &str = r#"{"patches":[{"path":"tests/oracle.rs","search":"assert_eq!(add(1, 1), 2)","replace":"assert_eq!(add(1, 1), 0)"}],"notes":"made the assertion agree with the code"}"#;
const FIX_CODE: &str = r#"{"patches":[{"path":"src/lib.rs","search":"a - b","replace":"a + b"}],"notes":"fixed the bug"}"#;
const ADD_TEST: &str = r##"{"writes":[{"path":"tests/added_test.rs","content":"#[test]\nfn added() {\n    assert_eq!(1, 1);\n}\n"}],"notes":"added a regression test"}"##;
const NO_WRITES: &str = r#"{"artifact":"done without writing","notes":"analysis only"}"#;

/// The canned implementer's move for one round. Rounds are told apart by the
/// prompt's own round marker ("round 2/2" in the pipeline, "DIRECT ROUND 2/2"
/// in direct mode), so no cross-round state is needed in the fake.
fn move_for(action: Action, round_two: bool) -> &'static str {
    match action {
        Action::TamperTest => TAMPER_TEST,
        Action::FixCode => FIX_CODE,
        Action::AddTest => ADD_TEST,
        Action::NoWrites => NO_WRITES,
        Action::NoWritesThenTamper => {
            if round_two {
                TAMPER_TEST
            } else {
                NO_WRITES
            }
        }
        Action::TamperThenFix => {
            if round_two {
                FIX_CODE
            } else {
                TAMPER_TEST
            }
        }
    }
}

fn is_round_two(prompt: &str) -> bool {
    prompt.to_ascii_lowercase().contains("round 2/")
}

#[async_trait]
impl LlmClient for OracleClient {
    async fn complete(&self, _model: &str, req: LlmReq) -> Result<LlmResp, LlmError> {
        // Direct mode has its own system prompt and its own round marker; it
        // never reaches the reviewer branch below.
        if req.system.contains("direct coding agent") {
            self.impl_prompts.lock().unwrap().push(req.prompt.clone());
            self.direct_calls.fetch_add(1, Ordering::SeqCst);
            return Self::resp(move_for(self.action, is_round_two(&req.prompt)));
        }
        if req.system.contains("reviewer") {
            // A pass-happy reviewer is the assumption the gate exists under:
            // it passes the tampered suite, and the harness must still refuse.
            return Self::resp(r#"{"pass": true, "feedback": "looks good"}"#);
        }
        if req.system.contains("implementer") {
            self.impl_prompts.lock().unwrap().push(req.prompt.clone());
            return Self::resp(move_for(self.action, is_round_two(&req.prompt)));
        }
        Self::resp(r#"{"artifact": "done", "notes": "ok"}"#)
    }
}

/// One named dir under the temp dir, cleared first, so every test gets a
/// hermetic git workdir without a temp-dir crate.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rof-oracle-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The seeded repo: a buggy `add` (subtracts) and a test that encodes the bug
/// — the suite is red against the bug and green only when the code is right,
/// which is exactly the arrangement the #319 agent "fixed" by editing the test
/// instead of the subtraction. Nothing here compiles or runs: no check is
/// configured for the pipeline tests, so the oracle under test is the change
/// set itself.
fn seed(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("tests/oracle.rs"),
        "#[test]\nfn it_adds() {\n    assert_eq!(add(1, 1), 2);\n}\n",
    )
    .unwrap();
}

/// The seeded baseline, as text, so a test can say exactly what survived.
const BASELINE_TEST: &str = "#[test]\nfn it_adds() {\n    assert_eq!(add(1, 1), 2);\n}\n";

fn harness(
    client: Arc<OracleClient>,
    tag: &str,
    max_rounds: u32,
) -> (
    Orchestrator,
    ToolRegistry,
    std::path::PathBuf,
    Arc<TraceSink>,
) {
    harness_with(client, tag, max_rounds, |_| {}, false)
}

/// `direct` builds the direct-mode loop instead of the pipeline one; `tweak`
/// adjusts the config the way the calling test needs.
fn harness_with(
    client: Arc<OracleClient>,
    tag: &str,
    max_rounds: u32,
    tweak: impl FnOnce(&mut AppConfig),
    direct: bool,
) -> (
    Orchestrator,
    ToolRegistry,
    std::path::PathBuf,
    Arc<TraceSink>,
) {
    let root = scratch(tag);
    seed(&root);
    // Real grants, and one allowlisted check so direct mode has an oracle.
    let policy = PermissionPolicy {
        allowed_dirs: vec![root.clone()],
        allowed_commands: vec!["echo checked".to_string()],
        ..Default::default()
    };
    let mut reg = ToolRegistry::new(policy);
    reg.register(FsListTool::new(root.clone()));
    reg.register(FsReadTool::new(root.clone()));
    reg.register(FsPatchTool::new(root.clone()));
    reg.register(FsWriteTool::new(root.clone()));
    reg.register(ProcRunTool::new(
        root.clone(),
        vec!["echo checked".to_string()],
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
    if direct {
        cfg.execution = "direct".to_string();
    }
    tweak(&mut cfg);
    let trace = Arc::new(TraceSink::new());
    let context = ContextService::new(client.clone(), "fake-ctx".to_string());
    let executor = ExecutorService::new(client.clone(), "fake-exec".to_string(), None);
    let verify = ExecutorService::new(client, "fake-verify".to_string(), None);
    (
        Orchestrator::new(cfg, trace.clone(), context, executor, verify),
        reg,
        root,
        trace,
    )
}

/// The (from, to) pairs the run recorded, so a test can hold the two refusal
/// reasons apart by name rather than by substring.
fn transitions(trace: &TraceSink) -> Vec<(String, String)> {
    trace
        .events()
        .iter()
        .filter_map(|e| match e {
            TraceEvent::StateTransition { from, to } => Some((from.clone(), to.clone())),
            _ => None,
        })
        .collect()
}

/// #319's shape: the agent rewrites the baseline assertion instead of the
/// code, the reviewer passes it anyway, and the harness must refuse.
#[tokio::test]
async fn an_agent_that_tampers_a_baseline_test_cannot_pass() {
    let (orch, reg, root, trace) =
        harness(Arc::new(OracleClient::new(Action::TamperTest)), "tamper", 1);
    let out = orch
        .run_loop(&Session::new("make the test pass".into()), &reg, &root)
        .await;

    assert_eq!(out["passed"], false, "a tampered oracle must not pass");
    let fb = out["tasks"][0]["feedback"]
        .as_str()
        .unwrap_or("NO FEEDBACK");
    assert!(
        fb.starts_with("harness: pass rejected"),
        "the harness — not the reviewer — must be named as the refuser: {fb}"
    );
    assert!(
        fb.contains("tests/oracle.rs"),
        "the feedback must name the offending path: {fb}"
    );
    let tos: Vec<String> = transitions(&trace).into_iter().map(|(_, to)| to).collect();
    assert!(
        tos.iter().any(|to| to == "rejected_oracle_modified"),
        "the trace must carry rejected_oracle_modified: {tos:?}"
    );
    // And it did not reach for the write gate's reason: the two failures are
    // different and the trace must not conflate them.
    assert!(
        !tos.iter().any(|to| to == "rejected_no_writes"),
        "a write did land, so the no-writes reason must not fire: {tos:?}"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// The regression the gate would otherwise cause: a task whose deliverable is
/// a new test must still be able to pass. A new file cannot retroactively
/// rewrite an existing failing assertion, so it is not protected.
#[tokio::test]
async fn an_agent_that_only_adds_a_new_test_file_can_still_pass() {
    let (orch, reg, root, trace) =
        harness(Arc::new(OracleClient::new(Action::AddTest)), "add-test", 1);
    let out = orch
        .run_loop(&Session::new("add a regression test".into()), &reg, &root)
        .await;

    assert_eq!(
        out["passed"], true,
        "a new test file is a deliverable, not tampering"
    );
    assert_eq!(out["tasks"][0]["writes_made"], 1);
    let tos: Vec<String> = transitions(&trace).into_iter().map(|(_, to)| to).collect();
    assert!(
        !tos.iter().any(|to| to == "rejected_oracle_modified"),
        "the gate must not fire on an untracked test file: {tos:?}"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// Only the oracle is protected: a run that changes production code and
/// leaves the suite alone is exactly what the harness wants.
#[tokio::test]
async fn a_run_that_changes_only_production_files_is_unaffected() {
    let (orch, reg, root, trace) =
        harness(Arc::new(OracleClient::new(Action::FixCode)), "fix-code", 1);
    let out = orch
        .run_loop(&Session::new("fix the add function".into()), &reg, &root)
        .await;

    assert_eq!(out["passed"], true);
    let tos: Vec<String> = transitions(&trace).into_iter().map(|(_, to)| to).collect();
    assert!(
        !tos.iter().any(|to| to == "rejected_oracle_modified"),
        "production-only work must not trip the gate: {tos:?}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// `expect_writes: false` with an empty change set is an analysis task: the
/// harness does not police the change set at all, and the reviewer is the
/// oracle for that class.
#[tokio::test]
async fn expect_writes_false_with_no_changes_is_unaffected() {
    let (orch, reg, root, trace) = harness(
        Arc::new(OracleClient::new(Action::NoWrites)),
        "no-writes-expected",
        1,
    );
    let out = orch
        .run_loop(
            &Session::new("analyze the add function".into()).expecting_writes(false),
            &reg,
            &root,
        )
        .await;

    assert_eq!(out["passed"], true);
    assert_eq!(out["tasks"][0]["writes_made"], 0);
    let tos: Vec<String> = transitions(&trace).into_iter().map(|(_, to)| to).collect();
    assert!(
        !tos.iter().any(|to| to == "rejected_oracle_modified"),
        "expect_writes false must not trip the oracle gate: {tos:?}"
    );
    assert!(
        !tos.iter().any(|to| to == "rejected_no_writes"),
        "expect_writes false must not trip the write gate either: {tos:?}"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// The two refusal reasons must stay distinguishable in the trace: round 1
/// writes nothing (no-writes), round 2 tampers the test (oracle). Different
/// causes, different transitions, one run.
#[tokio::test]
async fn the_two_refusal_reasons_stay_distinguishable_in_the_trace() {
    let (orch, reg, root, trace) = harness(
        Arc::new(OracleClient::new(Action::NoWritesThenTamper)),
        "both-refusals",
        2,
    );
    let out = orch
        .run_loop(&Session::new("make the test pass".into()), &reg, &root)
        .await;

    assert_eq!(out["passed"], false);
    let tos = transitions(&trace);
    assert!(
        tos.iter().any(|(_, to)| to == "rejected_no_writes"),
        "round 1 wrote nothing, so the no-writes reason must fire: {tos:?}"
    );
    assert!(
        tos.iter().any(|(_, to)| to == "rejected_oracle_modified"),
        "round 2 tampered the test, so the oracle reason must fire: {tos:?}"
    );
    // Order matters as much as presence: the no-writes refusal precedes the
    // oracle refusal, because they are consecutive rounds of one run.
    let nw = tos
        .iter()
        .position(|(_, to)| to == "rejected_no_writes")
        .expect("no-writes refusal recorded");
    let orc = tos
        .iter()
        .position(|(_, to)| to == "rejected_oracle_modified")
        .expect("oracle refusal recorded");
    assert!(nw < orc, "the refusals are out of round order: {tos:?}");
    std::fs::remove_dir_all(&root).ok();
}

/// Requirement 4: the oracle gate refuses round 1's tamper, and the existing
/// pre-retry rollback is what reverts the tampered test — no new restore code
/// is needed. Round 2 then fixes the code and passes against an intact suite,
/// which is only possible because the baseline assertion was restored.
#[tokio::test]
async fn the_existing_pre_retry_rollback_reverts_the_tampered_oracle() {
    let (orch, reg, root, trace) = harness(
        Arc::new(OracleClient::new(Action::TamperThenFix)),
        "rollback-restores",
        2,
    );
    let out = orch
        .run_loop(&Session::new("make the test pass".into()), &reg, &root)
        .await;

    assert_eq!(out["passed"], true, "the run recovers via the honest fix");
    assert_eq!(out["rounds"], 2);
    let tos: Vec<String> = transitions(&trace).into_iter().map(|(_, to)| to).collect();
    assert!(
        tos.iter().any(|to| to == "rejected_oracle_modified"),
        "round 1's tamper must be refused: {tos:?}"
    );
    assert!(
        tos.iter().any(|to| to == "rolled_back"),
        "the pre-retry rollback must run between the rounds: {tos:?}"
    );
    // The load-bearing assertion: the tamper is gone from the tree, so the
    // suite the run is scored against is intact. The fix is what remains.
    assert_eq!(
        std::fs::read_to_string(root.join("tests/oracle.rs")).unwrap(),
        BASELINE_TEST,
        "the rollback did not restore the tampered oracle"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
        "round 2's fix must land"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// Direct mode has no reviewer, so the configured check suite — usually a test
/// command — *is* the oracle. That makes #319's shape the primary risk there
/// rather than a secondary one, so the same rule applies and the same
/// transition is emitted.
#[tokio::test]
async fn direct_mode_refuses_a_tampered_baseline_test_too() {
    let client = Arc::new(OracleClient::new(Action::TamperTest));
    let (orch, reg, root, trace) = harness_with(client.clone(), "direct-tamper", 1, |_| {}, true);
    let out = orch
        .run_loop(
            &Session::new("make the test pass".into())
                .with_checks(vec!["echo checked".to_string()]),
            &reg,
            &root,
        )
        .await;

    assert_eq!(
        out["passed"], false,
        "direct mode must refuse the tamper too"
    );
    let fb = out["tasks"][0]["feedback"]
        .as_str()
        .unwrap_or("NO FEEDBACK");
    assert!(
        fb.starts_with("harness: pass rejected"),
        "the harness must be named as the refuser: {fb}"
    );
    assert!(
        fb.contains("tests/oracle.rs"),
        "the feedback must name the offending path: {fb}"
    );
    let tos: Vec<String> = transitions(&trace).into_iter().map(|(_, to)| to).collect();
    assert!(
        tos.iter().any(|to| to == "rejected_oracle_modified"),
        "direct mode must emit the same refusal transition: {tos:?}"
    );
    std::fs::remove_dir_all(&root).ok();
}
