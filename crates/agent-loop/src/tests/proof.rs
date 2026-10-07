use crate::proof::note_tool_execution;
use crate::proof::refund_batch;
use crate::proof::snapshot_batch;
use crate::run::drive_tick;
use crate::state::ProviderMsg;
use crate::state::ToolMsg;
use crate::*;
use agent_event::{AgentEvent, Emitter, UsageReport};
use agent_log::ItemKind;
use agent_log::TurnEndReason;
use provider_core::StopReason;
use provider_core::ToolCallRef;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tool_core::ToolResult;

use super::{
    assistant, call, event_order, full_event_order, ok_round, run_kinds, run_registry, run_tmp,
    script_resp, text_resp, text_response, tool_use_message, FakeLlm, GateBatch, RecBets, Scripted,
    RUN_N,
};

#[test]
fn truncation_fails_batch_unexecuted() {
    let mut s = LoopState::new();
    s.turn = 1;
    let before = s.items.len();
    let outcome = s.step_claim(
        assistant(vec![call("a"), call("b")]),
        StopReason::MaxTokens,
        &RunConfig::default(),
    );
    assert!(matches!(outcome, ClaimOutcome::Truncated(2)));
    for id in ["a", "b"] {
        let state = &s.tool_calls[id];
        let res = state.result.as_ref().expect("answered, not dispatched");
        assert!(res.is_error);
    }
    assert!(s.items.len() > before);
    assert_eq!(s.turn_reason, Some(TurnEndReason::MaxTokens));
    s.stick_turn_reason(TurnEndReason::Completed); // sticky: no downgrade
    assert_eq!(s.turn_reason, Some(TurnEndReason::MaxTokens));
    assert!(matches!(
        s.terminate(&RunConfig::default()),
        PhaseVerdict::Return(Outcome::Halted(_))
    ));
}

#[test]
fn crash_repair_synthesizes_results() {
    let mut s = LoopState::new();
    s.turn = 1;
    let outcome = s.step_claim(
        assistant(vec![call("a"), call("b")]),
        StopReason::ToolUse,
        &RunConfig::default(),
    );
    assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
    assert_eq!(s.crash_repair("boom"), 2);
    assert_eq!(s.crash_repair("boom"), 0); // idempotent
    for id in ["a", "b"] {
        let res = s.tool_calls[id].result.as_ref().unwrap();
        assert!(res.is_error);
    }
    assert!(s.batch_complete());
}

#[test]
fn cancelled_provider_msg_refunds_bounded() {
    let mut s = LoopState::new();
    s.turn = 1;
    let root = CancellationToken::new();
    assert!(s.start_provider_call(&root).is_some());
    assert_eq!(s.budget.counters().steps, 1);
    assert!(s.finish_provider_msg(ProviderMsg::Failed {
        turn: 1,
        err: "cancelled".into(),
        cancelled: true,
        usage: None,
    }));
    assert_eq!(s.budget.counters().steps, 0);
    assert_eq!(s.budget.counters().refunds, 1);
    // Floor holds: refunding an empty counter mints nothing.
    s.budget.refund_step();
    assert_eq!(
        (s.budget.counters().steps, s.budget.counters().refunds),
        (0, 1)
    );
}

