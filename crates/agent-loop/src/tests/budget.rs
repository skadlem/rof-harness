use crate::request::build_request;
use crate::run::drive_tick;
use crate::state::ToolMsg;
use crate::*;
use agent_event::AgentEvent;
use agent_event::Emitter;
use agent_log::InputSource;
use agent_log::ItemKind;
use provider_core::StopReason;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{
    assistant, call, result, run_registry, run_tmp, script_resp, text_resp, FakeLlm, Scripted,
    RUN_N,
};

#[test]
fn lessons_cap_at_three() {
    let mut s = LoopState::new();
    for i in 0..4 {
        s.push_lesson(format!("lesson-{i}"));
    }
    assert_eq!(s.lessons.len(), 3);
    assert!(!s.lessons.iter().any(|l| l == "lesson-0"));
    assert_eq!(s.lessons[2], "lesson-3");
}

#[test]
fn budget_nudge_appends_to_tool_result_tail_once() {
    let mut s = LoopState::new();
    s.turn = 1;
    let outcome = s.step_claim(
        assistant(vec![call("a")]),
        StopReason::ToolUse,
        &RunConfig::default(),
    );
    assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
    assert!(s.record_tool_result(ToolMsg {
        call_id: "a".into(),
        result: result(),
    }));
    let warn = s.budget.config().warn_steps.get();
    s.budget.counters_mut().steps = warn;
    let text = s.apply_budget_nudge().expect("nudge due at warn");
    assert!(text.contains("wrap up"));
    match s.items.last().map(|i| &i.kind) {
        Some(ItemKind::ToolResult { content, .. }) => assert!(content.contains(&text)),
        other => panic!("nudge must land on the ToolResult tail, got {other:?}"),
    }
    assert!(s.apply_budget_nudge().is_none()); // latched
}

#[test]
fn no_tail_keeps_directive_queued_and_nudge_latch_unburned() {
    let mut s = LoopState::new();
    // Nudge due at warn steps, but tailless: the latch is not consumed.
    s.budget.counters_mut().steps = s.budget.config().warn_steps.get();
    assert!(s.apply_budget_nudge().is_none());
    // A queued directive with no tail stays queued (retry, not drop).
    s.budget.counters_mut().actions_this_trial = s.budget.config().actions_per_trial / 2;
    s.queue_directives(&RunConfig::default());
    assert_eq!(s.pending_directives.len(), 1);
    assert_eq!(s.deliver_directives(), 0);
    assert_eq!(s.pending_directives.len(), 1);
    // First recorded result creates the tail: both land, once each.
    s.turn = 1;
    assert!(matches!(
        s.step_claim(
            assistant(vec![call("a")]),
            StopReason::ToolUse,
            &RunConfig::default()
        ),
        ClaimOutcome::Dispatch(_)
    ));
    assert!(s.record_tool_result(ToolMsg {
        call_id: "a".into(),
        result: result(),
    }));
    assert_eq!(s.deliver_directives(), 1);
    let nudge = s
        .apply_budget_nudge()
        .expect("latch survived the tailless attempt");
    assert!(nudge.contains("wrap up"));
    match s.items.last().map(|i| &i.kind) {
        Some(ItemKind::ToolResult { content, .. }) => {
            assert!(content.contains("0 edits so far after 15 actions"));
            assert!(content.contains(&nudge));
        }
        other => panic!("expected ToolResult tail, got {other:?}"),
    }
    assert!(s.pending_directives.is_empty());
    assert_eq!(s.deliver_directives(), 0); // delivered once, never twice
}

#[test]
fn directive_triggers_fire_once_at_half_and_late_cap() {
    let mut s = LoopState::new();
    let cap = s.budget.config().actions_per_trial;
    s.budget.counters_mut().actions_this_trial = cap / 2;
    s.queue_directives(&RunConfig::default());
    assert_eq!(
        s.pending_directives.front().unwrap(),
        "0 edits so far after 15 actions. Stop reading. Apply your first edit with the edit tool NOW."
    );
    s.queue_directives(&RunConfig::default());
    assert_eq!(s.pending_directives.len(), 1); // half-cap latch holds
    s.edits = 1; // an edit silences only the zero-edit rule
    s.budget.counters_mut().actions_this_trial = cap * 4 / 5;
    s.queue_directives(&RunConfig::default());
    assert_eq!(
        s.pending_directives.back().unwrap(),
        "only 6 actions remain before the run is stopped. Finish and submit your patch now."
    );
    s.queue_directives(&RunConfig::default());
    assert_eq!(s.pending_directives.len(), 2); // both one-shot
}

