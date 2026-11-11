use super::*;

struct CompletionTest {
    commands: Arc<Mutex<Vec<String>>>,
    outcomes: Arc<Mutex<VecDeque<bool>>>,
    delay: Duration,
    root: Option<std::path::PathBuf>,
}

#[async_trait::async_trait]
impl CoreTool for CompletionTest {
    fn definition(&self) -> CoreToolDef {
        CoreToolDef {
            name: "test".into(),
            description: "test command".into(),
            schema: serde_json::json!({
                "type": "object",
                "required": ["cmd"],
                "additionalProperties": false,
                "properties": {"cmd": {"type": "string"}}
            }),
        }
    }

    fn prepare(&self, call: &CoreToolCall) -> CoreCallStatus {
        CoreCallStatus::Dispatch(CoreInvocation {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            args: call.args.clone(),
        })
    }

    async fn execute(
        &self,
        inv: CoreInvocation,
        _: CancellationToken,
    ) -> Result<CoreToolOutcome, CoreToolError> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let cmd = inv.args["cmd"].as_str().unwrap().to_owned();
        self.commands.lock().unwrap().push(cmd.clone());
        let success = self.outcomes.lock().unwrap().pop_front().unwrap_or(true);
        if !success {
            if let Some(root) = &self.root {
                std::fs::write(root.join("predicate-marker"), "changed").unwrap();
            }
        }
        Ok(CoreToolOutcome {
            content: format!("{}: {cmd}", if success { "PASS" } else { "FAIL" }),
            truncated: false,
            success,
        })
    }
}

