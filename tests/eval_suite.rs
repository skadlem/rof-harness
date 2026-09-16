use async_trait::async_trait;
use rof::config::AppConfig;
use rof::eval::{EvalSuite, EvalTask, EvaluationRunner};
use rof::llm::{ContextService, ExecutorService, LlmClient, LlmError, LlmReq, LlmResp};
use rof::obs::TraceSink;
use std::sync::Arc;

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
        AppConfig::default(),
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
        ..Default::default()
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
        AppConfig::default(),
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

/// The task root is configurable (a suite whose checks build needs copies on
/// disk, not in a small tmpfs), and cleanup is opt-in.
#[tokio::test]
async fn task_root_is_honoured_and_cleanup_removes_copies() {
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
        clean_task_dirs: false,
        ..Default::default()
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
        ExecutorService::new(client.clone(), "fake-exec".to_string(), None),
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

    // Same run with cleanup on: the copy must be gone afterwards.
    let root2 = std::env::temp_dir().join(format!("rof-eval-root2-{}", std::process::id()));
    let copies2 = std::env::temp_dir().join(format!("rof-eval-copies2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root2);
    let _ = std::fs::remove_dir_all(&copies2);
    std::fs::create_dir_all(&root2).unwrap();
    std::fs::write(root2.join("in.md"), "seed").unwrap();
    let runner = EvaluationRunner::new(
        Arc::new(TraceSink::new()),
        AppConfig {
            task_root: Some(copies2.clone()),
            clean_task_dirs: true,
            ..Default::default()
        },
        ContextService::new(client.clone(), "fake-ctx".to_string()),
        ExecutorService::new(client, "fake-exec".to_string(), None),
        root2.clone(),
    );
    let rep = runner.run_suite(&suite).await;
    assert!(rep.tasks[0].matched, "{:?}", rep.tasks);
    assert_eq!(
        std::fs::read_dir(&copies2).unwrap().count(),
        0,
        "clean_task_dirs removes the copy"
    );
    self_clean(&copies_root);
    self_clean(&copies2);
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&root2).ok();
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