#[test]
fn lessons_ride_the_tail_once_each() {
    let mut s = LoopState::new();
    s.push_lesson("vary the approach".into());
    s.turn = 1;
    assert!(matches!(
        s.step_claim(
            assistant(vec![call("a")]),
            StopReason::ToolUse,
            &RunConfig::default()
        ),
        ClaimOutcome::Dispatch(_)
    ));
    assert!(s.record_tool_result(ToolMsg {
        call_id: "a".into(),
        result: result(),
    }));
    assert_eq!(s.deliver_directives(), 1);
    match s.items.last().map(|i| &i.kind) {
        Some(ItemKind::ToolResult { content, .. }) => {
            assert!(content.contains("vary the approach"))
        }
        other => panic!("expected ToolResult tail, got {other:?}"),
    }
    assert!(s.lessons.is_empty());
    assert_eq!(s.deliver_directives(), 0); // each lesson delivered once
}

#[tokio::test]
async fn run_half_cap_zero_edits_directive_lands_on_a_tool_tail() {
    use agent_log::read_log;
    let root = run_tmp("directive");
    let log_path = std::env::temp_dir().join(format!(
        "rof-directive-log-{}-{}.jsonl",
        std::process::id(),
        RUN_N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let requests = Arc::new(Mutex::new(Vec::new()));
    // 15 read-only actions (= cap/2) with zero edits: the directive fires.
    let mut queue: VecDeque<Scripted> = VecDeque::new();
    for i in 1..=15 {
        let id = format!("r{i}");
        queue.push_back(script_resp(
            vec![(id.as_str(), "read", serde_json::json!({"n": i}))],
            StopReason::ToolUse,
        ));
    }
    queue.push_back(text_resp("done"));
    let client = FakeLlm {
        order: Arc::new(Mutex::new(Vec::new())),
        requests: requests.clone(),
        queue: Mutex::new(queue),
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
        vec![Input::User("probe it".into())],
        &cancel,
    )
    .await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    assert_eq!(state.edits, 0);
    assert_eq!(state.budget.counters().actions_this_trial, 15);
    let hits: Vec<&str> = state
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::ToolResult { content, .. }
                if content.contains("0 edits so far after 15 actions") =>
            {
                Some(content.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].contains("Stop reading. Apply your first edit with the edit tool NOW."));
    assert!(state.pending_directives.is_empty());
    // The next model call sees it, on the 15th action's own tail.
    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 16);
    assert!(
        reqs[15]
            .messages
            .iter()
            .any(|m| m.content.contains("0 edits so far after 15 actions")),
        "{:?}",
        reqs[15].messages
    );
    // Durability: the file row is exactly the durable row the model saw
    // (the request-scoped budget line rides the wire copy only).
    let file_items = read_log(&log_path).unwrap();
    assert!(file_items.iter().any(|i| matches!(
        &i.kind,
        ItemKind::ToolResult { content, .. }
            if content.contains("0 edits so far after 15 actions")
    )));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&log_path);
}