#[tokio::test]
async fn e2e_truncation_halts_with_no_tool_start() {
    let mut s = LoopState::new();
    let mut emitter = Emitter::new();
    let (_itx, mut irx) = mpsc::channel(8);
    let (ptx, mut prx) = mpsc::channel(8);
    let (_ttx, mut trx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    s.apply_input(Input::User("go".into()));
    s.admit_steering();
    let root = CancellationToken::new();
    assert!(s.start_provider_call(&root).is_some());
    ptx.send(ProviderMsg::Settled {
        turn: 1,
        message: tool_use_message(),
        stop: StopReason::MaxTokens,
        usage: None,
    })
    .await
    .unwrap();
    let v = drive_tick(
        &mut s,
        &mut irx,
        &mut prx,
        &mut trx,
        &cancel,
        &mut emitter,
        &RunConfig::default(),
    )
    .await;
    assert!(matches!(v, PhaseVerdict::Return(Outcome::Halted(ref s)) if s == "max-tokens"));
    // Truncated batch: answered with errors, never dispatched.
    let order = event_order(emitter.history());
    assert!(!order.contains(&"ToolStart"));
    assert!(order.contains(&"TurnEnd"));
    match emitter.history().last().unwrap() {
        AgentEvent::TurnEnd { reason, .. } => {
            assert_eq!(*reason, agent_event::TurnEndReason::MaxSteps)
        }
        other => panic!("expected TurnEnd, got {other:?}"),
    }
    assert_eq!(s.turn_reason, Some(TurnEndReason::MaxTokens));
    // F1a: no provider usage = None on MessageEnd, zero totals on TurnEnd.
    assert!(emitter
        .history()
        .iter()
        .any(|e| matches!(e, AgentEvent::MessageEnd { usage: None, .. })));
    assert!(emitter.history().iter().any(|e| matches!(
        e,
        AgentEvent::TurnEnd { usage_totals, .. }
            if *usage_totals == UsageReport::default()
    )));
}

#[tokio::test]
async fn run_e2e_multi_turn_proven_prefix_kept_failed_batch_rolled_back() {
    use agent_event::check_pairing;
    use agent_log::read_log;
    let root = run_tmp("e2e");
    std::fs::write(root.join("a.rs"), "v1\n").unwrap();
    std::fs::write(root.join("goal.md"), "ship it\n").unwrap();
    let log_path = std::env::temp_dir().join(format!(
        "rof-run-e2e-log-{}-{}.jsonl",
        std::process::id(),
        RUN_N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let order = Arc::new(Mutex::new(Vec::new()));
    let client = FakeLlm {
        order: order.clone(),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([
            script_resp(
                vec![(
                    "c1",
                    "write",
                    serde_json::json!({"path": "a.rs", "content": "v2\n"}),
                )],
                StopReason::ToolUse,
            ),
            script_resp(
                vec![
                    (
                        "c2",
                        "write",
                        serde_json::json!({"path": "b.rs", "content": "new\n"}),
                    ),
                    ("c3", "boom", serde_json::json!({})),
                ],
                StopReason::ToolUse,
            ),
            text_resp("batch rounds done"),
            text_resp("second done"),
            // Held-declare round trip: the first done is held (unverified
            // writes), the second lands Done, the third closes turn 2.
            text_resp("third done"),
        ])),
    };
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    state.followups.push_back("second task".into());
    let mut emitter = Emitter::new();
    let cancel = CancellationToken::new();
    let cfg = RunConfig {
        log_path: Some(log_path.clone()),
        context_files: vec!["goal.md".into()],
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
        vec![Input::User("build it".into())],
        &cancel,
    )
    .await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    // Proven prefix kept on disk, failed batch rolled back.
    assert_eq!(std::fs::read_to_string(root.join("a.rs")).unwrap(), "v2\n");
    assert!(!root.join("b.rs").exists());
    // Budget consumed: one step per model call, tokens + spend metered
    // (five calls: the held declare adds one round trip).
    assert_eq!(state.budget.counters().steps, 5);
    assert_eq!(state.budget.counters().tokens, 75);
    assert_eq!(state.budget.counters().spent_cents, 5);
    // No cap tripped: the one grace step stayed untouched.
    assert!(state.budget.exceeded().is_none());
    // Events ordered and paired across two turns plus the run pair.
    let history = emitter.history();
    assert!(check_pairing(history), "unpaired: {history:?}");
    let kinds = event_order(history);
    assert!(matches!(
        history.first().unwrap(),
        AgentEvent::RunStart { .. }
    ));
    assert!(matches!(history.last().unwrap(), AgentEvent::RunEnd { .. }));
    assert_eq!(kinds.iter().filter(|k| **k == "TurnStart").count(), 2);
    assert_eq!(kinds.iter().filter(|k| **k == "TurnEnd").count(), 2);
    assert_eq!(kinds.iter().filter(|k| **k == "MessageStart").count(), 5);
    assert_eq!(kinds.iter().filter(|k| **k == "MessageEnd").count(), 5);
    assert_eq!(kinds.iter().filter(|k| **k == "ToolStart").count(), 3);
    assert_eq!(kinds.iter().filter(|k| **k == "ToolEnd").count(), 3);
    // File log validates and is balanced: header first.
    let file_items = read_log(&log_path).unwrap();
    assert!(matches!(file_items[0].kind, ItemKind::Header { .. }));
    assert_eq!(file_items.len(), state.items.len());
    let log_kinds = run_kinds(&file_items);
    assert_eq!(
        log_kinds,
        vec![
            "Header",
            "TurnStart",
            "Input",
            "Assistant",
            "ToolCall",
            "ToolResult",
            "Assistant",
            "ToolCall",
            "ToolCall",
            "ToolResult",
            "ToolResult",
            "Attempt",
            "Assistant",
            "Attempt",
            "Assistant",
            "TurnEnd",
            "TurnStart",
            "Input",
            "Assistant",
            "TurnEnd",
        ]
    );
    // Durable hold record: the unverified declare's Attempt carries the
    // nudge text for replay; the model-visible copy rode the next
    // request as its own user-role row.
    assert!(file_items.iter().any(|i| matches!(
        &i.kind,
        ItemKind::Attempt { error, will_retry: true } if error.contains("Unverified declare held")
    )));
    // The failed call is a durable error result, not a throw.
    let boom = file_items
        .iter()
        .find(|i| matches!(&i.kind, ItemKind::ToolResult { call_id, .. } if call_id == "c3"))
        .unwrap();
    assert!(matches!(
        &boom.kind,
        ItemKind::ToolResult { is_error: true, .. }
    ));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&log_path);
}

#[tokio::test]
async fn settle_usage_rides_message_end_and_cumulates_on_turn_end() {
    let root = run_tmp("usage");
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    let mut settled = text_response("done");
    settled.usage.reasoning = Some(3);
    let settled = Scripted::Respond(settled);
    let client = FakeLlm {
        order: Default::default(),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([
            script_resp(
                vec![(
                    "c1",
                    "write",
                    serde_json::json!({"path": "a.txt", "content": "bye\n"}),
                )],
                StopReason::ToolUse,
            ),
            settled,
            // Held-declare round trip: the first done is held (unverified
            // write), the second lands Done.
            text_resp("done"),
        ])),
    };
    let registry = run_registry(&root);
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
            cfg: RunConfig::default(),
        },
        vec![Input::User("edit the note".into())],
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    let history = emitter.history();
    // Every settle reports its own usage on MessageEnd.
    let ends: Vec<&UsageReport> = history
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageEnd { usage, .. } => {
                Some(usage.as_ref().expect("settle must carry usage"))
            }
            _ => None,
        })
        .collect();
    assert_eq!(ends.len(), 3, "one MessageEnd per settle");
    assert_eq!(
        (
            ends[0].input_tokens,
            ends[0].output_tokens,
            ends[0].cost_usd
        ),
        (10, 5, Some(0.01))
    );
    assert_eq!(ends[0].reasoning_tokens, None); // provider reported none
    assert_eq!(ends[1].reasoning_tokens, Some(3));
    assert_eq!(ends[2].reasoning_tokens, None); // post-hold declare
                                                // TurnEnd totals sum all three settles (10 each in, 5 each out).
    let totals = history
        .iter()
        .find_map(|e| match e {
            AgentEvent::TurnEnd { usage_totals, .. } => Some(usage_totals),
            _ => None,
        })
        .expect("turn closes with totals");
    assert_eq!((totals.input_tokens, totals.output_tokens), (30, 15));
    assert_eq!(totals.reasoning_tokens, Some(3)); // reported-only sum, not erased by None
    assert_eq!(totals.cost_usd, Some(0.03));
    assert_eq!(state.usage_totals.input_tokens, 30);
    assert_eq!(state.budget.counters().tokens, 45);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn run_failed_batch_records_rollback_notice_and_queues_it_for_the_model() {
    use agent_log::read_log;
    let root = run_tmp("rollback-notice");
    let log_path = std::env::temp_dir().join(format!(
        "rof-rollback-log-{}-{}.jsonl",
        std::process::id(),
        RUN_N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    std::fs::write(root.join("a.rs"), "v1\n").unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let client = FakeLlm {
        order: Arc::new(Mutex::new(Vec::new())),
        requests: requests.clone(),
        queue: Mutex::new(VecDeque::from([
            script_resp(
                vec![
                    (
                        "c1",
                        "write",
                        serde_json::json!({"path": "a.rs", "content": "v2\n"}),
                    ),
                    ("c2", "boom", serde_json::json!({})),
                ],
                StopReason::ToolUse,
            ),
            script_resp(
                vec![(
                    "c3",
                    "write",
                    serde_json::json!({"path": "a.rs", "content": "v3\n"}),
                )],
                StopReason::ToolUse,
            ),
            text_resp("done"),
            // Held-declare round trip: the first done is held (unverified
            // writes), the second lands Done.
            text_resp("done"),
        ])),
    };
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    let mut emitter = Emitter::new();
    let cancel = CancellationToken::new();
    let outcome = run(
        &mut state,
        Run {
            provider: &client,
            registry: &registry,
            agent: "agent",
            workdir: &root,
            emitter: &mut emitter,
            bets: &NoBets,
            cfg: RunConfig {
                log_path: Some(log_path.clone()),
                ..RunConfig::default()
            },
        },
        vec![Input::User("go".into())],
        &cancel,
    )
    .await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    // The failed batch's edit refunded past the rollback, then the
    // retry re-applied: exactly one live edit remains.
    assert_eq!(state.edits, 1);
    assert_eq!(std::fs::read_to_string(root.join("a.rs")).unwrap(), "v3\n");
    // Durable log-only row, never a model-message row.
    let notices: Vec<&str> = state
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::Attempt {
                error,
                will_retry: true,
            } if error.contains("whole batch was reverted") => Some(error.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(notices.len(), 1, "{notices:?}");
    // Durable: the rollback record is in the log file before the model's
    // next request, and only once.
    let file_items = read_log(&log_path).unwrap();
    let durable = file_items
        .iter()
        .filter(|i| {
            matches!(&i.kind, ItemKind::Attempt { error, .. } if error.contains("whole batch was reverted"))
        })
        .count();
    assert_eq!(durable, 1, "{file_items:?}");
    // The model sees it on the next tool tail, one batch later.
    let reqs = requests.lock().unwrap();
    let seen = |i: usize| {
        reqs[i]
            .messages
            .iter()
            .any(|m| m.content.contains("whole batch was reverted"))
    };
    assert!(!seen(1), "not with the failed batch's own results");
    assert!(seen(2), "next tool tail must carry the notice");
    assert!(state.pending_directives.is_empty());
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&log_path);
}

#[tokio::test]
async fn run_bets_hook_fires_at_step_head_and_post_batch_in_order() {
    let root = run_tmp("bets");
    std::fs::write(root.join("f.txt"), "old\n").unwrap();
    let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let client = FakeLlm {
        order: order.clone(),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([
            script_resp(
                vec![(
                    "c1",
                    "write",
                    serde_json::json!({"path": "f.txt", "content": "new\n"}),
                )],
                StopReason::ToolUse,
            ),
            text_resp("done"),
            // Held-declare round trip: the first done is held (unverified
            // write), the second lands Done.
            text_resp("done"),
        ])),
    };
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    let mut emitter = Emitter::new();
    let cancel = CancellationToken::new();
    let outcome = run(
        &mut state,
        Run {
            provider: &client,
            registry: &registry,
            agent: "agent",
            workdir: &root,
            emitter: &mut emitter,
            bets: &RecBets {
                order: order.clone(),
            },
            cfg: RunConfig::default(),
        },
        vec![Input::User("go".into())],
        &cancel,
    )
    .await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    // Step-head, post-batch, step-head interleave with the three model
    // calls (the held declare adds one round trip, no batch hooks).
    assert_eq!(
        *order.lock().unwrap(),
        vec!["bets", "model", "bets", "bets", "model", "bets", "model"],
    );
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "new\n"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// --- bets site 2: proof-gated post-batch verdicts ---

#[tokio::test]
async fn incremental_proof_keeps_the_proven_leading_prefix() {
    let root = run_tmp("incremental");
    std::fs::write(root.join("a.rs"), "one\n").unwrap();
    std::fs::write(root.join("b.rs"), "two\n").unwrap();
    // Probe: a b.rs containing "BAD" fails. Prefix 1 (a.rs only) passes,
    // prefix 2 (both) fails -> flags [true, false] -> Partial keeps a.rs.
    std::fs::write(
        root.join("probe.py"),
        "import pathlib, sys\nsys.exit(1 if 'BAD' in pathlib.Path('b.rs').read_text() else 0)\n",
    )
    .unwrap();
    let client = FakeLlm {
        order: Arc::new(Mutex::new(Vec::new())),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([
            script_resp(
                vec![
                    (
                        "c1",
                        "write",
                        serde_json::json!({"path": "a.rs", "content": "A2\n"}),
                    ),
                    (
                        "c2",
                        "write",
                        serde_json::json!({"path": "b.rs", "content": "BAD\n"}),
                    ),
                ],
                StopReason::ToolUse,
            ),
            text_resp("done"),
            // Held-declare round trip: the first done is held (unverified
            // writes), the second lands Done.
            text_resp("done"),
        ])),
    };
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    let mut emitter = Emitter::new();
    let cancel = CancellationToken::new();
    let outcome = run(
        &mut state,
        Run {
            provider: &client,
            registry: &registry,
            agent: "agent",
            workdir: &root,
            emitter: &mut emitter,
            bets: &GateBatch,
            cfg: RunConfig {
                proof_cmd: Some("python3 probe.py".into()),
                ..RunConfig::default()
            },
        },
        vec![Input::User("go".into())],
        &cancel,
    )
    .await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    assert_eq!(
        std::fs::read_to_string(root.join("a.rs")).unwrap(),
        "A2\n",
        "proven hunk stays"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("b.rs")).unwrap(),
        "two\n",
        "refuted hunk reverts"
    );
    assert_eq!(state.experiment.ablation.proven_hunks, 1);
    assert_eq!(state.experiment.ablation.rollbacks, 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn empty_hunk_batches_never_inflate_proven_hunks() {
    let root = run_tmp("nohunks");
    std::fs::write(root.join("a.rs"), "one\n").unwrap();
    let client = FakeLlm {
        order: Arc::new(Mutex::new(Vec::new())),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([
            script_resp(
                vec![("c1", "view", serde_json::json!({"path": "a.rs"}))],
                StopReason::ToolUse,
            ),
            text_resp("done"),
        ])),
    };
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    let mut emitter = Emitter::new();
    let cancel = CancellationToken::new();
    let outcome = run(
        &mut state,
        Run {
            provider: &client,
            registry: &registry,
            agent: "agent",
            workdir: &root,
            emitter: &mut emitter,
            bets: &GateBatch,
            cfg: RunConfig::default(),
        },
        vec![Input::User("go".into())],
        &cancel,
    )
    .await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    assert_eq!(
        state.experiment.ablation.proven_hunks, 0,
        "read-only batch: nothing proven, nothing counted"
    );
    assert_eq!(state.experiment.ablation.rollbacks, 0);
    let _ = std::fs::remove_dir_all(&root);
}

/// Verdict hook: keeps only the batch's first hunk, reverts the rest.
struct PartialFirst;

impl BetsHook for PartialFirst {
    fn on_post_batch(&self, claim: &bets::Claim, hunks: &[(String, bool)]) -> bets::CommitVerdict {
        assert!(!claim.predicted_verdict.trim().is_empty());
        assert!(hunks.len() >= 2, "need a multi-hunk batch, got {hunks:?}");
        // Uniform proof: this batch's tool results all passed.
        assert!(hunks.iter().all(|(_, ok)| *ok), "{hunks:?}");
        bets::CommitVerdict::Partial {
            savepoint: bets::Savepoint {
                kept_hunks: hunks[..1].iter().map(|(h, _)| h.clone()).collect(),
                reverted_hunks: hunks[1..].iter().map(|(h, _)| h.clone()).collect(),
            },
        }
    }
}

/// Verdict hook: rejects every batch outright.
struct AbortBatch;

impl BetsHook for AbortBatch {
    fn on_post_batch(&self, _claim: &bets::Claim, hunks: &[(String, bool)]) -> bets::CommitVerdict {
        assert!(!hunks.is_empty(), "gate sees the batch's hunks");
        bets::CommitVerdict::Aborted {
            reason: "stub rejects the batch".into(),
        }
    }
}

#[test]
fn no_bets_defaults_keep_the_run_ungated_and_step_head_delegates() {
    // Site 2 default = permissive Committed: feature off unless a hook opts in.
    let claim = bets::Claim {
        predicted_verdict: "all green".into(),
        on_mismatch: "rollback".into(),
    };
    assert!(matches!(
        NoBets.on_post_batch(&claim, &[]),
        bets::CommitVerdict::Committed
    ));
    assert!(matches!(NoBets.on_step_head(3), PhaseVerdict::Continue));
    // Site 1 default delegates to on_step: an on_step-only hook still gates.
    struct StopOnStep;
    impl BetsHook for StopOnStep {
        fn on_step(&self) -> PhaseVerdict {
            PhaseVerdict::Return(Outcome::Done)
        }
    }
    assert!(matches!(
        StopOnStep.on_step_head(0),
        PhaseVerdict::Return(Outcome::Done)
    ));
}

#[tokio::test]
async fn run_post_batch_partial_keeps_first_hunk_only_and_continues() {
    let root = run_tmp("partial");
    std::fs::write(root.join("a.rs"), "one\n").unwrap();
    std::fs::write(root.join("b.rs"), "two\n").unwrap();
    let client = FakeLlm {
        order: Arc::new(Mutex::new(Vec::new())),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([
            script_resp(
                vec![
                    (
                        "c1",
                        "write",
                        serde_json::json!({"path": "a.rs", "content": "A2\n"}),
                    ),
                    (
                        "c2",
                        "write",
                        serde_json::json!({"path": "b.rs", "content": "B2\n"}),
                    ),
                ],
                StopReason::ToolUse,
            ),
            text_resp("done"),
            // Held-declare round trip: the first done is held (unverified
            // writes), the second lands Done.
            text_resp("done"),
        ])),
    };
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    let mut emitter = Emitter::new();
    let cancel = CancellationToken::new();
    let outcome = run(
        &mut state,
        Run {
            provider: &client,
            registry: &registry,
            agent: "agent",
            workdir: &root,
            emitter: &mut emitter,
            bets: &PartialFirst,
            cfg: RunConfig::default(),
        },
        vec![Input::User("go".into())],
        &cancel,
    )
    .await;
    // Documented semantics: Partial rewrites the tree to the kept prefix,
    // then the run continues to its normal Done.
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    assert_eq!(std::fs::read_to_string(root.join("a.rs")).unwrap(), "A2\n");
    assert_eq!(std::fs::read_to_string(root.join("b.rs")).unwrap(), "two\n");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn run_post_batch_aborted_rolls_back_the_whole_batch() {
    let root = run_tmp("aborted");
    std::fs::write(root.join("a.rs"), "one\n").unwrap();
    let client = FakeLlm {
        order: Arc::new(Mutex::new(Vec::new())),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([
            script_resp(
                vec![(
                    "c1",
                    "write",
                    serde_json::json!({"path": "a.rs", "content": "A2\n"}),
                )],
                StopReason::ToolUse,
            ),
            text_resp("done"),
            // Held-declare round trip: the first done is held (unverified
            // write), the second lands Done.
            text_resp("done"),
        ])),
    };
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    let mut emitter = Emitter::new();
    let cancel = CancellationToken::new();
    let outcome = run(
        &mut state,
        Run {
            provider: &client,
            registry: &registry,
            agent: "agent",
            workdir: &root,
            emitter: &mut emitter,
            bets: &AbortBatch,
            cfg: RunConfig::default(),
        },
        vec![Input::User("go".into())],
        &cancel,
    )
    .await;
    // Documented semantics: Aborted runs the full rollback path, the run
    // itself continues and finishes normally.
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    assert_eq!(std::fs::read_to_string(root.join("a.rs")).unwrap(), "one\n");
    let _ = std::fs::remove_dir_all(&root);
}

