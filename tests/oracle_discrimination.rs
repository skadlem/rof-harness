//! The live-path oracle discrimination probe (P3 build item 6).
//!
//! The report's item 6 came from Fowler: a file with 100% statement coverage
//! and no unit tests, where Stryker reported 13 survivors — coverage is not
//! verification. This is NOT that. A real mutation engine re-runs the suite
//! per mutant, and this session established that token/spend explains most
//! of the variance between arms, so an expensive verifier inside the run
//! contradicts the cost work. The eval path already owns the property
//! (`src/eval/suite.rs::oracle_ok`, over the baseline the eval runner
//! captures before the loop). What was missing is narrower: the LIVE path
//! (`run` / `chat`) never asks whether its oracle discriminates at all, and
//! this repo has been bitten by a vacuous check once — STATUS records
//! "vacuous checks_pass(&[])" as a defect that shipped.
//!
//! The property: after a task's checks PASS, run the SAME commands against
//! the restored pre-change baseline. If they pass there too, the oracle
//! proves nothing about this change. That is fault injection at the ORACLE
//! level, not source mutation: one extra execution of commands the task
//! already ran, only when the task already passed, and only behind a knob
//! that is off by default (a default run must stay byte-identical).
//!
//! Reported, never a gate. Plenty of legitimate tasks — "add a comment",
//! "rename" — have checks that pass at baseline, and silently refusing them
//! would be the harness making a judgement call it has no evidence for.
//! `a_vacuous_oracle_is_reported_and_the_task_still_passes` pins that.
//!
//! Everything here runs headless against a real temp git workdir with a
//! canned fake model: no network, no PTY, no sleeps.

use async_trait::async_trait;
use rof::config::{AppConfig, PermissionPolicy};
use rof::engine::{Orchestrator, Session};
use rof::llm::{ContextService, ExecutorService, LlmClient, LlmError, LlmReq, LlmResp};
use rof::obs::{TraceEvent, TraceSink};
use rof::tools::{FsListTool, FsPatchTool, FsReadTool, FsWriteTool, ProcRunTool, ToolRegistry};
use std::path::Path;
use std::sync::Arc;

/// The seeded baseline: a subtract where an add belongs, plus a state marker
/// the check can look for. `grep -q FIXED src/lib.rs` therefore FAILS on the
/// pristine tree and PASSES once the model fixes it — a discriminating
/// oracle, built out of nothing but a real command and a real git baseline.
const BASELINE_LIB: &str = "// STATE broken\npub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n";
const FIXED_LIB: &str = "// STATE FIXED\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
/// A file the model CREATES, so the probe's restore has an untracked path to
/// put back. `git clean -fd` deletes it during the probe; a probe that did
/// not restore would leave it gone.
const NEW_FILE: &str = "pub const NEW: u8 = 1;\n";
const NEW_FILE_PATH: &str = "src/new.rs";

/// The canned implementer move: fix the marker AND create a new file, so a
/// run exercises both restore shapes (modified tracked path, untracked new
/// path) in one pass.
const FIX_AND_ADD: &str = r#"{"patches":[{"path":"src/lib.rs","search":"// STATE broken","replace":"// STATE FIXED"},{"path":"src/lib.rs","search":"a - b","replace":"a + b"}],"writes":[{"path":"src/new.rs","content":"pub const NEW: u8 = 1;\n"}],"notes":"fixed the add and added the constant"}"#;

/// The check that discriminates: red on the baseline, green after the fix.
const DISCRIMINATING_CHECK: &str = "grep -q FIXED src/lib.rs";
/// The check that cannot: green on the baseline and green after, so it says
/// nothing about this change — the vacuous-oracle case.
const VACUOUS_CHECK: &str = "echo checked";

struct ProbeClient {
    reviewer_passes: bool,
}

