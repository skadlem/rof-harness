use super::*;

fn checkpoint_state() -> LoopState {
    let mut state = LoopState::new();
    let kinds = [
        ItemKind::Input {
            input_id: "goal".into(),
            text: "Keep the public API unchanged. ".repeat(12),
            source: agent_log::InputSource::External,
        },
        ItemKind::Assistant {
            message: serde_json::to_value(assistant(vec![ToolCallRef {
                id: "read".into(),
                name: "view".into(),
                args: serde_json::json!({"path":"lib.rs"}),
            }]))
            .unwrap(),
            stop_reason: "ToolUse".into(),
            interrupted: false,
            raw_stop_reason: None,
            attempts: None,
        },
        ItemKind::ToolResult {
            call_id: "read".into(),
            content: "observed implementation\n".repeat(30),
            is_error: false,
            recovery: None,
        },
        ItemKind::Input {
            input_id: "recent".into(),
            text: "continue with care".into(),
            source: agent_log::InputSource::External,
        },
    ];
    for (id, kind) in ["user-goal", "model-call", "tool-fact", "recent-user"]
        .into_iter()
        .zip(kinds)
    {
        state.items.push(Item {
            seq: state.items.len() as u64 + 1,
            id: id.into(),
            parent_id: None,
            recorded_at: std::time::SystemTime::now(),
            kind,
        });
    }
    state
}

#[tokio::test]
async fn checkpoint_uses_context_capacity_independently_of_run_allowance_and_resolves_sources() {
    for allowance in [1_000_000, 2_000_000] {
        let mut state = checkpoint_state();
        let mut budget = config_for(Capability::UnattendedBatch);
        budget.max_tokens = allowance;
        state.budget = BudgetGuard::new(budget, Instant::now());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let client = FakeLlm {
            order: Arc::new(Mutex::new(Vec::new())),
            queue: Mutex::new(VecDeque::from([summary_resp(
                r#"{"entries":[{"kind":"fact","text":"Keep API","source_item_ids":["user-goal"]},{"kind":"fact","text":"Observed code","source_item_ids":["tool-fact"]},{"kind":"fact","text":"Unverified claim","source_item_ids":["missing"]},{"kind":"plan","text":"Run public tests","source_item_ids":[]},{"kind":"fact","text":"Unshown source claim","source_item_ids":["recent-user"]}] }"#,
                StopReason::Stop,
                100,
            )])),
            requests: requests.clone(),
        };
        let cfg = RunConfig {
            compaction: context::CompactionConfig {
                enabled: true,
                frac: 0.5,
                keep_tokens: 4,
                context_capacity_tokens: Some(100),
                output_reserve_tokens: 0,
                checkpoint_sources: true,
            },
            ..RunConfig::default()
        };
        let mut emitter = Emitter::new();
        assert!(
            crate::request::checkpoint(&mut state, &client, &cfg, &mut emitter, &mut false).await
        );
        let checkpoint = state.checkpoint.as_ref().unwrap();
        assert!(checkpoint.summary.contains("[fact]") && checkpoint.summary.contains("user-goal"));
        assert!(checkpoint
            .summary
            .contains("[hypothesis]: Unverified claim"));
        assert!(checkpoint.summary.contains("[plan]: Run public tests"));
        assert!(checkpoint
            .summary
            .contains("[hypothesis]: Unshown source claim"));
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let payload = &requests[0].messages[1].content;
        assert!(
            payload.contains("[source \"user-goal\"]")
                && payload.contains("[source \"tool-fact\"]")
        );
        assert!(!payload.contains("[source \"model-call\"]"));
        assert!(payload.ends_with(context::CHECKPOINT_PROMPT));
        assert_eq!(state.usage_totals.input_tokens, 100);
        let folded = state.derived_messages(&cfg);
        assert!(folded[0].content.contains("[fact]"));
    }
}

#[tokio::test]
async fn malformed_sourced_checkpoint_keeps_raw_history_and_meters_the_failed_summary() {
    let mut state = checkpoint_state();
    let before = serde_json::to_string(&state.items).unwrap();
    let client = FakeLlm {
        order: Arc::new(Mutex::new(Vec::new())),
        queue: Mutex::new(VecDeque::from([summary_resp(
            "not JSON",
            StopReason::Stop,
            100,
        )])),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let cfg = RunConfig {
        compaction: context::CompactionConfig {
            enabled: true,
            frac: 0.5,
            keep_tokens: 4,
            context_capacity_tokens: Some(100),
            output_reserve_tokens: 0,
            checkpoint_sources: true,
        },
        ..RunConfig::default()
    };
    let mut emitter = Emitter::new();
    assert!(!crate::request::checkpoint(&mut state, &client, &cfg, &mut emitter, &mut false).await);
    assert!(state.checkpoint.is_none());
    assert_eq!(serde_json::to_string(&state.items).unwrap(), before);
    assert_eq!(state.usage_totals.input_tokens, 100);
    assert!(emitter
        .history()
        .iter()
        .any(|e| matches!(e, AgentEvent::Error { error } if error.code == "compaction-refused")));
}

#[test]
fn repeated_checkpoint_keeps_source_mapping_without_inventing_an_id_for_the_summary() {
    let mut state = checkpoint_state();
    state.checkpoint = Some(Checkpoint {
        keep_from: 3,
        summary: "[fact] earlier facts".into(),
    });
    let cfg = RunConfig::default();
    let folded = state.derived_messages(&cfg);
    let view = crate::request::checkpoint_view(&state, &folded);
    assert_eq!(view.len(), 2);
    assert_eq!(view[0].source_item_id, None);
    assert_eq!(view[1].source_item_id.as_deref(), Some("recent-user"));
}
