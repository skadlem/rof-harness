use async_trait::async_trait;
use rof::config::AppConfig;
use rof::engine::CheckResult;
use rof::eval::{
    compare, fnv1a_hex, git_head, ContextMetrics, EvalSuite, EvalTask, EvaluationRunner, RunLabel,
    SuiteReport, TaskChange, TaskResult,
};
use rof::llm::{ContextService, ExecutorService, LlmClient, LlmError, LlmReq, LlmResp};
use rof::obs::TraceSink;
use rof::skills::SkillPolicy;
use std::sync::Arc;

/// Config for a test runner: everything default except the skill store, which
/// is pinned to a scratch path. `AppConfig::default()` means `~/.rof/skills`,
/// so an unpinned test would read whatever the developer's machine happens to
/// have approved — a hidden input that changes prompts and metrics.
fn test_cfg() -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.skills.root =
        Some(std::env::temp_dir().join(format!("rof-test-skills-{}", std::process::id())));
    cfg.skills.policy = SkillPolicy::ReadOnly;
    cfg
}

struct Fake;

#[async_trait]
impl LlmClient for Fake {
    async fn complete(&self, _model: &str, req: LlmReq) -> Result<LlmResp, LlmError> {
        let text = if req.system.contains("reviewer") {
            "{\"pass\": true, \"feedback\": \"ok\"}"
        } else if req.system.contains("implementer") {
            // A real write: an expect_writes task must not pass on prose, and
            // the harness (not the reviewer) is the backstop for that.
            "{\"artifact\": \"x\", \"notes\": \"y\", \"writes\": [{\"path\": \"notes.md\", \"content\": \"updated\"}]}"
        } else {
            "{\"tasks\": [\"t\"], \"acceptance\": [\"a\"]}"
        };
        Ok(LlmResp {
            text: text.to_string(),
            input_tokens: 4,
            output_tokens: 2,
            latency_ms: 1,
            cost_usd: None,
            cached_input_tokens: 0,
            attempts: 1,
        })
    }
}