impl ProbeClient {
    fn new() -> Self {
        Self {
            reviewer_passes: true,
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
impl LlmClient for ProbeClient {
    async fn complete(&self, _model: &str, req: LlmReq) -> Result<LlmResp, LlmError> {
        if req.system.contains("reviewer") {
            let verdict = if self.reviewer_passes {
                r#"{"pass": true, "feedback": "looks right"}"#
            } else {
                r#"{"pass": false, "feedback": "not yet"}"#
            };
            return Self::resp(verdict);
        }
        Self::resp(FIX_AND_ADD)
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rof-probe-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), BASELINE_LIB).unwrap();
    dir
}

/// `probe: Option<&str>` names the check to configure; `None` configures no
/// check at all (the "nothing to probe" case). `knob` sets the config flag
/// under test; `direct` builds the direct-mode loop.
fn harness(
    tag: &str,
    probe: Option<&str>,
    knob: bool,
    direct: bool,
) -> (
    Orchestrator,
    ToolRegistry,
    std::path::PathBuf,
    Arc<TraceSink>,
) {
    let root = scratch(tag);
    let client = Arc::new(ProbeClient::new());
    let allowed: Vec<String> = probe.iter().map(|c| c.to_string()).collect();
    let policy = PermissionPolicy {
        allowed_dirs: vec![root.clone()],
        allowed_commands: allowed.clone(),
        ..Default::default()
    };
    let mut reg = ToolRegistry::new(policy);
    reg.register(FsListTool::new(root.clone()));
    reg.register(FsReadTool::new(root.clone()));
    reg.register(FsPatchTool::new(root.clone()));
    reg.register(FsWriteTool::new(root.clone()));
    reg.register(ProcRunTool::new(root.clone(), allowed));

    let mut cfg = AppConfig {
        max_review_rounds: 1,
        budgets: rof::config::TokenBudgets {
            long_term: 2000,
            mid_term: 4000,
            short_term: 6000,
        },
        oracle_discrimination: knob,
        ..Default::default()
    };
    if direct {
        cfg.execution = "direct".to_string();
    }
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

fn session(probe: Option<&str>, expect_writes: bool) -> Session {
    let s = Session::new("fix the add function in src/lib.rs".into());
    let s = match probe {
        Some(cmd) => s.with_checks(vec![cmd.to_string()]),
        None => s,
    };
    s.expecting_writes(expect_writes)
}

/// The one event the probe emits, in order, so a test can say "the probe
/// reported" rather than "a string appeared somewhere".
fn probe_events(trace: &TraceSink) -> Vec<TraceEvent> {
    trace
        .events()
        .into_iter()
        .filter(|e| matches!(e, TraceEvent::OracleDiscrimination { .. }))
        .collect()
}

/// How many times a check command actually ran, counted from the trace's own
/// tool events. This is the cost the knob buys, measured rather than
/// asserted in prose.
fn check_runs(trace: &TraceSink) -> usize {
    trace
        .events()
        .iter()
        .filter(|e| matches!(e, TraceEvent::ToolCall { tool, .. } if tool == "proc.run"))
        .count()
}

/// Every file under `root` except `.git`, as `relpath -> content`, so a test
/// can compare the work root as a whole rather than one file it picked.
fn work_root_files(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if path.is_dir() {
                walk(&path, base, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                let rel = path
                    .strip_prefix(base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, bytes));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn clean(root: &Path) {
    std::fs::remove_dir_all(root).ok();
}

/// A check that FAILS on the baseline discriminates: the probe must say so.
/// The task's own pass is untouched by that report.
#[tokio::test]
async fn a_check_that_fails_at_the_baseline_reports_a_discriminating_oracle() {
    let (orch, reg, root, trace) =
        harness("discriminating", Some(DISCRIMINATING_CHECK), true, false);
    let out = orch
        .run_loop(&session(Some(DISCRIMINATING_CHECK), true), &reg, &root)
        .await;

    assert_eq!(out["passed"], true, "the fix passes its own check");
    let events = probe_events(&trace);
    assert_eq!(events.len(), 1, "one probe, one event: {events:?}");
    match &events[0] {
        TraceEvent::OracleDiscrimination {
            task,
            vacuous,
            failed_at_baseline,
            restored,
        } => {
            assert_eq!(task, "fix the add function in src/lib.rs");
            assert!(
                vacuous.is_empty(),
                "a red-at-baseline check is not vacuous: {vacuous:?}"
            );
            assert_eq!(
                *failed_at_baseline,
                vec![DISCRIMINATING_CHECK.to_string()],
                "the check is named as the discriminating one"
            );
            assert!(*restored, "the probe must put the tree back");
        }
        other => panic!("wrong event: {other:?}"),
    }
    // The same finding on the task result, so a consumer reading `tasks` does
    // not have to know about the trace to see it.
    assert_eq!(
        out["tasks"][0]["oracle_probe"]["failed_at_baseline"],
        serde_json::json!([DISCRIMINATING_CHECK]),
    );
    assert_eq!(
        out["tasks"][0]["oracle_probe"]["vacuous"],
        serde_json::json!([] as [String; 0]),
    );
    clean(&root);
}

/// A check that PASSES at baseline is vacuous: the probe says so — and the
/// run still passes. This is the load-bearing requirement: a vacuous oracle
/// is a REPORTED signal, never a gate, because "add a comment" and "rename"
/// are legitimate tasks whose checks pass on a clean tree.
#[tokio::test]
async fn a_vacuous_oracle_is_reported_and_the_task_still_passes() {
    let (orch, reg, root, trace) = harness("vacuous", Some(VACUOUS_CHECK), true, false);
    let out = orch
        .run_loop(&session(Some(VACUOUS_CHECK), true), &reg, &root)
        .await;

    assert_eq!(
        out["passed"], true,
        "a vacuous oracle must NOT flip the pass: the harness has no evidence \
         the work was not done"
    );
    let events = probe_events(&trace);
    assert_eq!(events.len(), 1, "the vacuity is reported, not swallowed");
    match &events[0] {
        TraceEvent::OracleDiscrimination { vacuous, .. } => assert_eq!(
            vacuous,
            &vec![VACUOUS_CHECK.to_string()],
            "the check that passes at baseline is named"
        ),
        other => panic!("wrong event: {other:?}"),
    }
    assert_eq!(
        out["tasks"][0]["oracle_probe"]["vacuous"],
        serde_json::json!([VACUOUS_CHECK]),
    );
    // And the work is genuinely there: the run did not pass by doing nothing.
    assert_eq!(read(&root, "src/lib.rs"), FIXED_LIB);
    assert_eq!(read(&root, NEW_FILE_PATH), NEW_FILE);
    clean(&root);
}

/// The cost: the probe re-runs the SAME commands the task already ran, so a
/// passing task with one check executes it twice. Asserted by counting the
/// trace's own tool events, not by prose.
#[tokio::test]
async fn the_probe_costs_exactly_one_extra_check_run() {
    let (orch, reg, root, trace) = harness("cost", Some(DISCRIMINATING_CHECK), true, false);
    let out = orch
        .run_loop(&session(Some(DISCRIMINATING_CHECK), true), &reg, &root)
        .await;
    assert_eq!(out["passed"], true);
    assert_eq!(
        check_runs(&trace),
        2,
        "one run for the task, one for the baseline probe — and no third"
    );
    clean(&root);
}

/// The work root after the probe must be byte-identical to what it was
/// before it: the harness hashes the changed files rather than trusting the
/// order of its own operations. A probe that can leave a user's tree
/// modified is worse than no probe at all.
#[tokio::test]
async fn the_work_root_is_byte_identical_after_the_probe() {
    let (orch, reg, root, trace) = harness("restore", Some(DISCRIMINATING_CHECK), true, false);
    let out = orch
        .run_loop(&session(Some(DISCRIMINATING_CHECK), true), &reg, &root)
        .await;
    assert_eq!(out["passed"], true);
    assert_eq!(probe_events(&trace).len(), 1, "the probe actually ran");

    // What the run was supposed to leave behind, byte for byte.
    let expected: Vec<(String, Vec<u8>)> = vec![
        ("src/lib.rs".to_string(), FIXED_LIB.as_bytes().to_vec()),
        (NEW_FILE_PATH.to_string(), NEW_FILE.as_bytes().to_vec()),
    ];
    assert_eq!(
        work_root_files(&root),
        expected,
        "the probe must leave the work root exactly as the run left it"
    );
    assert_eq!(read(&root, "src/lib.rs"), FIXED_LIB);
    assert_eq!(read(&root, NEW_FILE_PATH), NEW_FILE);
    clean(&root);
}

/// The dedicated "wired but not restored" guard. If the probe rolled the tree
/// back to the baseline and never put the change back, `src/lib.rs` would
/// read `a - b`, `src/new.rs` would be missing (the rollback's `git clean`
/// took it), and the event's own `restored` flag would be false. All three
/// are asserted, so removing the restore fails this test.
#[tokio::test]
async fn a_probe_that_did_not_restore_the_tree_would_fail_this() {
    let (orch, reg, root, trace) = harness("not-restored", Some(DISCRIMINATING_CHECK), true, false);
    let out = orch
        .run_loop(&session(Some(DISCRIMINATING_CHECK), true), &reg, &root)
        .await;

    assert_eq!(
        read(&root, "src/lib.rs"),
        FIXED_LIB,
        "the fix survived the probe: an unrestored tree reads as the baseline"
    );
    assert!(
        root.join(NEW_FILE_PATH).exists(),
        "the untracked file the model created survived the probe's git clean"
    );
    match &probe_events(&trace)[0] {
        TraceEvent::OracleDiscrimination { restored, .. } => {
            assert!(*restored, "the probe verified its own restore by hash")
        }
        other => panic!("wrong event: {other:?}"),
    }
    // The change set the run reported is still the truth on disk, which is
    // the disagreement a pane must never be able to show.
    let reported: Vec<String> = out["tasks"][0]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let on_disk: Vec<String> = work_root_files(&root)
        .into_iter()
        .map(|(rel, _)| rel)
        .collect();
    for name in &reported {
        assert!(
            on_disk.contains(name),
            "reported change {name} is not on disk: {on_disk:?}"
        );
    }
    clean(&root);
}

/// Direct mode gets the same probe, for the same reason the #319 gate is
/// stricter there: with no reviewer, the configured check suite IS the
/// oracle, so a vacuous one is at least as dangerous. Same helper, same
/// skips, so the two loops cannot drift.
#[tokio::test]
async fn direct_mode_probes_the_oracle_too() {
    // Discriminating: reported as discriminating, run passes.
    let (orch, reg, root, trace) = harness("direct-disc", Some(DISCRIMINATING_CHECK), true, true);
    let out = orch
        .run_loop(&session(Some(DISCRIMINATING_CHECK), true), &reg, &root)
        .await;
    assert_eq!(out["passed"], true);
    match &probe_events(&trace)[0] {
        TraceEvent::OracleDiscrimination {
            vacuous,
            failed_at_baseline,
            ..
        } => {
            assert!(vacuous.is_empty());
            assert_eq!(failed_at_baseline.len(), 1);
        }
        other => panic!("wrong event: {other:?}"),
    }
    assert_eq!(
        read(&root, "src/lib.rs"),
        FIXED_LIB,
        "and the tree is restored"
    );
    clean(&root);

    // Vacuous: reported, and still a pass.
    let (orch, reg, root, trace) = harness("direct-vac", Some(VACUOUS_CHECK), true, true);
    let out = orch
        .run_loop(&session(Some(VACUOUS_CHECK), true), &reg, &root)
        .await;
    assert_eq!(out["passed"], true, "direct mode must not gate on vacuity");
    match &probe_events(&trace)[0] {
        TraceEvent::OracleDiscrimination { vacuous, .. } => {
            assert_eq!(vacuous, &vec![VACUOUS_CHECK.to_string()])
        }
        other => panic!("wrong event: {other:?}"),
    }
    clean(&root);
}

/// Skip where the probe cannot apply, and emit nothing when it skips: a
/// silent skip must be indistinguishable from a probe that found a
/// discriminating oracle, which is exactly why the event is only ever emitted
/// when a probe actually ran.
#[tokio::test]
async fn the_probe_is_skipped_silently_where_it_cannot_apply() {
    // No checks configured: there is no oracle to test.
    let (orch, reg, root, trace) = harness("skip-no-checks", None, true, false);
    let out = orch.run_loop(&session(None, true), &reg, &root).await;
    assert_eq!(out["passed"], true);
    assert!(
        probe_events(&trace).is_empty(),
        "no checks, no probe, no event"
    );
    assert_eq!(check_runs(&trace), 0, "and no check was executed");
    assert!(
        out["tasks"][0].get("oracle_probe").is_none(),
        "and the task result says nothing was measured"
    );
    clean(&root);

    // expect_writes == false: an analysis task makes no claim a check could
    // discriminate, so the probe would answer a question nobody asked.
    let (orch, reg, root, trace) =
        harness("skip-no-writes", Some(DISCRIMINATING_CHECK), true, false);
    let out = orch
        .run_loop(&session(Some(DISCRIMINATING_CHECK), false), &reg, &root)
        .await;
    assert_eq!(out["passed"], true);
    assert!(
        probe_events(&trace).is_empty(),
        "expect_writes false must not emit a probe event"
    );
    assert_eq!(check_runs(&trace), 1, "only the task's own check ran");
    assert!(out["tasks"][0].get("oracle_probe").is_none());
    clean(&root);

    // A run that did not pass is not probed either: the property is stated
    // about an oracle that just granted a pass.
    let (orch, reg, root, trace) =
        harness("skip-knob-off", Some(DISCRIMINATING_CHECK), false, false);
    let out = orch
        .run_loop(&session(Some(DISCRIMINATING_CHECK), true), &reg, &root)
        .await;
    assert_eq!(out["passed"], true);
    assert!(
        probe_events(&trace).is_empty(),
        "the knob is off, so there is no probe and no event"
    );
    assert_eq!(
        check_runs(&trace),
        1,
        "the default run executes its checks exactly once — no extra cost"
    );
    assert!(
        out["tasks"][0].get("oracle_probe").is_none(),
        "an unmeasured task result must not grow a key: the default run stays \
         byte-identical to one from before the probe existed"
    );
    clean(&root);
}

/// The knob is off by default and an old config file — one written before
/// this field existed — still loads with the probe off. The two are the same
/// claim seen from the config side.
#[test]
fn the_knob_defaults_off_and_an_old_config_still_loads() {
    assert!(
        !AppConfig::default().oracle_discrimination,
        "the probe must be opt-in: it doubles the check executions of every \
         passing task"
    );

    let dir = scratch("config");
    let path = dir.join("old.json");
    // Exactly the shape of a config written before the knob existed.
    std::fs::write(
        &path,
        r#"{"max_review_rounds": 2, "execution": "pipeline", "cost_lambda": 0.5}"#,
    )
    .unwrap();
    let cfg = AppConfig::load(&path).unwrap();
    assert_eq!(
        cfg.max_review_rounds, 2,
        "the old file still means what it said"
    );
    assert!(
        !cfg.oracle_discrimination,
        "an old config gets the default, not a surprise probe"
    );
    // And the canonical dump round-trips, so `rof config` output stays
    // diffable against a versioned config.
    let dumped = dir.join("new.json");
    std::fs::write(&dumped, cfg.to_json()).unwrap();
    assert_eq!(AppConfig::load(&dumped).unwrap().to_json(), cfg.to_json());
    clean(&dir);
}