#[tokio::test]
async fn run_halt_budget_steps_after_grace_with_budget_frame() {
    assert_eq!(RunConfig::default().drain_timeout, Duration::from_secs(30));
    let root = run_tmp("halt");
    let order = Arc::new(Mutex::new(Vec::new()));
    let client = FakeLlm {
        order,
        requests: Default::default(),
        queue: Mutex::new(VecDeque::new()),
    };
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    let max = state.budget.config().max_steps.get();
    state.budget.counters_mut().steps = max;
    let mut emitter = Emitter::new();
    let cancel = CancellationToken::new();
    // Configurable drain: 5s here proves the knob, 30s is the default.
    let cfg = RunConfig {
        drain_timeout: Duration::from_secs(5),
        ..RunConfig::default()
    };
    assert_eq!(cfg.drain_timeout, Duration::from_secs(5));
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
        vec![Input::User("too late".into())],
        &cancel,
    )
    .await;
    assert!(
        matches!(outcome, Outcome::Halted(ref s) if s == "steps"),
        "got {outcome:?}"
    );
    assert!(state.budget.may_step().is_err()); // grace ran first, halt after
    match emitter.history().last().unwrap() {
        AgentEvent::RunEnd { outcome, .. } => {
            assert!(matches!(outcome, agent_event::RunOutcome::Failed(_)))
        }
        other => panic!("expected RunEnd, got {other:?}"),
    }
    let turn_end = emitter
        .history()
        .iter()
        .find(|e| matches!(e, AgentEvent::TurnEnd { .. }))
        .unwrap();
    assert!(matches!(
        turn_end,
        AgentEvent::TurnEnd {
            reason: agent_event::TurnEndReason::BudgetExceeded,
            ..
        }
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn run_halt_same_action_and_crash_input_source() {
    // Same action + same observation x4 trips the tripwire; lessons stay capped.
    let root = run_tmp("trip");
    std::fs::write(root.join("f.txt"), "x\n").unwrap();
    let order = Arc::new(Mutex::new(Vec::new()));
    let same = || {
        script_resp(
            vec![(
                "c",
                "write",
                serde_json::json!({"path": "f.txt", "content": "x\n"}),
            )],
            StopReason::ToolUse,
        )
    };
    let client = FakeLlm {
        order,
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([same(), same(), same(), same()])),
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
            cfg: RunConfig::default(),
        },
        vec![Input::User("again".into())],
        &cancel,
    )
    .await;
    assert!(
        matches!(outcome, Outcome::Halted(ref s) if s == "same-action"),
        "got {outcome:?}"
    );
    assert!(state.lessons.len() <= 3);
    let _ = std::fs::remove_dir_all(&root);
    // Crash queues like User but keeps the Crash source in the log.
    let root = run_tmp("crash");
    let order = Arc::new(Mutex::new(Vec::new()));
    let client = FakeLlm {
        order,
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([text_resp("recovered")])),
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
        vec![Input::Crash("boom state".into())],
        &cancel,
    )
    .await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    // items[0] is the run Header, [1] the TurnStart, [2] the admitted Crash.
    match &state.items[2].kind {
        ItemKind::Input { source, text, .. } => {
            assert_eq!(*source, InputSource::Crash);
            assert_eq!(text, "boom state");
        }
        other => panic!("expected Crash Input, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn e2e_budget_halt_after_grace_fires_once() {
    let mut s = LoopState::new();
    let mut emitter = Emitter::new();
    let (itx, mut irx) = mpsc::channel(8);
    let (_ptx, mut prx) = mpsc::channel(8);
    let (_ttx, mut trx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let max = s.budget.config().max_steps.get();
    s.budget.counters_mut().steps = max;
    // Grace fires exactly once through the loop's guard.
    assert!(s.budget.may_step().is_ok());
    assert!(s.budget.may_step().is_err());
    // Step head now refuses; the tick halts with a BudgetExceeded frame.
    let root = CancellationToken::new();
    assert!(s.start_provider_call(&root).is_none());
    // A live inbox send drives the tick (a senders-dropped tick stalls here).
    itx.send(Input::User("too late".into())).await.unwrap();
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
    assert!(matches!(v, PhaseVerdict::Return(Outcome::Halted(ref s)) if s == "steps"));
    match emitter.history().last().unwrap() {
        AgentEvent::TurnEnd { reason, .. } => {
            assert_eq!(*reason, agent_event::TurnEndReason::BudgetExceeded)
        }
        other => panic!("expected TurnEnd, got {other:?}"),
    }
}

#[test]
fn incentives_levels_gate_contract_and_directive_channel() {
    let root = run_tmp("incentives");
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    // Base: no workflow contract in the system string, directives dropped.
    let cfg = RunConfig {
        incentives: IncentivesLevel::Base,
        ..RunConfig::default()
    };
    let r = build_request(&mut state, &registry, &root, &cfg);
    assert!(!r.messages[0].content.contains("WORKFLOW CONTRACT"));
    state.push_directive("go".into(), &cfg);
    assert!(state.pending_directives.is_empty(), "Base drops directives");
    // Contract: contract on, directives still off.
    let cfg = RunConfig {
        incentives: IncentivesLevel::Contract,
        ..RunConfig::default()
    };
    let r = build_request(&mut state, &registry, &root, &cfg);
    assert!(r.messages[0].content.contains("WORKFLOW CONTRACT"));
    state.push_directive("go".into(), &cfg);
    assert!(
        state.pending_directives.is_empty(),
        "Contract drops directives"
    );
    // Full (default = current behavior): both live.
    let cfg = RunConfig::default();
    state.push_directive("go".into(), &cfg);
    assert_eq!(state.pending_directives.len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

/// (5) already-started final step: `record_step` hit max at the head, the
/// admitted step's Done still lands Done instead of Halted(steps).
#[tokio::test]
async fn last_step_done_wins_over_steps_halt() {
    let root = run_tmp("last-done");
    let client = FakeLlm {
        order: Default::default(),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([text_resp("finished")])),
    };
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    let max = state.budget.config().max_steps.get();
    state.budget.counters_mut().steps = max - 1;
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
    assert_eq!(state.budget.counters().steps, max);
    let _ = std::fs::remove_dir_all(&root);
}
