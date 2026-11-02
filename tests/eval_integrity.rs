use rof::config::AppConfig;
use rof::eval::{compare, EvalSuite, EvalTask, EvaluationRunner, ExecutionStatus, SuiteReport};
use rof::llm::{ContextService, ExecutorService, StubClient};
use rof::obs::TraceSink;
use std::sync::Arc;

async fn copy_failure_report(reps: usize) -> SuiteReport {
    let root = std::env::temp_dir().join(format!("rof-eval-integrity-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let client = Arc::new(StubClient);
    let runner = EvaluationRunner::new(
        Arc::new(TraceSink::new()),
        AppConfig {
            task_root: Some(root.join("copies")),
            ..AppConfig::default()
        },
        ContextService::new(client.clone(), "stub".into()),
        ExecutorService::new(client.clone(), "stub".into(), None),
        ExecutorService::new(client, "stub".into(), None),
        root.join("missing-source"),
    );
    let suite = EvalSuite {
        name: "infrastructure".into(),
        tasks: vec![EvalTask {
            name: "expected-failure".into(),
            goal: "an impossible task".into(),
            expect_pass: false,
            checks: Vec::new(),
            fail_to_pass: Vec::new(),
            pass_to_pass: Vec::new(),
            expect_writes: true,
            max_tokens: None,
        }],
    };
    let report = runner.run_suite_with_reps(&suite, 1, reps).await;
    std::fs::remove_dir_all(&root).unwrap();
    report
}

/// A missing source tree is an infrastructure error, never evidence that an
/// agent correctly failed an intentionally unsatisfiable task.
#[tokio::test]
async fn task_copy_failure_cannot_satisfy_an_expected_failure() {
    let report = copy_failure_report(1).await;
    assert!(report.tasks[0].feedback.contains("task-dir copy failed"));
    assert!(
        !report.tasks[0].matched,
        "failure to prepare a task must not match an expected agent failure"
    );
    assert_eq!(report.matched(), 0);
    assert_eq!(report.success_rate(), 0.0);
}

#[tokio::test]
async fn inconclusive_repetitions_are_not_agent_failures() {
    let report = copy_failure_report(3).await;
    assert_eq!(report.tasks[0].status, ExecutionStatus::Inconclusive);
    assert_eq!(report.tasks[0].reps, 3);
    assert_eq!(report.tasks[0].inconclusive_reps, 3);
    assert_eq!(report.aggregate.inconclusive_tasks, 3);
    assert_eq!(report.aggregate.tasks, 0);
    assert_eq!(report.aggregate.passed, 0);
    assert_eq!(report.completed(), 0);
}

#[tokio::test]
async fn infrastructure_errors_do_not_become_capability_gains() {
    let before = copy_failure_report(1).await;
    let mut after = before.clone();
    after.tasks[0].status = ExecutionStatus::Completed;
    after.tasks[0].inconclusive_reps = 0;
    after.tasks[0].matched = true;
    let comparison = compare(&before, &after);
    assert_eq!(comparison.gained(), 0);
    assert_eq!(comparison.lost(), 0);
    assert!(comparison.render().contains("inconclusive"));
}

#[tokio::test]
async fn legacy_task_results_default_to_completed_execution() {
    let report = copy_failure_report(1).await;
    let mut old = serde_json::to_value(&report.tasks[0]).unwrap();
    old.as_object_mut().unwrap().remove("status");
    old.as_object_mut().unwrap().remove("inconclusive_reps");
    let restored: rof::eval::TaskResult = serde_json::from_value(old).unwrap();
    assert_eq!(restored.status, ExecutionStatus::Completed);
    assert_eq!(restored.inconclusive_reps, 0);
}