#[tokio::test]
async fn suite_tracks_per_task_match() {
    let root = std::env::temp_dir().join(format!("rof-eval-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("notes.md"), "eval target content").unwrap();
    let client = Arc::new(Fake);
    let runner = EvaluationRunner::new(
        Arc::new(TraceSink::new()),
        test_cfg(),
        ContextService::new(client.clone(), "fake-ctx".to_string()),
        ExecutorService::new(client, "fake-exec".to_string(), None),
        root.clone(),
    );
    let suite = EvalSuite {
        name: "t".to_string(),
        tasks: vec![
            EvalTask {
                name: "a".to_string(),
                goal: "eval target".to_string(),
                expect_pass: true,
                checks: Vec::new(),
                expect_writes: true,
                max_tokens: None,
            },
            EvalTask {
                name: "b".to_string(),
                goal: "eval target".to_string(),
                expect_pass: false,
                checks: Vec::new(),
                expect_writes: true,
                max_tokens: None,
            },
        ],
    };
    let rep = runner.run_suite(&suite).await;
    assert_eq!(rep.tasks.len(), 2);
    assert!(rep.tasks[0].matched);
    assert!(!rep.tasks[1].matched);
    assert_eq!(rep.matched(), 1);
    assert_eq!((rep.aggregate.tasks, rep.aggregate.passed), (2, 2));
    assert_eq!(
        (rep.aggregate.verdicts, rep.aggregate.passed_verdicts),
        (2, 2)
    );
    assert!(rep.aggregate.tool_calls > 0);
    // Isolation: the eval must never mutate the tree it was pointed at.
    assert_eq!(
        std::fs::read_to_string(root.join("notes.md")).unwrap(),
        "eval target content",
        "suite tasks run against copies, not the source tree"
    );
    cleanup_task_dirs(&["a", "b"]);
    std::fs::remove_dir_all(&root).ok();
}

/// Remove the per-task copies this process left in the temp dir.
/// Scoped by task name: tests in one binary share a PID, so an unscoped
/// sweep would delete another test's live copies mid-run.
fn cleanup_task_dirs(tags: &[&str]) {
    let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        let name = p.file_name().map(|n| n.to_string_lossy().to_string());
        let matches = name.map(|n| {
            tags.iter()
                .any(|t| n.starts_with(&format!("rof-task-{}-{t}-", std::process::id())))
        });
        if p.is_dir() && matches.unwrap_or(false) {
            std::fs::remove_dir_all(&p).ok();
        }
    }
}

/// A suite whose checks run a build leaves a full `target/` in every task
/// copy (~1 GB on a Rust repo). The default deletes the copy when the task
/// is done: leftover state is debug-only, and on a small tmpfs it has cost
/// real task failures (disk quota). `clean_task_dirs: false` keeps it.
#[tokio::test]
async fn task_dirs_are_removed_by_default() {
    let root = std::env::temp_dir().join(format!("rof-eval-clean-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("in.md"), "seed").unwrap();
    let copies = std::env::temp_dir().join(format!("rof-eval-clean-copies-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&copies);
    std::fs::create_dir_all(&copies).unwrap();

    let client = Arc::new(Fake);
    let runner = EvaluationRunner::new(
        Arc::new(TraceSink::new()),
        AppConfig {
            task_root: Some(copies.clone()),
            ..test_cfg()
        },
        ContextService::new(client.clone(), "fake-ctx".to_string()),
        ExecutorService::new(client, "fake-exec".to_string(), None),
        root.clone(),
    );
    let suite = EvalSuite {
        name: "clean".to_string(),
        tasks: vec![EvalTask {
            name: "clean".to_string(),
            goal: "write out.md".to_string(),
            expect_pass: true,
            checks: Vec::new(),
            expect_writes: true,
            max_tokens: None,
        }],
    };
    let rep = runner.run_suite(&suite).await;
    assert!(rep.tasks[0].matched, "the task should pass: {rep:?}");
    // The copy is gone once the task finished — nothing in it is needed to
    // interpret the report.
    assert!(
        std::fs::read_dir(&copies).unwrap().next().is_none(),
        "the task copy survived a clean_task_dirs run"
    );
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&copies).ok();
}

/// Fan-out: every task runs in its own copy of the tree, so two tasks
/// writing different paths cannot see or clobber each other's work — and
/// the source tree is never a target.
#[tokio::test]
async fn parallel_tasks_are_isolated_per_task_dir() {
    /// Picks the written file from the goal text in the prompt.
    struct GoalAware;
    #[async_trait]
    impl LlmClient for GoalAware {
        async fn complete(&self, _model: &str, req: LlmReq) -> Result<LlmResp, LlmError> {
            let text = if req.system.contains("reviewer") {
                "{\"pass\": true, \"feedback\": \"ok\"}".to_string()
            } else if req.system.contains("implementer") {
                let (path, content) = if req.prompt.contains("alpha") {
                    ("alpha.md", "A")
                } else {
                    ("beta.md", "B")
                };
                format!(
                    "{{\"artifact\":\"x\",\"notes\":\"y\",\"writes\":\
                     [{{\"path\":\"{path}\",\"content\":\"{content}\"}}]}}"
                )
            } else {
                "{\"tasks\": [\"t\"], \"acceptance\": [\"a\"]}".to_string()
            };
            Ok(LlmResp {
                text,
                input_tokens: 4,
                output_tokens: 2,
                latency_ms: 1,
                cost_usd: None,
                cached_input_tokens: 0,
                attempts: 1,
            })
        }
    }

    let root = std::env::temp_dir().join(format!("rof-eval-fanout-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("notes.md"), "source").unwrap();

    let client = Arc::new(GoalAware);
    let cfg = AppConfig {
        max_parallel_tasks: 2,
        // Keep the copies so isolation can be asserted on them afterwards;
        // production runs delete them (a suite whose checks build leaves a
        // full target/ per task).
        clean_task_dirs: false,
        ..test_cfg()
    };
    let runner = EvaluationRunner::new(
        Arc::new(TraceSink::new()),
        cfg,
        ContextService::new(client.clone(), "fake-ctx".to_string()),
        ExecutorService::new(client, "fake-exec".to_string(), None),
        root.clone(),
    );
    let mk = |name: &str, goal: &str| EvalTask {
        name: name.to_string(),
        goal: goal.to_string(),
        expect_pass: true,
        checks: Vec::new(),
        expect_writes: true,
        max_tokens: None,
    };
    let suite = EvalSuite {
        name: "fanout".to_string(),
        tasks: vec![
            mk("alpha", "do the alpha change"),
            mk("beta", "do the beta change"),
        ],
    };
    let rep = runner.run_suite(&suite).await;

    // Results keep suite order even though both tasks ran concurrently.
    assert_eq!(
        rep.tasks
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta"],
        "report order must not depend on finish order"
    );
    assert!(
        rep.tasks.iter().all(|t| t.matched),
        "both tasks must pass: {:?}",
        rep.tasks
    );

    // Source tree untouched; writes landed only in task copies.
    assert_eq!(
        std::fs::read_to_string(root.join("notes.md")).unwrap(),
        "source"
    );
    assert!(
        !root.join("alpha.md").exists() && !root.join("beta.md").exists(),
        "no write may land in the source tree"
    );

    // Exactly one copy per task, each holding exactly its own artifact.
    let mut alphas = 0;
    let mut betas = 0;
    let rd = std::fs::read_dir(std::env::temp_dir()).unwrap();
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let a = p.join("alpha.md").exists();
        let b = p.join("beta.md").exists();
        if !a && !b {
            continue; // a copy from another test in this process
        }
        assert!(
            a ^ b,
            "a task copy holds its own artifact only: {}",
            p.display()
        );
        if a {
            alphas += 1
        } else {
            betas += 1
        }
    }
    assert_eq!(
        (alphas, betas),
        (1, 1),
        "each writer must have its own tree (no cross-talk)"
    );
    cleanup_task_dirs(&["alpha", "beta"]);
    std::fs::remove_dir_all(&root).ok();
}

/// The write gate is harness-side: a reviewer that says "pass" on empty work
/// must still produce a mismatch (the prompt asks; the harness enforces).
#[tokio::test]
async fn harness_rejects_pass_without_writes() {
    struct NoWrite;
    #[async_trait]
    impl LlmClient for NoWrite {
        async fn complete(&self, _model: &str, req: LlmReq) -> Result<LlmResp, LlmError> {
            let text = if req.system.contains("reviewer") {
                "{\"pass\": true, \"feedback\": \"looks fine to me\"}"
            } else if req.system.contains("implementer") {
                "{\"artifact\": \"prose only\", \"notes\": \"no writes\"}"
            } else {
                "{\"tasks\": [\"t\"], \"acceptance\": [\"a\"]}"
            };
            Ok(LlmResp {
                text: text.to_string(),
                input_tokens: 4,
                output_tokens: 2,
                latency_ms: 1,
                cost_usd: None,
                cached_input_tokens: 0,
                attempts: 1,
            })
        }
    }
    let root = std::env::temp_dir().join(format!("rof-eval-nowrite-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let client = Arc::new(NoWrite);
    let sink = Arc::new(TraceSink::new());
    let runner = EvaluationRunner::new(
        sink.clone(),
        test_cfg(),
        ContextService::new(client.clone(), "fake-ctx".to_string()),
        ExecutorService::new(client, "fake-exec".to_string(), None),
        root.clone(),
    );
    let suite = EvalSuite {
        name: "gate".to_string(),
        tasks: vec![EvalTask {
            name: "must-not-pass".to_string(),
            goal: "change a file".to_string(),
            expect_pass: false,
            checks: Vec::new(),
            expect_writes: true,
            max_tokens: None,
        }],
    };
    let rep = runner.run_suite(&suite).await;
    // expect_pass: false + the gate working => not passed => matched.
    assert!(
        !rep.tasks[0].passed,
        "empty work must not pass an expect_writes task"
    );
    assert!(rep.tasks[0].matched, "the task correctly did not pass");
    assert!(
        rep.tasks[0].feedback.contains("harness"),
        "the override explains itself: {}",
        rep.tasks[0].feedback
    );
    let rejected = sink.events().iter().any(|e| {
        matches!(
            e,
            rof::obs::TraceEvent::StateTransition { to, .. } if to == "rejected_no_writes"
        )
    });
    assert!(rejected, "the override is visible in the trace");
    std::fs::remove_dir_all(&root).ok();
}

/// §4.2: the write gate reads the tree through git, not the artifact's
/// self-report. A "write" that leaves the file byte-identical changed nothing,
/// so a pass-happy reviewer on an expect_writes task must still be rejected —
/// with the old self-reported count that no-op read as one write.
#[tokio::test]
async fn write_gate_counts_the_tree_not_the_claim() {
    struct NoOp;
    #[async_trait]
    impl LlmClient for NoOp {
        async fn complete(&self, _model: &str, req: LlmReq) -> Result<LlmResp, LlmError> {
            let text = if req.system.contains("reviewer") {
                "{\"pass\": true, \"feedback\": \"looks done\"}"
            } else if req.system.contains("implementer") {
                // The file already holds this content: the write lands and
                // changes nothing, though the artifact reports it as a write.
                "{\"artifact\":\"x\",\"notes\":\"y\",\"writes\":[{\"path\":\"notes.md\",\"content\":\"seed\"}]}"
            } else {
                "{\"tasks\": [\"t\"], \"acceptance\": [\"a\"]}"
            };
            Ok(LlmResp {
                text: text.to_string(),
                input_tokens: 4,
                output_tokens: 2,
                latency_ms: 1,
                cost_usd: None,
                cached_input_tokens: 0,
                attempts: 1,
            })
        }
    }

    let root = std::env::temp_dir().join(format!("rof-eval-noop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("notes.md"), "seed").unwrap();

    let client = Arc::new(NoOp);
    let sink = Arc::new(TraceSink::new());
    let runner = EvaluationRunner::new(
        sink.clone(),
        test_cfg(),
        ContextService::new(client.clone(), "fake-ctx".to_string()),
        ExecutorService::new(client, "fake-exec".to_string(), None),
        root.clone(),
    );
    let suite = EvalSuite {
        name: "git-gate".to_string(),
        tasks: vec![EvalTask {
            name: "noop".to_string(),
            goal: "change a file".to_string(),
            expect_pass: true,
            checks: Vec::new(),
            expect_writes: true,
            max_tokens: None,
        }],
    };
    let rep = runner.run_suite(&suite).await;

    // Expected to pass; git counted no change, so the pass was rejected.
    assert!(!rep.tasks[0].passed, "a no-op write must not pass");
    assert!(!rep.tasks[0].matched, "expected pass, got the gate");
    assert!(
        rep.tasks[0].feedback.contains("no writes were applied"),
        "the gate names the real change set: {}",
        rep.tasks[0].feedback
    );
    let rejected = sink.events().iter().any(|e| {
        matches!(
            e,
            rof::obs::TraceEvent::StateTransition { to, .. } if to == "rejected_no_writes"
        )
    });
    assert!(rejected, "git's empty diff is what tripped the gate");
    cleanup_task_dirs(&["noop"]);
    std::fs::remove_dir_all(&root).ok();
}

/// §4.2: every task copy is a git repo, even when the source tree was not one —
/// a non-repo source gets `git init` plus one commit, so rollback and the write
/// gate never silently degrade on the degenerate input.
#[tokio::test]
async fn a_non_repo_source_still_gets_the_substrate() {
    let root = std::env::temp_dir().join(format!("rof-eval-norepo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("notes.md"), "seed").unwrap();
    // The source is deliberately not a repo: no `.git` anywhere.
    assert!(!root.join(".git").exists());

    let client = Arc::new(Fake);
    let runner = EvaluationRunner::new(
        Arc::new(TraceSink::new()),
        // The assertions below read the copy after the run, so it must survive.
        AppConfig {
            clean_task_dirs: false,
            ..test_cfg()
        },
        ContextService::new(client.clone(), "fake-ctx".to_string()),
        ExecutorService::new(client, "fake-exec".to_string(), None),
        root.clone(),
    );
    let suite = EvalSuite {
        name: "substrate".to_string(),
        tasks: vec![EvalTask {
            name: "substrate".to_string(),
            goal: "eval target".to_string(),
            expect_pass: true,
            checks: Vec::new(),
            expect_writes: true,
            max_tokens: None,
        }],
    };
    let rep = runner.run_suite(&suite).await;
    assert!(rep.tasks[0].matched, "the task should pass: {rep:?}");

    // The copy the run made carries the substrate the gate read.
    let mut found = 0;
    let rd = std::fs::read_dir(std::env::temp_dir()).unwrap();
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let name = p.file_name().map(|n| n.to_string_lossy().to_string());
        let is_copy = name
            .map(|n| n.starts_with(&format!("rof-task-{}-substrate-", std::process::id())))
            .unwrap_or(false);
        if !is_copy {
            continue;
        }
        found += 1;
        assert!(
            p.join(".git").exists(),
            "a non-repo source must still become a repo: {}",
            p.display()
        );
        // The change the task made is visible to git in the copy.
        let out = std::process::Command::new("git")
            .args(["-C", p.to_str().unwrap(), "status", "--porcelain"])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("notes.md"),
            "the gate's change set must be git-visible in the copy"
        );
    }
    assert_eq!(found, 1, "expected exactly one task copy, found {found}");
    cleanup_task_dirs(&["substrate"]);
    std::fs::remove_dir_all(&root).ok();
}

/// The task root is configurable: a suite whose checks build needs copies on
/// disk, not in a small tmpfs.
#[tokio::test]
async fn task_root_is_honoured() {
    struct Writer;
    #[async_trait]
    impl LlmClient for Writer {
        async fn complete(&self, _model: &str, req: LlmReq) -> Result<LlmResp, LlmError> {
            let text = if req.system.contains("reviewer") {
                "{\"pass\": true, \"feedback\": \"ok\"}".to_string()
            } else if req.system.contains("implementer") {
                "{\"artifact\":\"x\",\"notes\":\"y\",\"writes\":[{\"path\":\"out.md\",\"content\":\"v\"}]}"
                    .to_string()
            } else {
                "{\"tasks\": [\"t\"], \"acceptance\": [\"a\"]}".to_string()
            };
            Ok(LlmResp {
                text,
                input_tokens: 4,
                output_tokens: 2,
                latency_ms: 1,
                cost_usd: None,
                cached_input_tokens: 0,
                attempts: 1,
            })
        }
    }

    let root = std::env::temp_dir().join(format!("rof-eval-root-{}", std::process::id()));
    let copies_root = std::env::temp_dir().join(format!("rof-eval-copies-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&copies_root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("in.md"), "seed").unwrap();

    let client = Arc::new(Writer);
    let cfg = AppConfig {
        task_root: Some(copies_root.clone()),
        clean_task_dirs: false, // the assertion below reads the copy
        ..test_cfg()
    };
    let suite = EvalSuite {
        name: "root".to_string(),
        tasks: vec![EvalTask {
            name: "one".to_string(),
            goal: "write out.md".to_string(),
            expect_pass: true,
            checks: Vec::new(),
            expect_writes: true,
            max_tokens: None,
        }],
    };
    let runner = EvaluationRunner::new(
        Arc::new(TraceSink::new()),
        cfg,
        ContextService::new(client.clone(), "fake-ctx".to_string()),
        ExecutorService::new(client, "fake-exec".to_string(), None),
        root.clone(),
    );
    let rep = runner.run_suite(&suite).await;
    assert!(rep.tasks[0].matched, "{:?}", rep.tasks);
    let copies: Vec<_> = std::fs::read_dir(&copies_root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    assert_eq!(copies.len(), 1, "copy lands under task_root: {copies:?}");
    assert_eq!(
        std::fs::read_to_string(copies[0].join("out.md")).unwrap(),
        "v",
        "the write is visible in the kept copy"
    );
    self_clean(&copies_root);
    std::fs::remove_dir_all(&root).ok();
}

fn self_clean(dir: &std::path::Path) {
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn suite_loads_from_json() {
    // Manifest-relative: the test must not depend on the process cwd.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/suites/sample.json");
    let s = EvalSuite::load(&path).unwrap();
    assert_eq!(s.name, "sample");
    assert_eq!(s.tasks.len(), 2);
}

// ---------------------------------------------------------------- stage 0 ---
// Report labels, per-task context metrics and `rof compare`. All of it is
// additive: these tests also pin that an old report still loads.

/// A suite of `(name, goal)` tasks, each expecting a write and a pass.
fn suite_of(name: &str, goals: &[(&str, &str)]) -> EvalSuite {
    EvalSuite {
        name: name.to_string(),
        tasks: goals
            .iter()
            .map(|(n, g)| EvalTask {
                name: n.to_string(),
                goal: g.to_string(),
                expect_pass: true,
                checks: Vec::new(),
                expect_writes: true,
                max_tokens: None,
            })
            .collect(),
    }
}

/// A report that ran against a scratch tree: the `Fake` client writes
/// `notes.md`, so a goal naming "eval target" retrieves exactly that file.
async fn run_fake_suite(tag: &str) -> SuiteReport {
    let root = std::env::temp_dir().join(format!("rof-eval-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("notes.md"), "eval target content").unwrap();
    let client = Arc::new(Fake);
    let runner = EvaluationRunner::new(
        Arc::new(TraceSink::new()),
        test_cfg(),
        ContextService::new(client.clone(), "fake-ctx".to_string()),
        ExecutorService::new(client, "fake-exec".to_string(), None),
        root.clone(),
    );
    let rep = runner
        .run_suite(&suite_of(tag, &[("work", "eval target")]))
        .await;
    cleanup_task_dirs(&["work"]);
    std::fs::remove_dir_all(&root).ok();
    rep
}

/// A report must say which harness, config, suite and model pair produced it —
/// that is what makes two runs comparable instead of anecdotal.
#[tokio::test]
async fn report_carries_a_label_and_its_inputs() {
    let rep = run_fake_suite("label").await;
    assert_eq!(rep.label.ctx_model, "fake-ctx");
    assert_eq!(rep.label.exec_model, "fake-exec");
    assert_eq!(rep.label.harness_version, env!("CARGO_PKG_VERSION"));
    assert!(!rep.label.config_hash.is_empty(), "{:?}", rep.label);
    assert!(!rep.label.suite_hash.is_empty(), "{:?}", rep.label);
    assert_eq!(
        rep.label.git_head, "unknown",
        "a scratch tree is not a git repo: {:?}",
        rep.label
    );
    // The label survives a round trip through the report file, and the hashes
    // are recomputable by hand (config = the `rof config` dump, verbatim).
    let cfg = test_cfg();
    let suite = suite_of("label", &[("work", "eval target")]);
    assert_eq!(
        rep.label.config_hash,
        fnv1a_hex(cfg.to_json().as_bytes()),
        "config_hash is the canonical config dump, hashed"
    );
    assert_eq!(
        rep.label.suite_hash,
        fnv1a_hex(serde_json::to_string(&suite).unwrap().as_bytes())
    );
    let json = serde_json::to_string(&rep).unwrap();
    let back: SuiteReport = serde_json::from_str(&json).unwrap();
    assert_eq!(back.label, rep.label);
    assert_eq!(back.tasks[0].context, rep.tasks[0].context);

    // Same inputs, same hashes; a changed config or task set is a different
    // report. `--limit` truncates the suite before it runs, so the label
    // describes the tasks that actually ran.
    let root = std::env::temp_dir();
    let rebuilt = RunLabel::build(&cfg, &suite, &root);
    assert_eq!(rebuilt.git_head, rep.label.git_head);
    assert_eq!(rebuilt.config_hash, rep.label.config_hash);
    assert_eq!(rebuilt.suite_hash, rep.label.suite_hash);
    assert_eq!(rebuilt.harness_version, rep.label.harness_version);
    // The model ids name the services the run actually used (`rof eval` wires
    // both from the same config; this runner was handed fake ones).
    assert_eq!(rebuilt.ctx_model, "cheap-model");
    assert_eq!(rep.label.ctx_model, "fake-ctx");
    let other_cfg = AppConfig {
        max_review_rounds: 9,
        ..cfg.clone()
    };
    assert_ne!(
        RunLabel::build(&other_cfg, &suite, &root).config_hash,
        rep.label.config_hash
    );
    let mut two = suite.clone();
    two.tasks.push(EvalTask {
        name: "second".to_string(),
        goal: "eval target".to_string(),
        expect_pass: true,
        checks: Vec::new(),
        expect_writes: true,
        max_tokens: None,
    });
    let mut shorter = two.clone();
    shorter.tasks.truncate(1);
    assert_ne!(
        RunLabel::build(&cfg, &shorter, &root).suite_hash,
        RunLabel::build(&cfg, &two, &root).suite_hash,
        "a --limit run is a different task set, so a different label"
    );
    // The fallback: a label must never fail a run.
    assert_eq!(
        git_head(std::path::Path::new("/no/such/dir/at/all")),
        "unknown"
    );
}

/// Context accounting is folded from data the run already carries: what
/// retrieval handed over, what the task touched, what summarizing cost.
#[tokio::test]
async fn context_metrics_fold_retrieval_against_what_the_task_touched() {
    let rep = run_fake_suite("ctx").await;
    let c = &rep.tasks[0].context;
    assert_eq!(c.retrieved_files, 1, "goal keywords match notes.md: {c:?}");
    assert!(c.retrieved_chars > 0, "{c:?}");
    assert_eq!(
        c.referenced_chars, c.retrieved_chars,
        "the task wrote the one retrieved file: {c:?}"
    );
    assert!((c.relevance_proxy - 1.0).abs() < 1e-6, "{c:?}");
}

#[test]
fn context_metrics_count_only_what_matched_and_never_panic_on_an_old_run() {
    let out = serde_json::json!({
        "retrieved": [{"path": "src/a.rs", "chars": 100}, {"path": "src/b.rs", "chars": 300}],
        "tasks": [{"artifact": {"file_state": [
            {"path": "src/b.rs", "why": "applied", "current_content": "x"},
            {"path": "src/c.rs", "why": "applied", "current_content": "y"},
        ]}}],
        "summarize_calls": 2,
        "summarize_tokens": 700,
        "truncated_views": 3,
    });
    let m = ContextMetrics::from_run(&out);
    assert_eq!(m.retrieved_files, 2);
    assert_eq!(m.retrieved_chars, 400);
    assert_eq!(m.referenced_chars, 300, "src/c.rs was not retrieved at all");
    assert!((m.relevance_proxy - 0.75).abs() < 1e-6, "{m:?}");
    assert_eq!(m.summarize_calls, 2);
    assert_eq!(m.summarize_tokens, 700);
    assert_eq!(m.truncated_views, 3);
    // A run JSON from before these keys existed folds to zeroes, no panic.
    assert_eq!(
        ContextMetrics::from_run(&serde_json::json!({})),
        ContextMetrics::default()
    );
}

fn labelled_task(name: &str, matched: bool, rounds: u32, feedback: &str) -> TaskResult {
    TaskResult {
        name: name.to_string(),
        passed: matched,
        expected: true,
        matched,
        rounds,
        feedback: feedback.to_string(),
        context: ContextMetrics {
            retrieved_files: 1,
            retrieved_chars: 400,
            referenced_chars: 200,
            relevance_proxy: 0.5,
            ..ContextMetrics::default()
        },
        checks: Vec::new(),
    }
}

fn labelled_report(suite_hash: &str, tasks: Vec<TaskResult>) -> SuiteReport {
    SuiteReport {
        tasks,
        label: RunLabel {
            git_head: "aaaaaaa".to_string(),
            config_hash: "c0ffee".to_string(),
            suite_hash: suite_hash.to_string(),
            ctx_model: "ctx".to_string(),
            exec_model: "exec".to_string(),
            harness_version: "0.1.0".to_string(),
        },
        ..SuiteReport::default()
    }
}

/// `rof compare`: the task that moved, the metric that moved, and the warning
/// that says whether the two reports are comparable at all.
#[test]
fn compare_names_the_tasks_and_metrics_that_moved() {
    let mut a = labelled_report(
        "suite-a",
        vec![
            labelled_task("winner", true, 1, ""),
            labelled_task(
                "breaker",
                false,
                2,
                "patch refused: search string not found",
            ),
            labelled_task("gone", true, 1, ""),
        ],
    );
    let mut b = labelled_report(
        "suite-a",
        vec![
            labelled_task("winner", true, 1, ""),
            labelled_task("breaker", true, 1, ""),
            labelled_task("new", false, 1, "no writes applied"),
        ],
    );
    a.aggregate.est_cost_usd = 0.010;
    a.aggregate.est_input_tokens = 1_000;
    a.aggregate.tool_calls = 10;
    a.aggregate.tool_ok = 10;
    b.aggregate.est_cost_usd = 0.020;
    b.aggregate.est_input_tokens = 800;
    b.aggregate.tool_calls = 10;
    b.aggregate.tool_ok = 9;

    let c = compare(&a, &b);
    assert_eq!((c.matched_a, c.matched_b), (2, 2));
    assert_eq!(c.gained(), 1, "breaker went fail -> pass");
    assert_eq!(c.lost(), 0);
    let change = |name: &str| {
        c.tasks
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.change())
            .unwrap()
    };
    assert_eq!(change("breaker"), TaskChange::Gained);
    assert_eq!(change("gone"), TaskChange::OnlyInA);
    assert_eq!(change("new"), TaskChange::OnlyInB);
    assert_eq!(change("winner"), TaskChange::Same);
    assert_eq!(
        c.changed().len(),
        3,
        "one gain, one only-in-a, one only-in-b"
    );

    // The note explains the move from the side that failed. A task that exists
    // on one side only did not "move": it has no note.
    let breaker = c.tasks.iter().find(|t| t.name == "breaker").unwrap();
    assert!(breaker.note.contains("patch refused"), "{}", breaker.note);
    let only_b = c.tasks.iter().find(|t| t.name == "new").unwrap();
    assert!(only_b.note.is_empty(), "{}", only_b.note);
    let gone = c.tasks.iter().find(|t| t.name == "gone").unwrap();
    assert!(gone.note.is_empty(), "{}", gone.note);

    let cost = c.metric("est_cost_usd").unwrap();
    assert!((cost.delta() - 0.010).abs() < 1e-9, "{cost:?}");
    assert!(cost.changed());
    assert!(
        (c.metric("tool_accuracy").unwrap().delta() + 0.1).abs() < 1e-9,
        "{:?}",
        c.metric("tool_accuracy")
    );
    assert_eq!(c.metric("est_input_tokens").unwrap().delta(), -200.0);
    assert_eq!(c.metric("relevance_proxy").unwrap().a, 0.5);
    assert_eq!(c.metric("matched").unwrap().delta(), 0.0);

    // Same suite hash, same config => no comparability warning.
    assert!(c.notes.is_empty(), "{:?}", c.notes);
    // A different task set is not an A/B, and says so.
    let d = compare(&a, &labelled_report("suite-b", b.tasks.clone()));
    assert!(
        d.notes.iter().any(|n| n.contains("suite_hash differs")),
        "{:?}",
        d.notes
    );

    // Self-comparison is all zeroes.
    let z = compare(&a, &a);
    assert!(z.changed().is_empty());
    assert!(z.metrics.iter().all(|m| !m.changed()), "{:?}", z.metrics);
    assert_eq!(z.metric("est_cost_usd").unwrap().delta(), 0.0);

    // The rendering carries the rows a human reads.
    let text = c.render();
    for needle in ["breaker", "est_cost_usd", "relevance_proxy", "+0.01000"] {
        assert!(text.contains(needle), "missing {needle} in:\n{text}");
    }
}

/// §4.3: a task that moved because a check flipped must say which one, and a
/// task that moved with no check flipped must say that too — the second is the
/// model's verdict moving, not the gate's, and the two have different fixes.
#[test]
fn compare_names_the_check_that_flipped_for_a_moved_task() {
    let mut breaker = labelled_task("breaker", false, 2, "the configured check failed");
    breaker.checks = vec![
        CheckResult {
            name: "cargo test".to_string(),
            passed: false,
            output: String::new(),
        },
        CheckResult {
            name: "cargo fmt --check".to_string(),
            passed: true,
            output: String::new(),
        },
    ];
    let mut fixed = labelled_task("breaker", true, 1, "");
    fixed.checks = vec![
        CheckResult {
            name: "cargo test".to_string(),
            passed: true,
            output: String::new(),
        },
        CheckResult {
            name: "cargo fmt --check".to_string(),
            passed: true,
            output: String::new(),
        },
    ];
    // A task that moved with NO check flipped: the verdict changed, the gate
    // did not. This is the case where an empty `flipped_checks` carries
    // information.
    let mut mood_a = labelled_task("mood", false, 3, "reviewer: not convinced");
    mood_a.checks = vec![CheckResult {
        name: "cargo test".to_string(),
        passed: true,
        output: String::new(),
    }];
    let mut mood_b = labelled_task("mood", true, 3, "reviewer: convinced");
    mood_b.checks = mood_a.checks.clone();

    let a = labelled_report("suite-a", vec![breaker, mood_a]);
    let b = labelled_report("suite-a", vec![fixed, mood_b]);
    let c = compare(&a, &b);

    let breaker = c.tasks.iter().find(|t| t.name == "breaker").unwrap();
    assert_eq!(breaker.change(), TaskChange::Gained);
    assert_eq!(breaker.flipped_checks.len(), 1, "only cargo test flipped");
    assert_eq!(breaker.flipped_checks[0].name, "cargo test");
    assert!(breaker.flipped_checks[0].render().contains("fail -> pass"));

    let mood = c.tasks.iter().find(|t| t.name == "mood").unwrap();
    assert_eq!(mood.change(), TaskChange::Gained);
    assert!(
        mood.flipped_checks.is_empty(),
        "the gate held; an empty list is the signal, not a gap"
    );

    // The rendered table names the check, so a human reads the cause.
    let text = c.render();
    assert!(
        text.contains("check 'cargo test' flipped: fail -> pass"),
        "missing the flip line in:\n{text}"
    );
}

/// The committed baseline predates stage 0: it must still load, compare, and
/// say honestly that it carries no label and no context metrics.
#[test]
fn a_pre_stage_0_report_still_loads_and_compares_with_itself() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs/baselines/2026-09-14-deepseek-chat-6.json");
    let rep = SuiteReport::load(&path).unwrap();
    assert_eq!(rep.tasks.len(), 6);
    assert!(rep.label.is_unlabeled(), "{:?}", rep.label);
    assert_eq!(
        rep.matched(),
        3,
        "the committed baseline is 3/6 (docs/STATUS.md)"
    );
    assert!(
        rep.tasks
            .iter()
            .all(|t| t.context == ContextMetrics::default()),
        "no context metrics in a pre-stage-0 report"
    );
    let c = compare(&rep, &rep);
    assert!(c.changed().is_empty());
    assert!(c.metrics.iter().all(|m| !m.changed()), "{:?}", c.metrics);
    assert!(
        c.notes.iter().any(|n| n.contains("unlabeled")),
        "{:?}",
        c.notes
    );
    assert!(
        c.notes
            .iter()
            .any(|n| n.contains("no retrieved context recorded")),
        "{:?}",
        c.notes
    );
}