/// Rollback refunds batch counters: `edits`, `actions_this_trial` (+ the
/// tripwire streak/sig describing those actions), and
/// `verify.verified_since_write` return to the pre-batch snapshot on a full
/// rollback, so rolled-back edits leave no stale counters behind.
#[test]
fn rollback_refunds_edits_actions_and_verified() {
    let mut s = LoopState::new();
    // Verified baseline: one write covered by a passing test.
    ok_round(&mut s, "w0", "write", serde_json::json!({"path": "a"}));
    ok_round(&mut s, "t0", "test", Value::Null);
    assert!(s.verify.verified_since_write);
    let snap = snapshot_batch(&s);
    let edits_before = s.edits;
    let actions_before = s.budget.counters().actions_this_trial;
    // Claim the batch that the tree will roll back, then apply the
    // shared effects (`run`'s `note_tool_execution` + `record` order).
    let outcome = s.step_claim(
        assistant(vec![ToolCallRef {
            id: "w1".into(),
            name: "write".into(),
            args: serde_json::json!({"path": "b"}),
        }]),
        StopReason::ToolUse,
        &RunConfig::default(),
    );
    assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
    let (wname, wargs) = {
        let c = &s.tool_calls["w1"];
        (c.name.clone(), c.args.clone())
    };
    let w1 = ToolResult {
        content: "wrote b".into(),
        is_error: false,
    };
    let _ = note_tool_execution(&mut s, &wname, &wargs, &w1);
    assert!(s.record_tool_result(ToolMsg {
        call_id: "w1".into(),
        result: w1,
    }));
    // The batch moved the counters and dirtied the verified flag.
    assert_eq!(s.edits, edits_before + 1);
    assert_eq!(s.budget.counters().actions_this_trial, actions_before + 1);
    assert!(!s.verify.verified_since_write);
    // The tree rolled the batch back: refund to the snapshot.
    refund_batch(&mut s, &snap);
    assert_eq!(s.edits, snap.edits);
    assert_eq!(
        s.budget.counters().actions_this_trial,
        snap.actions_this_trial
    );
    assert_eq!(
        s.budget.counters().same_action_streak,
        snap.same_action_streak
    );
    assert_eq!(s.last_sig, snap.last_sig);
    assert_eq!(s.last_obs, snap.last_obs);
    assert_eq!(s.verify.verified_since_write, snap.verified_since_write);
    assert!(s.verify.verified_since_write);
}