#[tokio::test]
async fn configured_completion_check_runs_through_the_test_tool_for_no_edit_tasks() {
    let root = run_tmp("completion-pass");
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut registry = CoreRegistry::new(Arc::new(GrantGate::new(
        [("agent".to_owned(), vec!["test".to_owned()])].into(),
    )));
    registry.register(Arc::new(CompletionTest {
        commands: commands.clone(),
        outcomes: Arc::new(Mutex::new(VecDeque::from([true]))),
        delay: Duration::ZERO,
        root: None,
    }));
    let client = FakeLlm {
        order: Arc::new(Mutex::new(Vec::new())),
        queue: Mutex::new(VecDeque::from(vec![text_resp("done")])),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let cfg = RunConfig {
        completion_check: Some("cargo test -p fixture".into()),
        ..RunConfig::default()
    };
    let mut state = LoopState::new();
    let mut emitter = Emitter::new();
    let outcome = run(
        &mut state,
        Run {
            provider: &client,
            registry: &registry,
            agent: "agent",
            workdir: &root,
            emitter: &mut emitter,
            bets: &NoBets,
            cfg,
        },
        vec![Input::User("read the files and answer".into())],
        &CancellationToken::new(),
    )
    .await;

    match outcome {
        Outcome::Done => {}
        Outcome::Halted(message) => panic!("unexpected halt: {message}"),
        Outcome::Failed { kind, message } => panic!("unexpected {kind:?}: {message}"),
        Outcome::Cancelled => panic!("unexpected cancellation"),
    }
    assert_eq!(&*commands.lock().unwrap(), &["cargo test -p fixture"]);
    assert!(state.items.iter().any(|item| matches!(
        &item.kind,
        ItemKind::ToolCall { tool, args, .. }
            if tool == "test" && args["cmd"] == "cargo test -p fixture"
    )));
    assert!(state.items.iter().any(|item| matches!(
        &item.kind,
        ItemKind::ToolResult { content, is_error: false, .. }
            if content == "PASS: cargo test -p fixture"
    )));
    assert!(state.items.iter().any(|item| matches!(
        &item.kind,
        ItemKind::Attempt { error, .. }
            if error == "harness-origin completion-check call_id=harness-completion-1"
    )));
    let _ = std::fs::remove_dir_all(root);
}

struct CompletionCase {
    outcomes: Vec<bool>,
    allowed: bool,
    budget: agent_budget::BudgetConfig,
    delay: Duration,
    preserve_failed_checks: bool,
    completion_check: Option<String>,
}

async fn run_completion_case(
    name: &str,
    replies: Vec<Scripted>,
    case: CompletionCase,
    cancel: CancellationToken,
) -> (
    Outcome,
    LoopState,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Vec<Request>>>,
    std::path::PathBuf,
    bool,
) {
    let root = run_tmp(name);
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut registry = CoreRegistry::new(Arc::new(GrantGate::new(
        [(
            "agent".to_owned(),
            if case.allowed {
                vec!["test".to_owned(), "write".into()]
            } else {
                vec![]
            },
        )]
        .into(),
    )));
    registry.register(Arc::new(CompletionTest {
        commands: commands.clone(),
        outcomes: Arc::new(Mutex::new(VecDeque::from(case.outcomes))),
        delay: case.delay,
        root: Some(root.clone()),
    }));
    registry.register(Arc::new(WriteFile { root: root.clone() }));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let client = FakeLlm {
        order: Arc::new(Mutex::new(Vec::new())),
        queue: Mutex::new(VecDeque::from(replies)),
        requests: requests.clone(),
    };
    let mut state = LoopState::new();
    state.budget = BudgetGuard::new(case.budget, Instant::now());
    let mut emitter = Emitter::new();
    let cfg = RunConfig {
        completion_check: case.completion_check,
        preserve_failed_checks: case.preserve_failed_checks,
        ..RunConfig::default()
    };
    let outcome = run(
        &mut state,
        Run {
            provider: &client,
            registry: &registry,
            agent: "agent",
            workdir: &root,
            emitter: &mut emitter,
            bets: &NoBets,
            cfg,
        },
        vec![Input::User("finish the task".into())],
        &cancel,
    )
    .await;
    let mutation_survived = root.join("predicate-marker").exists();
    (outcome, state, commands, requests, root, mutation_survived)
}

fn default_budget() -> agent_budget::BudgetConfig {
    agent_budget::config_for(agent_budget::Capability::UnattendedBatch)
}

#[tokio::test]
async fn failed_predicate_allows_one_tool_repair_continuation_and_a_second_check() {
    let (outcome, state, commands, requests, root, _) = run_completion_case(
        "completion-repair",
        vec![
            text_resp("done"),
            script_resp(
                vec![(
                    "repair",
                    "write",
                    serde_json::json!({"path":"answer.txt","content":"fixed"}),
                )],
                StopReason::ToolUse,
            ),
            text_resp("done again"),
        ],
        CompletionCase {
            outcomes: vec![false, true],
            allowed: true,
            budget: default_budget(),
            delay: Duration::ZERO,
            preserve_failed_checks: false,
            completion_check: Some("cargo test -p fixture".into()),
        },
        CancellationToken::new(),
    )
    .await;
    match outcome {
        Outcome::Done => {}
        Outcome::Halted(message) => panic!("unexpected halt: {message}"),
        Outcome::Failed { kind, message } => panic!("unexpected {kind:?}: {message}"),
        Outcome::Cancelled => panic!("unexpected cancellation"),
    }
    assert_eq!(commands.lock().unwrap().len(), 2);
    assert_eq!(requests.lock().unwrap().len(), 3);
    let captured = requests.lock().unwrap();
    let repair = &captured[1];
    for (i, message) in repair.messages.iter().enumerate() {
        if let Some(id) = &message.tool_call_id {
            assert!(
                repair.messages[..i]
                    .iter()
                    .any(|prior| prior.role == "assistant"
                        && prior.tool_calls.iter().any(|call| &call.id == id)),
                "unpaired provider tool result: {id}"
            );
        }
    }
    drop(captured);
    assert!(state.items.iter().any(|item| matches!(
        &item.kind,
        ItemKind::ToolResult { call_id, content, is_error: true, .. }
            if call_id == "harness-completion-1" && content == "FAIL: cargo test -p fixture"
    )));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn two_failed_predicate_checks_are_completion_failure() {
    let (outcome, state, commands, _, root, _) = run_completion_case(
        "completion-two-failures",
        vec![text_resp("done"), text_resp("done again")],
        CompletionCase {
            outcomes: vec![false, false],
            allowed: true,
            budget: default_budget(),
            delay: Duration::ZERO,
            preserve_failed_checks: false,
            completion_check: Some("cargo test -p fixture".into()),
        },
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(
        outcome,
        Outcome::Failed {
            kind: FailureKind::Completion,
            ..
        }
    ));
    assert_eq!(commands.lock().unwrap().len(), 2);
    assert!(state.items.iter().any(|item| matches!(
        &item.kind,
        ItemKind::ToolCall { call_id, .. } if call_id == "harness-completion-2"
    )));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn policy_rejection_is_completion_failure_without_execution() {
    let (outcome, _, commands, _, root, _) = run_completion_case(
        "completion-policy",
        vec![text_resp("done")],
        CompletionCase {
            outcomes: vec![],
            allowed: false,
            budget: default_budget(),
            delay: Duration::ZERO,
            preserve_failed_checks: false,
            completion_check: Some("cargo test -p fixture".into()),
        },
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(
        outcome,
        Outcome::Failed {
            kind: FailureKind::Completion,
            ..
        }
    ));
    assert!(commands.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn unrelated_passing_test_does_not_satisfy_the_configured_command() {
    let (outcome, _, commands, _, root, _) = run_completion_case(
        "completion-exact-command",
        vec![
            script_resp(
                vec![("unrelated", "test", serde_json::json!({"cmd":"make test"}))],
                StopReason::ToolUse,
            ),
            text_resp("done"),
        ],
        CompletionCase {
            outcomes: vec![true, true],
            allowed: true,
            budget: default_budget(),
            delay: Duration::ZERO,
            preserve_failed_checks: false,
            completion_check: Some("cargo test -p fixture".into()),
        },
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, Outcome::Done));
    assert_eq!(
        &*commands.lock().unwrap(),
        &["make test", "cargo test -p fixture"]
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn default_none_does_not_dispatch_a_completion_check() {
    let (outcome, _, commands, _, root, _) = run_completion_case(
        "completion-disabled",
        vec![text_resp("done")],
        CompletionCase {
            outcomes: vec![],
            allowed: true,
            budget: default_budget(),
            delay: Duration::ZERO,
            preserve_failed_checks: false,
            completion_check: None,
        },
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, Outcome::Done));
    assert!(commands.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn passing_predicate_keeps_the_existing_last_step_done_grace() {
    let mut budget = default_budget();
    budget.max_steps = std::num::NonZeroU32::new(1).unwrap();
    let (outcome, _, commands, _, root, _) = run_completion_case(
        "completion-step-grace",
        vec![text_resp("done")],
        CompletionCase {
            outcomes: vec![true],
            allowed: true,
            budget,
            delay: Duration::ZERO,
            preserve_failed_checks: false,
            completion_check: Some("cargo test -p fixture".into()),
        },
        CancellationToken::new(),
    )
    .await;
    match outcome {
        Outcome::Done => {}
        Outcome::Halted(message) => panic!("unexpected halt: {message}"),
        Outcome::Failed { kind, message } => panic!("unexpected {kind:?}: {message}"),
        Outcome::Cancelled => panic!("unexpected cancellation"),
    }
    assert_eq!(commands.lock().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn failed_predicate_cannot_use_last_step_grace_for_a_repair_request() {
    let mut budget = default_budget();
    budget.max_steps = std::num::NonZeroU32::new(1).unwrap();
    let (outcome, _, commands, requests, root, _) = run_completion_case(
        "completion-no-repair-grace",
        vec![text_resp("done")],
        CompletionCase {
            outcomes: vec![false],
            allowed: true,
            budget,
            delay: Duration::ZERO,
            preserve_failed_checks: false,
            completion_check: Some("cargo test -p fixture".into()),
        },
        CancellationToken::new(),
    )
    .await;
    assert!(
        matches!(outcome, Outcome::Halted(ref name) if name == "steps"),
        "requests={} commands={}",
        requests.lock().unwrap().len(),
        commands.lock().unwrap().len()
    );
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert_eq!(commands.lock().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn exhausted_token_budget_skips_the_predicate_and_keeps_budget_outcome() {
    let mut budget = default_budget();
    budget.max_tokens = 1;
    let (outcome, _, commands, _, root, _) = run_completion_case(
        "completion-token-budget",
        vec![text_resp("done")],
        CompletionCase {
            outcomes: vec![true],
            allowed: true,
            budget,
            delay: Duration::ZERO,
            preserve_failed_checks: false,
            completion_check: Some("cargo test -p fixture".into()),
        },
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, Outcome::Halted(ref name) if name == "tokens"));
    assert!(commands.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn action_budget_charges_predicate_and_keeps_budget_outcome() {
    let mut budget = default_budget();
    budget.actions_per_trial = 1;
    let (outcome, state, commands, _, root, _) = run_completion_case(
        "completion-action-budget",
        vec![text_resp("done")],
        CompletionCase {
            outcomes: vec![true],
            allowed: true,
            budget,
            delay: Duration::ZERO,
            preserve_failed_checks: false,
            completion_check: Some("cargo test -p fixture".into()),
        },
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, Outcome::Halted(ref name) if name == "trial-actions"));
    assert_eq!(commands.lock().unwrap().len(), 1);
    assert_eq!(state.budget.counters().actions_this_trial, 1);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn failed_predicate_rolls_back_unless_failed_checks_are_preserved() {
    for (preserve, want_file) in [(false, false), (true, true)] {
        let (outcome, _, _, _, root, survived) = run_completion_case(
            if preserve {
                "completion-preserve"
            } else {
                "completion-rollback"
            },
            vec![text_resp("done"), text_resp("done again")],
            CompletionCase {
                outcomes: vec![false, true],
                allowed: true,
                budget: default_budget(),
                delay: Duration::ZERO,
                preserve_failed_checks: preserve,
                completion_check: Some("cargo test -p fixture".into()),
            },
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(outcome, Outcome::Done));
        assert_eq!(survived, want_file);
        let _ = std::fs::remove_dir_all(root);
    }
}

#[tokio::test]
async fn predicate_timeout_is_a_wallclock_halt() {
    let mut budget = default_budget();
    budget.max_wallclock = Duration::from_millis(500);
    let (outcome, _, commands, _, root, _) = run_completion_case(
        "completion-timeout",
        vec![text_resp("done")],
        CompletionCase {
            outcomes: vec![true],
            allowed: true,
            budget,
            delay: Duration::from_secs(2),
            preserve_failed_checks: false,
            completion_check: Some("cargo test -p fixture".into()),
        },
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, Outcome::Halted(ref name) if name == "wallclock"));
    assert_eq!(commands.lock().unwrap().len(), 0);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn cancellation_during_predicate_is_cancelled_not_completion_failure() {
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.cancel();
    });
    let (outcome, _, _, _, root, _) = run_completion_case(
        "completion-cancel",
        vec![text_resp("done")],
        CompletionCase {
            outcomes: vec![true],
            allowed: true,
            budget: default_budget(),
            delay: Duration::from_secs(10),
            preserve_failed_checks: false,
            completion_check: Some("cargo test -p fixture".into()),
        },
        cancel,
    )
    .await;
    assert!(matches!(outcome, Outcome::Cancelled));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn last_step_grace_cannot_hide_a_simultaneous_token_limit() {
    let mut budget = default_budget();
    budget.max_steps = std::num::NonZeroU32::new(1).unwrap();
    budget.max_tokens = 1;
    let (outcome, _, commands, _, root, _) = run_completion_case(
        "completion-combined-limits",
        vec![text_resp("done")],
        CompletionCase {
            outcomes: vec![true],
            allowed: true,
            budget,
            delay: Duration::ZERO,
            preserve_failed_checks: false,
            completion_check: Some("cargo test -p fixture".into()),
        },
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, Outcome::Halted(_)));
    assert!(commands.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn last_step_passing_predicate_cannot_hide_its_action_limit() {
    let mut budget = default_budget();
    budget.max_steps = std::num::NonZeroU32::new(1).unwrap();
    budget.actions_per_trial = 1;
    let (outcome, _, commands, _, root, _) = run_completion_case(
        "completion-combined-actions",
        vec![text_resp("done")],
        CompletionCase {
            outcomes: vec![true],
            allowed: true,
            budget,
            delay: Duration::ZERO,
            preserve_failed_checks: false,
            completion_check: Some("cargo test -p fixture".into()),
        },
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, Outcome::Halted(ref reason) if reason == "trial-actions"));
    assert_eq!(commands.lock().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn completion_call_ids_do_not_overwrite_model_tool_calls() {
    let (outcome, state, commands, _, root, _) = run_completion_case(
        "completion-call-collision",
        vec![
            script_resp(
                vec![(
                    "harness-completion-1",
                    "test",
                    serde_json::json!({"cmd":"make test"}),
                )],
                StopReason::ToolUse,
            ),
            text_resp("done"),
        ],
        CompletionCase {
            outcomes: vec![true, true],
            allowed: true,
            budget: default_budget(),
            delay: Duration::ZERO,
            preserve_failed_checks: false,
            completion_check: Some("cargo test -p fixture".into()),
        },
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, Outcome::Done));
    assert_eq!(commands.lock().unwrap().len(), 2);
    let calls = state
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::ToolCall { call_id, args, .. } => Some((call_id, args)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0].0, calls[1].0);
    assert_eq!(calls[0].1["cmd"], "make test");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn predicate_obeys_the_same_action_tripwire_without_double_charging() {
    let mut budget = default_budget();
    budget.same_action_cycles = 1;
    let (outcome, state, _, _, root, _) = run_completion_case(
        "completion-same-action",
        vec![
            script_resp(
                vec![(
                    "prior-test",
                    "test",
                    serde_json::json!({"cmd":"cargo test -p fixture"}),
                )],
                StopReason::ToolUse,
            ),
            text_resp("done"),
        ],
        CompletionCase {
            outcomes: vec![true, true],
            allowed: true,
            budget,
            delay: Duration::ZERO,
            preserve_failed_checks: false,
            completion_check: Some("cargo test -p fixture".into()),
        },
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, Outcome::Halted(ref reason) if reason == "same-action"));
    assert_eq!(state.budget.counters().actions_this_trial, 2);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn second_failed_predicate_keeps_a_genuine_action_budget_halt() {
    let mut budget = default_budget();
    budget.actions_per_trial = 2;
    let (outcome, _, commands, _, root, _) = run_completion_case(
        "completion-second-cap",
        vec![text_resp("done"), text_resp("done again")],
        CompletionCase {
            outcomes: vec![false, false],
            allowed: true,
            budget,
            delay: Duration::ZERO,
            preserve_failed_checks: false,
            completion_check: Some("cargo test -p fixture".into()),
        },
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, Outcome::Halted(ref reason) if reason == "trial-actions"));
    assert_eq!(commands.lock().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(root);
}
