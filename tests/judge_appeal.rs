//! Build item 7: the judge appeal — ONE compact second verdict, at the
//! task's failure boundary, and only where every deterministic signal is
//! green.
//!
//! The judge is measured to flip on ~13.6% of identical re-runs, so a single
//! verdict is under-powered. Majority aggregation is the textbook fix and is
//! refused here: 11 samples is 11x the reviewer cost, and this harness treats
//! spend as the dominant quality axis. The appeal buys one sample, and only
//! where a false FAILURE is already the expensive outcome.
//!
//! What these tests hold:
//!   * the conditions for appealing, and a refusal for each of them;
//!   * the bound — exactly one extra verdict per task, ever;
//!   * that a disagreement is RECORDED, since the count of disagreements is the
//!     harness's own measurement of its judge instability;
//!   * that the default is load-bearing, so the knob cannot rot into a no-op.
//!
//! Hermetic: real temp git workdir, canned fake model, no network, no
//! terminal, no sleeps.

use async_trait::async_trait;
use rof::config::{AppConfig, PermissionPolicy};
use rof::engine::{Orchestrator, Session};
use rof::llm::{ContextService, ExecutorService, LlmClient, LlmError, LlmReq, LlmResp};
use rof::obs::{TraceEvent, TraceSink};
use rof::tools::{FsListTool, FsPatchTool, FsReadTool, ProcRunTool, ToolRegistry};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// What the fake reviewer says, in call order. The last value repeats, so a
/// test that wants exactly one disagreement names two values and a test that
/// wants stability names one.
struct Fake {
    /// One verdict per reviewer call, consumed in order; the last repeats.
    verdicts: Mutex<Vec<(bool, &'static str)>>,
    /// Reviewer calls so far.
    calls: AtomicUsize,
    /// Reviewer verdicts, in order, for assertions about the bound.
    seen: Mutex<Vec<bool>>,
    /// When true, the implementer also weakens a baseline test file — the
    /// oracle-tamper case. It has to happen INSIDE the implementer's turn,
    /// because the orchestrator commits its own baseline before the round and
    /// would otherwise absorb a tamper the test made beforehand.
    tamper_oracle: bool,
}

impl Fake {
    fn new(verdicts: &[(bool, &'static str)]) -> Arc<Fake> {
        Self::with_tamper(verdicts, false)
    }

    fn with_tamper(verdicts: &[(bool, &'static str)], tamper_oracle: bool) -> Arc<Fake> {
        Arc::new(Fake {
            verdicts: Mutex::new(verdicts.to_vec()),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
            tamper_oracle,
        })
    }

    fn reviewer_calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn seen(&self) -> Vec<bool> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmClient for Fake {
    async fn complete(&self, _model: &str, req: LlmReq) -> Result<LlmResp, LlmError> {
        if req.system.contains("Compress the input") {
            return Ok(resp("condensed".into()));
        }
        if req.system.contains("reviewer") || req.prompt.contains("APPEAL task:") {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let (pass, note) = {
                let v = self.verdicts.lock().unwrap();
                let i = n.min(v.len().saturating_sub(1));
                v.get(i).copied().unwrap_or((false, "no verdict"))
            };
            self.seen.lock().unwrap().push(pass);
            return Ok(resp(
                serde_json::json!({"pass": pass, "feedback": note}).to_string(),
            ));
        }
        if req.system.contains("implementer") || req.system.contains("direct coding agent") {
            // A real write, so the write gate is satisfied: the point of the
            // appeal is that the DETERMINISTIC evidence is green.
            if self.tamper_oracle {
                return Ok(resp(
                    concat!(
                        r#"{"artifact":"did the work","notes":"ok","#,
                        r#""patches":[{"path":"src/lib.rs","search":"a - b","replace":"a + b"},"#,
                        r#"{"path":"tests/spec.rs","search":"fn t() {}","replace":"fn t() { /* weakened */ }"}"#,
                        r#"]}"#
                    )
                    .into(),
                ));
            }
            return Ok(resp(
                r#"{"artifact":"did the work","notes":"ok","patches":[{"path":"src/lib.rs","search":"a - b","replace":"a + b"}]}"#
                    .into(),
            ));
        }
        Ok(resp(r#"{"artifact":"ok"}"#.into()))
    }
}

fn resp(text: String) -> LlmResp {
    LlmResp {
        text,
        input_tokens: 10,
        output_tokens: 5,
        latency_ms: 1,
        cost_usd: None,
        cached_input_tokens: 0,
        attempts: 1,
    }
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("rof-appeal-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n",
    )
    .unwrap();
    root
}

fn harness(
    client: Arc<Fake>,
    tag: &str,
    max_rounds: u32,
    appeal: bool,
) -> (
    Orchestrator,
    ToolRegistry,
    std::path::PathBuf,
    Arc<TraceSink>,
) {
    let root = scratch(tag);
    let policy = PermissionPolicy {
        allowed_dirs: vec![root.clone()],
        allowed_commands: vec!["echo checked".to_string(), "false".to_string()],
        ..Default::default()
    };
    let mut reg = ToolRegistry::new(policy);
    reg.register(FsListTool::new(root.clone()));
    reg.register(FsReadTool::new(root.clone()));
    reg.register(FsPatchTool::new(root.clone()));
    reg.register(ProcRunTool::new(
        root.clone(),
        vec!["echo checked".to_string(), "false".to_string()],
    ));
    let mut cfg = AppConfig {
        max_review_rounds: max_rounds,
        judge_appeal: appeal,
        budgets: rof::config::TokenBudgets {
            long_term: 2000,
            mid_term: 4000,
            short_term: 6000,
        },
        ..Default::default()
    };
    // The discrimination probe stays out of it: this task is about the judge,
    // and the probe has its own tests.
    cfg.oracle_discrimination = false;
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

fn appeals(trace: &TraceSink) -> Vec<(String, bool, bool)> {
    trace
        .events()
        .iter()
        .filter_map(|e| match e {
            TraceEvent::JudgeAppeal {
                task,
                first_pass,
                second_pass,
            } => Some((task.clone(), *first_pass, *second_pass)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_flaky_judge_whose_checks_are_green_is_appealed_and_promoted() {
    let client = Fake::new(&[(false, "not done"), (true, "on reflection it is")]);
    let (orch, reg, root, trace) = harness(client.clone(), "promote", 1, true);
    let out = orch
        .run_loop(
            &Session::new("make the thing right".into())
                .with_checks(vec!["echo checked".to_string()])
                .expecting_writes(true),
            &reg,
            &root,
        )
        .await;

    assert_eq!(out["passed"], true, "the appeal should promote the task");
    let seen = appeals(&trace);
    assert_eq!(
        seen,
        vec![("make the thing right".to_string(), false, true)],
        "the disagreement must be recorded exactly once: {seen:?}"
    );
    // The bound: ONE extra reviewer call, and the first was the round's own.
    assert_eq!(client.reviewer_calls(), 2, "one round + one appeal");
    assert_eq!(
        client.seen(),
        vec![false, true],
        "the second verdict is the one that promoted it"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn with_the_appeal_disabled_the_same_task_still_fails() {
    let client = Fake::new(&[(false, "not done"), (true, "on reflection it is")]);
    let (orch, reg, root, trace) = harness(client.clone(), "off", 1, false);
    let out = orch
        .run_loop(
            &Session::new("make the thing right".into())
                .with_checks(vec!["echo checked".to_string()])
                .expecting_writes(true),
            &reg,
            &root,
        )
        .await;

    assert_eq!(out["passed"], false, "the knob must be load-bearing");
    assert!(
        appeals(&trace).is_empty(),
        "a disabled appeal records nothing"
    );
    assert_eq!(client.reviewer_calls(), 1, "no extra call when disabled");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn a_failing_check_means_no_appeal_however_many_verdicts_agree() {
    let client = Fake::new(&[(false, "not done"), (true, "looks fine")]);
    let (orch, reg, root, trace) = harness(client.clone(), "redcheck", 1, true);
    let out = orch
        .run_loop(
            &Session::new("make the thing right".into())
                .with_checks(vec!["false".to_string()])
                .expecting_writes(true),
            &reg,
            &root,
        )
        .await;

    assert_eq!(out["passed"], false, "a red check is never appealed away");
    assert!(
        appeals(&trace).is_empty(),
        "no green evidence, no appeal: {:?}",
        appeals(&trace)
    );
    assert_eq!(client.reviewer_calls(), 1, "the refusal costs nothing");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn a_stable_judge_still_gets_exactly_one_appeal() {
    let client = Fake::new(&[(false, "not done")]);
    let (orch, reg, root, trace) = harness(client.clone(), "stable", 1, true);
    let out = orch
        .run_loop(
            &Session::new("make the thing right".into())
                .with_checks(vec!["echo checked".to_string()])
                .expecting_writes(true),
            &reg,
            &root,
        )
        .await;

    assert_eq!(out["passed"], false, "both verdicts said no");
    // The appeal is not skipped merely because the first verdict was "no" —
    // it cannot know in advance that the second will agree.
    assert_eq!(client.reviewer_calls(), 2, "one round + one appeal");
    // Both said no, so nothing was recorded: the event fires only on a
    // disagreement.
    assert!(
        appeals(&trace).is_empty(),
        "agreement is not a disagreement: {:?}",
        appeals(&trace)
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn no_checks_configured_means_no_appeal() {
    let client = Fake::new(&[(false, "not done"), (true, "fine")]);
    let (orch, reg, root, trace) = harness(client.clone(), "nocheck", 1, true);
    orch.run_loop(
        &Session::new("make the thing right".into()).expecting_writes(true),
        &reg,
        &root,
    )
    .await;
    assert!(
        appeals(&trace).is_empty(),
        "with no checks there is no green evidence to appeal with"
    );
    assert_eq!(client.reviewer_calls(), 1, "and it costs nothing");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn a_tampered_oracle_means_no_appeal() {
    let client = Fake::with_tamper(&[(false, "not done"), (true, "fine")], true);
    let (orch, reg, root, trace) = harness(client.clone(), "tamper", 1, true);
    // The suite exists at baseline, so the implementer's edit to it is a
    // TRACKED modification of a test-shaped path — the protected case.
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(root.join("tests/spec.rs"), "#[test]\nfn t() {}\n").unwrap();
    let tree = rof::engine::TreeService::new(root.clone());
    tree.ensure().unwrap();
    tree.baseline().unwrap();

    orch.run_loop(
        &Session::new("make the thing right".into())
            .with_checks(vec!["echo checked".to_string()])
            .expecting_writes(true),
        &reg,
        &root,
    )
    .await;

    assert!(
        appeals(&trace).is_empty(),
        "a modified oracle is never appealed past: {:?}",
        appeals(&trace)
    );
    assert_eq!(client.reviewer_calls(), 1, "the refusal costs nothing");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn direct_mode_has_no_judge_and_so_never_appeals() {
    let client = Fake::new(&[(false, "not done"), (true, "fine")]);
    let (mut orch, reg, root, trace) = harness(client.clone(), "direct", 1, true);
    orch.set_execution_for_test("direct");
    // Direct mode's verdict IS the check suite: there is no judge, so the
    // run finishes on its own evidence and no second opinion is bought.
    let _ = orch
        .run_loop(
            &Session::new("make the thing right".into())
                .with_checks(vec!["echo checked".to_string()])
                .expecting_writes(true),
            &reg,
            &root,
        )
        .await;
    assert!(
        appeals(&trace).is_empty(),
        "direct mode has no judge to appeal to: {:?}",
        appeals(&trace)
    );
    assert_eq!(client.reviewer_calls(), 0, "no reviewer ran at all");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn the_default_is_off_and_an_old_config_without_the_key_agrees() {
    // The appeal only ever runs where a task is already FAILING, so the flip
    // it buys can manufacture a false pass — on a task whose checks are green
    // but weak, that is exactly the outcome this harness exists to prevent.
    // The promotion is therefore opt-in; the disagreement measurement is not
    // lost by that, because the event is recorded whenever the appeal runs.
    assert!(
        !AppConfig::default().judge_appeal,
        "the promotion must be opt-in: it is a gamble in the dangerous direction"
    );
    // A config file written before this knob existed must still load, and must
    // land on the same safe default rather than a surprise.
    let legacy = serde_json::json!({"max_review_rounds": 2}).to_string();
    let cfg: AppConfig = serde_json::from_str(&legacy).expect("a pre-knob config loads");
    assert!(!cfg.judge_appeal, "a missing key must mean OFF");
}