#[tokio::test]
async fn char_rollback_event_and_log_sequence() {
    use agent_event::check_pairing;
    let root = run_tmp("char-rollback");
    std::fs::write(root.join("a.rs"), "v1\n").unwrap();
    let client = FakeLlm {
        order: Default::default(),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([
            script_resp(
                vec![
                    (
                        "c1",
                        "write",
                        serde_json::json!({"path": "a.rs", "content": "v2\n"}),
                    ),
                    ("c2", "boom", serde_json::json!({})),
                ],
                StopReason::ToolUse,
            ),
            script_resp(
                vec![(
                    "c3",
                    "write",
                    serde_json::json!({"path": "a.rs", "content": "v3\n"}),
                )],
                StopReason::ToolUse,
            ),
            text_resp("done"),
            text_resp("done"),
        ])),
    };
    let registry = run_registry(&root);
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
            cfg: RunConfig::default(),
        },
        vec![Input::User("go".into())],
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    assert_eq!(std::fs::read_to_string(root.join("a.rs")).unwrap(), "v3\n");
    // Failed batch: rollback Attempt row, then retry, then held declare.
    assert_eq!(
        run_kinds(&state.items),
        vec![
            "Header",
            "TurnStart",
            "Input",
            "Assistant",
            "ToolCall",
            "ToolCall",
            "ToolResult",
            "ToolResult",
            "Attempt",
            "Assistant",
            "ToolCall",
            "ToolResult",
            "Assistant",
            "Attempt",
            "Assistant",
            "TurnEnd",
        ]
    );
    assert_eq!(
        full_event_order(emitter.history()),
        vec![
            "RunStart",
            "TurnStart",
            "MessageStart",
            "MessageUpdate",
            "MessageEnd",
            "ToolStart",
            "ToolStart",
            "ToolEnd",
            "ToolEnd",
            "MessageStart",
            "MessageUpdate",
            "MessageEnd",
            "ToolStart",
            "ToolEnd",
            "MessageStart",
            "MessageUpdate",
            "MessageEnd",
            "MessageStart",
            "MessageUpdate",
            "MessageEnd",
            "TurnEnd",
            "RunEnd",
        ]
    );
    assert!(check_pairing(emitter.history()));
    let _ = std::fs::remove_dir_all(&root);
}
