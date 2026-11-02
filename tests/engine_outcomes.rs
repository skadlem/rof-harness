use rof::{
    config::AppConfig,
    eval::{EvalTask, EvaluationRunner, ExecutionStatus},
    llm::{ContextService, ExecutorService, StubClient},
    obs::TraceSink,
};
use std::sync::Arc;

#[tokio::test]
async fn engine_initialization_error_is_inconclusive_not_expected_failure() {
    let root = std::env::temp_dir().join(format!("rof-outcomes-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let client = Arc::new(StubClient);
    let runner = EvaluationRunner::new(
        Arc::new(TraceSink::new()),
        AppConfig::default(),
        ContextService::new(client.clone(), "stub".into()),
        ExecutorService::new(client.clone(), "stub".into(), None),
        ExecutorService::new(client, "stub".into(), None),
        root.clone(),
    );
    let task = EvalTask {
        name: "negative".into(),
        goal: "impossible".into(),
        expect_pass: false,
        checks: vec![],
        fail_to_pass: vec![],
        pass_to_pass: vec![],
        expect_writes: true,
        max_tokens: None,
    };
    let result = runner.run_task_in(&task, root.join("missing")).await;
    assert_eq!(result.status, ExecutionStatus::Inconclusive);
    assert!(!result.matched);
    assert!(!result.feedback.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

struct SlowClient;
#[async_trait::async_trait]
impl rof::llm::LlmClient for SlowClient {
    async fn complete(
        &self,
        _: &str,
        _: rof::llm::LlmReq,
    ) -> Result<rof::llm::LlmResp, rof::llm::LlmError> {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        Err(rof::llm::LlmError::Transport("slow provider".into()))
    }
}

#[tokio::test]
async fn task_deadline_bounds_a_stalled_goal_and_preserves_trace() {
    let root = std::env::temp_dir().join(format!("rof-task-deadline-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let client = Arc::new(SlowClient);
    let mut cfg = AppConfig::default();
    cfg.limits.task_timeout_ms = 100;
    let trace = Arc::new(TraceSink::new());
    let registry = rof::tools::ToolRegistry::with_defaults(
        root.clone(),
        cfg.permissions.clone(),
        cfg.skills.clone(),
    );
    let orch = rof::engine::Orchestrator::new(
        cfg,
        trace.clone(),
        ContextService::new(client.clone(), "slow".into()),
        ExecutorService::new(client.clone(), "slow".into(), None),
        ExecutorService::new(client, "slow".into(), None),
    );
    let started = std::time::Instant::now();
    let outcome = orch
        .run_outcome(
            &rof::engine::Session::new("explain".into()).expecting_writes(false),
            &registry,
            &root,
        )
        .await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "task deadline failed to interrupt provider"
    );
    assert!(outcome
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("deadline"));
    assert!(!outcome.passed);
    assert!(!trace.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn evaluation_baseline_checks_share_the_task_deadline() {
    let root = std::env::temp_dir().join(format!("rof-baseline-deadline-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("slow.py"), "import time\ntime.sleep(0.5)\n").unwrap();
    let cmd = "python3 slow.py".to_string();
    let mut cfg = AppConfig::default();
    cfg.limits.task_timeout_ms = 100;
    cfg.permissions.allowed_commands = vec![cmd.clone()];
    let client = Arc::new(StubClient);
    let runner = EvaluationRunner::new(
        Arc::new(TraceSink::new()),
        cfg,
        ContextService::new(client.clone(), "stub".into()),
        ExecutorService::new(client.clone(), "stub".into(), None),
        ExecutorService::new(client, "stub".into(), None),
        root.clone(),
    );
    let task = EvalTask {
        name: "baseline".into(),
        goal: "fix".into(),
        expect_pass: false,
        checks: vec![cmd.clone()],
        fail_to_pass: vec![cmd],
        pass_to_pass: vec![],
        expect_writes: true,
        max_tokens: None,
    };
    let started = std::time::Instant::now();
    let result = runner.run_task_in(&task, root.clone()).await;
    assert!(
        started.elapsed() < std::time::Duration::from_millis(450),
        "baseline escaped task deadline"
    );
    assert_eq!(result.status, ExecutionStatus::Inconclusive);
    assert!(!result.matched);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollback_of_an_initially_empty_tree_is_valid() {
    let root = std::env::temp_dir().join(format!("rof-empty-rollback-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let tree = rof::engine::TreeService::new(root.clone());
    tree.ensure().unwrap();
    tree.baseline().unwrap();
    std::fs::write(root.join("new.txt"), "attempt").unwrap();
    let rollback = tree.rollback();
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        rollback.is_ok(),
        "empty baseline should be restorable: {rollback:?}"
    );
}

struct OfflineClient;
#[async_trait::async_trait]
impl rof::llm::LlmClient for OfflineClient {
    async fn complete(
        &self,
        _: &str,
        _: rof::llm::LlmReq,
    ) -> Result<rof::llm::LlmResp, rof::llm::LlmError> {
        Err(rof::llm::LlmError::Transport("connection refused".into()))
    }
}

#[tokio::test]
async fn provider_failure_cannot_satisfy_an_expected_agent_failure() {
    for mode in ["pipeline", "direct"] {
        let root = std::env::temp_dir().join(format!("rof-offline-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let client = Arc::new(OfflineClient);
        let cfg = AppConfig {
            execution: mode.into(),
            attempts: 3,
            ..Default::default()
        };
        let trace = Arc::new(TraceSink::new());
        let runner = EvaluationRunner::new(
            trace.clone(),
            cfg,
            ContextService::new(client.clone(), "offline".into()),
            ExecutorService::new(client.clone(), "offline".into(), None),
            ExecutorService::new(client, "offline".into(), None),
            root.clone(),
        );
        let task = EvalTask {
            name: "offline".into(),
            goal: "explain".into(),
            expect_pass: false,
            checks: vec![],
            fail_to_pass: vec![],
            pass_to_pass: vec![],
            expect_writes: false,
            max_tokens: None,
        };
        let result = runner.run_task_in(&task, root.clone()).await;
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(
            result.status,
            ExecutionStatus::Inconclusive,
            "{mode}: {}",
            result.feedback
        );
        assert!(!result.matched);
        assert_eq!(trace.events().iter().filter(|event| matches!(event,rof::obs::TraceEvent::StateTransition{to,..} if to.starts_with("attempt_"))).count(),1,"infrastructure failure retried independent attempts");
    }
}
