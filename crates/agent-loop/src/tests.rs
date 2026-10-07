use crate::proof::{note_tool_execution, outcome_to_result, refund_batch, snapshot_batch};
use crate::request::{build_request, CHECKPOINT_PREFIX};
use crate::state::{turn_end_reason_to_event, STEP_RETRY_BUDGET};
use crate::verify::{is_verification_call, stop_label, VERIFY_NUDGE};
use crate::*;
use agent_budget::{config_for, BudgetGuard, BudgetHalt, Capability};
use agent_event::{AgentEvent, Emitter, UsageReport};
use agent_log::{InputSource, Item, ItemKind, TurnEndReason};
use provider_core::{AssistantMessage, ProviderMessage, StopReason, Usage};
use serde_json::Value;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tool_core::{ToolCall, ToolOutcome, ToolResult};

use provider_core::ToolCallRef;

fn assistant(calls: Vec<ToolCallRef>) -> AssistantMessage {
    AssistantMessage {
        content: "thinking".into(),
        tool_calls: calls,
        thinking: None,
    }
}

fn call(id: &str) -> ToolCallRef {
    ToolCallRef {
        id: id.into(),
        name: "edit".into(),
        args: Value::Null,
    }
}

fn settled(turn: u64) -> ProviderMsg {
    ProviderMsg::Settled {
        turn,
        message: assistant(vec![]),
        stop: StopReason::Stop,
        usage: None,
    }
}

fn result() -> ToolResult {
    ToolResult {
        content: "ok".into(),
        is_error: false,
    }
}

#[test]
fn gate_transitions_idempotent() {
    let gate = EffectGate::new();
    assert_eq!(gate.status(), GateStatus::Open);
    assert!(gate.admit().is_ok());
    gate.begin_abort();
    assert_eq!(gate.status(), GateStatus::Aborting);
    assert!(matches!(gate.admit(), Err(GateError::AbortRequested)));
    gate.begin_abort(); // idempotent: still aborting, not closed
    assert_eq!(gate.status(), GateStatus::Aborting);
    let token = CancellationToken::new();
    gate.signal_abort(&token);
    assert!(token.is_cancelled());
    gate.signal_abort(&token); // idempotent propagate
    assert!(token.is_cancelled());
    gate.close("boom".into());
    assert_eq!(gate.status(), GateStatus::Closed);
    assert!(matches!(gate.admit(), Err(GateError::Closed(_))));
    gate.begin_abort(); // close wins over a late abort
    assert_eq!(gate.status(), GateStatus::Closed);
}

#[test]
fn should_call_model_truth_table() {
    let mut s = LoopState::new();
    assert!(!s.should_call_model()); // fresh: nothing to do
    s.call_model = true;
    assert!(s.should_call_model());
    s.gate.begin_abort();
    assert!(!s.should_call_model()); // aborting gate says nay
    s.gate.close("x".into());
    assert!(!s.should_call_model());
    let mut s = LoopState::new();
    s.call_model = true;
    s.stop_hard = true;
    assert!(!s.should_call_model());
    let mut s = LoopState::new(); // pending steering, free slot
    s.steering.push_back("hi".into());
    assert!(s.should_call_model());
    s.in_flight = Some(InFlight {
        turn: 0,
        token: CancellationToken::new(),
    });
    assert!(!s.should_call_model()); // single-flight occupied
    let mut s = LoopState::new(); // pending steering, tool still open
    s.steering.push_back("hi".into());
    s.tool_calls.insert(
        "c1".into(),
        ToolCallState {
            name: "edit".into(),
            args: "null".into(),
            is_verification: false,
            result: None,
        },
    );
    assert!(!s.should_call_model());
}

#[test]
fn stale_turn_guard_drops_late_provider_msg() {
    let mut s = LoopState::new();
    s.turn = 1;
    let root = CancellationToken::new();
    assert!(s.start_provider_call(&root).is_some());
    s.turn = 2; // interrupt opened a newer turn
    assert!(!s.finish_provider_msg(settled(1))); // stale: dropped
    assert!(s.in_flight.is_some());
    assert!(s.finish_provider_msg(settled(2)));
    assert!(s.in_flight.is_none());
}

#[test]
fn single_flight_provider_call() {
    let mut s = LoopState::new();
    s.turn = 1;
    let root = CancellationToken::new();
    let child = s.start_provider_call(&root).expect("first starts");
    assert!(!child.is_cancelled());
    assert!(s.start_provider_call(&root).is_none()); // second refused
    assert_eq!(s.budget.counters().steps, 1);
    assert!(s.finish_provider_msg(settled(1)));
    assert!(s.start_provider_call(&root).is_some()); // slot freed
}
#[test]
fn termination_order() {
    let mut s = LoopState::new(); // fresh parks, it does not exit
    assert!(matches!(s.terminate(), PhaseVerdict::Continue));
    let mut s = LoopState::new(); // cancel beats budget
    s.stop_hard = true;
    s.budget.counters_mut().steps = s.budget.config().max_steps.get();
    assert!(matches!(
        s.terminate(),
        PhaseVerdict::Return(Outcome::Cancelled)
    ));
    let mut s = LoopState::new(); // hard error beats budget
    s.fatal_error = Some("e".into());
    let max = s.budget.config().max_steps.get();
    s.budget.counters_mut().steps = max;
    assert!(matches!(
        s.terminate(),
        PhaseVerdict::Return(Outcome::Failed(_))
    ));
    let mut s = LoopState::new(); // budget beats done-shaped state
    s.stop_when_idle = true;
    let max = s.budget.config().max_steps.get();
    s.budget.counters_mut().steps = max;
    assert!(matches!(
        s.terminate(),
        PhaseVerdict::Return(Outcome::Halted(_))
    ));
    let mut s = LoopState::new(); // refusal is terminal-with-error
    s.turn_reason = Some(TurnEndReason::Error("refused".into()));
    assert!(matches!(
        s.terminate(),
        PhaseVerdict::Return(Outcome::Failed(_))
    ));
    let mut s = LoopState::new(); // idle + StopWhenIdle exits
    s.stop_when_idle = true;
    assert!(matches!(s.terminate(), PhaseVerdict::Return(Outcome::Done)));
    // guard halt labels surface verbatim
    let mut s = LoopState::new();
    let max = s.budget.config().max_steps.get();
    s.budget.counters_mut().steps = max;
    assert!(matches!(
        s.terminate(),
        PhaseVerdict::Return(Outcome::Halted(ref s)) if s == "steps"
    ));
}

#[test]
fn truncation_fails_batch_unexecuted() {
    let mut s = LoopState::new();
    s.turn = 1;
    let before = s.items.len();
    let outcome = s.step_claim(assistant(vec![call("a"), call("b")]), StopReason::MaxTokens);
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
        s.terminate(),
        PhaseVerdict::Return(Outcome::Halted(_))
    ));
}

#[test]
fn steering_admitted_only_at_pre_boundary() {
    let mut s = LoopState::new();
    s.phase = Phase::Running;
    let items = s.items.len();
    s.apply_input(Input::User("steer".into()));
    assert_eq!(s.steering.len(), 1);
    assert_eq!(s.items.len(), items); // invisible until the boundary
    assert_eq!(s.admit_steering(), 1);
    assert!(s.steering.is_empty());
    assert_eq!(s.items.len(), items + 1);

    let mut s = LoopState::new(); // idle input opens a new turn
    s.apply_input(Input::User("hi".into()));
    assert_eq!((s.turn, s.phase), (1, Phase::Running));
    assert!(matches!(s.items[0].kind, ItemKind::TurnStart { .. }));

    let mut s = LoopState::new(); // maintenance latches, idle replays
    s.phase = Phase::Maintenance;
    s.apply_input(Input::User("m".into()));
    assert!(s.wake_requested);
    s.phase = Phase::Idle;
    s.admit_steering();
    assert_eq!((s.phase, s.turn), (Phase::Running, 1));
    assert!(!s.wake_requested);
}

#[test]
fn turn_guard_drop_appends_turn_end() {
    let mut items = Vec::new();
    {
        let _guard = TurnGuard::open(&mut items, 3);
    }
    assert!(matches!(
        items.last().map(|i| &i.kind),
        Some(ItemKind::TurnEnd { turn_id, reason: TurnEndReason::Completed })
        if turn_id == "turn-3"
    ));
    {
        let guard = TurnGuard::open(&mut items, 4);
        guard.end(TurnEndReason::Budget);
    }
    assert!(matches!(
        items.last().map(|i| &i.kind),
        Some(ItemKind::TurnEnd {
            reason: TurnEndReason::Budget,
            ..
        })
    ));
}

#[test]
fn stop_hard_drains_bounded() {
    let mut s = LoopState::new(); // busy + generous deadline: keep draining
    s.stop_hard = true;
    s.tool_calls.insert(
        "c1".into(),
        ToolCallState {
            name: "edit".into(),
            args: "null".into(),
            is_verification: false,
            result: None,
        },
    );
    assert!(matches!(s.terminate(), PhaseVerdict::Break));
    s.drain_timeout = Duration::ZERO; // deadline passes: terminal now
    s.drain_until = None;
    assert!(matches!(
        s.terminate(),
        PhaseVerdict::Return(Outcome::Cancelled)
    ));
    let mut s = LoopState::new(); // idle cancels at once
    s.stop_hard = true;
    assert!(matches!(
        s.terminate(),
        PhaseVerdict::Return(Outcome::Cancelled)
    ));
}

#[test]
fn in_step_retry_then_hard_exit() {
    let mut s = LoopState::new();
    s.turn = 1;
    let root = CancellationToken::new();
    s.start_provider_call(&root);
    assert!(s.finish_provider_msg(ProviderMsg::Failed {
        turn: 1,
        err: "flaky".into(),
        cancelled: false,
        usage: None,
    }));
    assert!(s.call_model); // retry reuses the open step
    assert!(s.in_flight.is_none());
    assert!(matches!(s.terminate(), PhaseVerdict::Continue));
    s.step_retries = 0;
    s.start_provider_call(&root);
    assert!(s.finish_provider_msg(ProviderMsg::Failed {
        turn: 1,
        err: "dead".into(),
        cancelled: false,
        usage: None,
    }));
    assert_eq!(s.fatal_error.as_deref(), Some("dead"));
    assert!(matches!(
        s.terminate(),
        PhaseVerdict::Return(Outcome::Failed(_))
    ));
    assert!(!s.should_call_model()); // closed gate says nay
}

#[test]
fn failed_attempt_usage_is_metered_and_still_retries() {
    let mut s = LoopState::new();
    s.turn = 1;
    let root = CancellationToken::new();
    assert!(s.start_provider_call(&root).is_some());
    let u = Usage {
        input: 100,
        output: 20,
        cache_read: 0,
        cache_write: 0,
        reasoning: Some(5),
        cost_usd: Some(0.02),
    };
    assert!(s.finish_provider_msg(ProviderMsg::Failed {
        turn: 1,
        err: "output truncated at 64 tokens".into(),
        cancelled: false,
        usage: Some(u),
    }));
    // Budget and run totals see the failed attempt's spend.
    assert_eq!(s.budget.counters().tokens, 120);
    assert_eq!(s.budget.counters().spent_cents, 2);
    assert_eq!(s.usage_totals.input_tokens, 100);
    assert_eq!(s.usage_totals.output_tokens, 20);
    assert_eq!(s.usage_totals.reasoning_tokens, Some(5));
    assert_eq!(s.usage_totals.cost_usd, Some(0.02));
    // The Attempt row still drives the in-step retry.
    assert!(s.call_model);
    assert!(matches!(
        s.items.last().map(|i| &i.kind),
        Some(ItemKind::Attempt {
            will_retry: true,
            ..
        })
    ));
}

#[test]
fn crash_repair_synthesizes_results() {
    let mut s = LoopState::new();
    s.turn = 1;
    let outcome = s.step_claim(assistant(vec![call("a"), call("b")]), StopReason::ToolUse);
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
fn tripwire_and_thirty_action_abort() {
    let mut s = LoopState::new();
    assert_eq!(s.observe_action("a", "o"), None);
    assert_eq!(s.observe_action("a", "o"), None);
    assert_eq!(s.observe_action("a", "o"), None);
    assert_eq!(s.observe_action("a", "o"), Some(BudgetHalt::SameAction));
    let mut s = LoopState::new();
    let mut last = None;
    for i in 0..30 {
        last = s.observe_action(&format!("act-{i}"), "o");
    }
    assert!(matches!(last, Some(BudgetHalt::TrialActions)));
    assert!(matches!(
        s.terminate(),
        PhaseVerdict::Return(Outcome::Halted(_))
    ));
    // varying the observation resets the streak
    let mut s = LoopState::new();
    for _ in 0..3 {
        s.observe_action("a", "o");
    }
    assert_eq!(s.observe_action("a", "different"), None);
}

#[test]
fn derived_messages_fold_history() {
    let mut s = LoopState::new();
    s.phase = Phase::Running;
    s.apply_input(Input::User("build it".into()));
    s.admit_steering();
    let msgs = s.derived_messages();
    assert_eq!(msgs.len(), 1);
    assert_eq!(
        (msgs[0].role.as_str(), msgs[0].content.as_str()),
        ("user", "build it")
    );
}

#[test]
fn derived_messages_collapse_5_and_ids() {
    let mut s = LoopState::new();
    for i in 0..7 {
        s.items.push(Item {
            seq: s.items.len() as u64,
            id: format!("t{i}"),
            parent_id: None,
            recorded_at: SystemTime::now(),
            kind: ItemKind::ToolResult {
                call_id: format!("c{i}"),
                content: format!("line{i}-head\nline{i}-tail"),
                is_error: false,
                recovery: None,
            },
        });
    }
    let msgs = s.derived_messages();
    assert_eq!(msgs.len(), 7);
    assert_eq!(
        msgs[0].content,
        "[collapsed: 21b — re-open to edit] line0-head\nline0-tail"
    );
    assert_eq!(
        msgs[1].content,
        "[collapsed: 21b — re-open to edit] line1-head\nline1-tail"
    );
    assert_eq!(msgs[2].content, "line2-head\nline2-tail");
    assert_eq!(msgs[2].tool_call_id.as_deref(), Some("c2"));
}

/// Tool-only history of `n` rows, content `obs-{i}` (5 bytes each).
fn tool_history(n: usize) -> LoopState {
    let mut s = LoopState::new();
    for i in 0..n {
        s.items.push(Item {
            seq: s.items.len() as u64,
            id: format!("t{i}"),
            parent_id: None,
            recorded_at: SystemTime::now(),
            kind: ItemKind::ToolResult {
                call_id: format!("c{i}"),
                content: format!("obs-{i}"),
                is_error: false,
                recovery: None,
            },
        });
    }
    s
}

/// The boundary a fold used: the contiguous leading stub prefix (fixtures
/// are tool-only, so it is also the stub count). A stub behind a verbatim
/// row would mean the fold moved the boundary one row at a time.
fn stub_boundary(msgs: &[ProviderMessage]) -> usize {
    const STUB: &str = "[collapsed: ";
    let b = msgs.iter().filter(|m| m.content.starts_with(STUB)).count();
    assert!(
        msgs[..b].iter().all(|m| m.content.starts_with(STUB)),
        "stubs must be the leading prefix"
    );
    assert!(
        msgs[b..].iter().all(|m| !m.content.starts_with(STUB)),
        "stub behind a verbatim row: boundary moved one row at a time"
    );
    b
}

/// H=0 is today's collapse-5 fold, byte for byte: 3 of 8 tool rows stub,
/// the last 5 stay verbatim; the env-less default folds the same bytes.
#[test]
fn collapse_hysteresis_zero_is_byte_identical_to_collapse_5() {
    let s = tool_history(8);
    let msgs = s.raw_messages_with(0);
    let got: Vec<(&str, String, Option<&str>)> = msgs
        .iter()
        .map(|m| {
            (
                m.role.as_str(),
                m.content.clone(),
                m.tool_call_id.as_deref(),
            )
        })
        .collect();
    let stub = |i: usize| format!("[collapsed: 5b — re-open to edit] obs-{i}");
    let want = vec![
        ("tool", stub(0), Some("c0")),
        ("tool", stub(1), Some("c1")),
        ("tool", stub(2), Some("c2")),
        ("tool", "obs-3".into(), Some("c3")),
        ("tool", "obs-4".into(), Some("c4")),
        ("tool", "obs-5".into(), Some("c5")),
        ("tool", "obs-6".into(), Some("c6")),
        ("tool", "obs-7".into(), Some("c7")),
    ];
    assert_eq!(got, want);
    // Cached default (no env read) folds the byte-identical path.
    assert_eq!(
        serde_json::to_string(&s.raw_messages()).unwrap(),
        serde_json::to_string(&msgs).unwrap()
    );
}

/// H=5 over a growing history: one boundary move per 5 added rows, each
/// moving a 5-row batch (H=0 moves every row: 25 moves vs 5). The verbatim
/// window stays inside [COLLAPSE_KEEP, COLLAPSE_KEEP + H] throughout.
#[test]
fn collapse_hysteresis_moves_boundary_once_per_h_rows_in_one_batch() {
    let folds: Vec<(usize, usize)> = (1..=30)
        .map(|t| (t, stub_boundary(&tool_history(t).raw_messages_with(5))))
        .collect();
    let mut moves = Vec::new();
    let mut prev = 0;
    for &(t, b) in &folds {
        assert_eq!(b, 5 * (t.saturating_sub(5) / 5), "boundary at t={t}");
        let window = t - b;
        assert!(
            (5.min(t)..=10).contains(&window),
            "t={t}: verbatim window {window} outside [KEEP, KEEP+H]"
        );
        if b != prev {
            moves.push(t);
            prev = b;
        }
    }
    // Exact move turns; between moves the stub set never grows.
    assert_eq!(moves, vec![10, 15, 20, 25, 30]);
    for w in folds.windows(2) {
        let delta = w[1].1 - w[0].1;
        assert!(
            delta == 0 || delta == 5,
            "t={}: boundary moved {delta} rows, not one H-batch",
            w[1].0
        );
    }
    // H=0 control: the boundary tracks the tail, one row per fold.
    let h0: Vec<usize> = (1..=30)
        .map(|t| stub_boundary(&tool_history(t).raw_messages_with(0)))
        .collect();
    assert_eq!(
        h0,
        (1..=30usize)
            .map(|t| t.saturating_sub(5))
            .collect::<Vec<_>>()
    );
    assert_eq!(h0.iter().filter(|&&b| b != 0).count(), 25);
    assert_eq!(moves.len(), 5);
}

/// Run-head snapshot: both fold knobs resolve from the env once, and a
/// mid-run env change no longer flips the fold. This is the only test
/// that sets these vars; the fold itself never reads them (only
/// `resolve_fold_config` does) and run-path tests assert behavior, not
/// fold bytes, so the save/restore window cannot flip a parallel test.
#[test]
fn fold_knobs_frozen_at_run_head_mid_run_env_change_ignored() {
    let saved_thinking = std::env::var("THINKING_KEEP").ok();
    let saved_hyst = std::env::var("COLLAPSE_HYSTERESIS").ok();
    // Fixture with both signals: 8 tool rows (collapse) + 4 thinking
    // assistants (echo trim).
    let mut s = tool_history(8);
    for i in 0..4 {
        s.push_assistant(
            &AssistantMessage {
                content: format!("step-{i}"),
                tool_calls: Vec::new(),
                thinking: Some(format!("reason-{i}")),
            },
            "Stop",
        );
    }
    std::env::set_var("THINKING_KEEP", "99");
    std::env::set_var("COLLAPSE_HYSTERESIS", "5");
    s.resolve_fold_config();
    assert_eq!((s.thinking_keep, s.collapse_hysteresis), (99, 5));
    // H=5 over 8 rows: boundary 5*((8-5)/5) = 0, no stubs; keep 99:
    // every thinking row survives.
    assert_eq!(stub_boundary(&s.derived_messages()), 0);
    let before = serde_json::to_string(&s.derived_messages()).unwrap();
    assert_eq!(before.matches("reason-").count(), 4);
    // Mid-run env change: the fold must not move.
    std::env::set_var("THINKING_KEEP", "0");
    std::env::set_var("COLLAPSE_HYSTERESIS", "0");
    let after = serde_json::to_string(&s.derived_messages()).unwrap();
    assert_eq!(before, after, "mid-run env change flipped the fold");
    // A fresh head re-resolves: the new env takes effect only there.
    s.resolve_fold_config();
    assert_eq!((s.thinking_keep, s.collapse_hysteresis), (0, 0));
    let re = serde_json::to_string(&s.derived_messages()).unwrap();
    assert_ne!(re, before, "re-resolve must pick up the new env");
    assert_eq!(stub_boundary(&s.derived_messages()), 3); // H=0 collapse-5
    match saved_thinking {
        Some(v) => std::env::set_var("THINKING_KEEP", v),
        None => std::env::remove_var("THINKING_KEEP"),
    }
    match saved_hyst {
        Some(v) => std::env::set_var("COLLAPSE_HYSTERESIS", v),
        None => std::env::remove_var("COLLAPSE_HYSTERESIS"),
    }
}

/// The cached fields alone steer the fold: no env touched at all.
#[test]
fn fold_uses_cached_knobs_not_the_environment() {
    let mut s = tool_history(8);
    s.collapse_hysteresis = 5;
    assert_eq!(stub_boundary(&s.derived_messages()), 0);
    s.collapse_hysteresis = 0;
    assert_eq!(stub_boundary(&s.derived_messages()), 3);
    for i in 0..4 {
        s.push_assistant(
            &AssistantMessage {
                content: format!("step-{i}"),
                tool_calls: Vec::new(),
                thinking: Some(format!("reason-{i}")),
            },
            "Stop",
        );
    }
    let thinking = |s: &LoopState| {
        s.derived_messages()
            .iter()
            .filter(|m| m.role == "assistant")
            .filter_map(|m| m.thinking.clone())
            .collect::<Vec<_>>()
    };
    s.thinking_keep = 1;
    assert_eq!(thinking(&s), vec!["reason-3".to_string()]);
    s.thinking_keep = 99;
    assert_eq!(thinking(&s).len(), 4);
}

#[test]
fn derived_messages_keeps_thinking_on_last_two_assistants() {
    let mut s = LoopState::new();
    for i in 0..4 {
        s.push_assistant(
            &AssistantMessage {
                content: format!("step-{i}"),
                tool_calls: Vec::new(),
                thinking: Some(format!("reason-{i}")),
            },
            "Stop",
        );
    }
    let msgs = s.derived_messages();
    let thinking: Vec<Option<&str>> = msgs
        .iter()
        .filter(|m| m.role == "assistant")
        .map(|m| m.thinking.as_deref())
        .collect();
    assert_eq!(
        thinking,
        vec![None, None, Some("reason-2"), Some("reason-3")],
        "thinking survives on exactly the last 2 assistant rows"
    );
}

#[tokio::test]
async fn drive_tick_select_spine() {
    let mut s = LoopState::new();
    let (itx, mut irx) = mpsc::channel(8);
    let (_ptx, mut prx) = mpsc::channel(8);
    let (_ttx, mut trx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let mut emitter = Emitter::new();
    itx.send(Input::User("go".into())).await.unwrap();
    let verdict = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(verdict, PhaseVerdict::Continue));
    assert_eq!(s.turn, 1);
    itx.send(Input::StopHard).await.unwrap();
    let verdict = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(verdict, PhaseVerdict::Return(Outcome::Cancelled)));
}

#[tokio::test]
async fn cancel_token_drives_stop() {
    let mut s = LoopState::new();
    let (_itx, mut irx) = mpsc::channel(8);
    let (_ptx, mut prx) = mpsc::channel(8);
    let (_ttx, mut trx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let mut emitter = Emitter::new();
    cancel.cancel();
    let verdict = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(verdict, PhaseVerdict::Return(Outcome::Cancelled)));
}

#[test]
fn turn_end_reason_log_to_event_mapping() {
    use agent_event::TurnEndReason as EventReason;
    assert_eq!(
        turn_end_reason_to_event(&TurnEndReason::Completed),
        EventReason::Completed
    );
    assert_eq!(
        turn_end_reason_to_event(&TurnEndReason::Error("r".into())),
        EventReason::Error
    );
    assert_eq!(
        turn_end_reason_to_event(&TurnEndReason::Interrupted),
        EventReason::Aborted
    );
    assert_eq!(
        turn_end_reason_to_event(&TurnEndReason::Budget),
        EventReason::BudgetExceeded
    );
    assert_eq!(
        turn_end_reason_to_event(&TurnEndReason::MaxTokens),
        EventReason::MaxSteps
    );
}

#[test]
fn crash_input_queues_like_user_with_crash_source() {
    let mut s = LoopState::new();
    s.apply_input(Input::Crash("boom state".into()));
    assert_eq!((s.turn, s.phase), (1, Phase::Running));
    assert_eq!(s.admit_steering(), 1);
    match &s.items[1].kind {
        ItemKind::Input { source, text, .. } => {
            assert_eq!(*source, InputSource::Crash);
            assert_eq!(text, "boom state");
        }
        other => panic!("expected Crash Input, got {other:?}"),
    }
    // Running-phase crash also just queues.
    s.apply_input(Input::Crash("again".into()));
    assert_eq!(s.steering.len(), 1);
    assert_eq!(s.steering[0].source, InputSource::Crash);
}

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
    let outcome = s.step_claim(assistant(vec![call("a")]), StopReason::ToolUse);
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
    s.queue_directives();
    assert_eq!(s.pending_directives.len(), 1);
    assert_eq!(s.deliver_directives(), 0);
    assert_eq!(s.pending_directives.len(), 1);
    // First recorded result creates the tail: both land, once each.
    s.turn = 1;
    assert!(matches!(
        s.step_claim(assistant(vec![call("a")]), StopReason::ToolUse),
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
    s.queue_directives();
    assert_eq!(
        s.pending_directives.front().unwrap(),
        "0 edits so far after 15 actions. Stop reading. Apply your first edit with the edit tool NOW."
    );
    s.queue_directives();
    assert_eq!(s.pending_directives.len(), 1); // half-cap latch holds
    s.edits = 1; // an edit silences only the zero-edit rule
    s.budget.counters_mut().actions_this_trial = cap * 4 / 5;
    s.queue_directives();
    assert_eq!(
        s.pending_directives.back().unwrap(),
        "only 6 actions remain before the run is stopped. Finish and submit your patch now."
    );
    s.queue_directives();
    assert_eq!(s.pending_directives.len(), 2); // both one-shot
}

/// One successful tool round through the claim seam, mirroring run():
/// Dispatch registers name+args, the ok result lands, edits count here
/// (run() owns the counter in production).
fn ok_round(s: &mut LoopState, id: &str, name: &str, args: Value) {
    let outcome = s.step_claim(
        assistant(vec![ToolCallRef {
            id: id.into(),
            name: name.into(),
            args,
        }]),
        StopReason::ToolUse,
    );
    assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
    assert!(s.record_tool_result(ToolMsg {
        call_id: id.into(),
        result: result(),
    }));
    if name == "edit" || name == "write" {
        s.edits += 1;
    }
}

fn declare(s: &mut LoopState) -> ClaimOutcome {
    s.step_claim(assistant(vec![]), StopReason::Stop)
}

#[test]
fn verify_nudge_fires_on_done_with_unverified_edits() {
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    assert_eq!(s.edits, 1);
    let outcome = declare(&mut s);
    assert!(matches!(outcome, ClaimOutcome::VerifyHold));
    assert_eq!(s.verify.hold.as_deref(), Some(VERIFY_NUDGE));
    assert!(s
        .verify
        .hold
        .unwrap()
        .starts_with("Unverified declare held:"));
    assert_eq!(s.verify.nudges_used, 1);
    assert_eq!(s.verify.edits_at_last_nudge, 1);
    assert!(s.call_model); // turn held alive for the next request
    assert!(s.pending_directives.is_empty()); // request tail only: no double delivery
}

#[test]
fn verify_nudge_needs_edits_before_first_fire() {
    let mut s = LoopState::new();
    assert!(matches!(declare(&mut s), ClaimOutcome::Done));
    assert_eq!(s.verify.nudges_used, 0);
    assert!(s.verify.hold.is_none());
}

#[test]
fn verify_nudge_silent_when_test_passed_since_write() {
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    ok_round(&mut s, "t1", "test", Value::Null);
    assert!(matches!(declare(&mut s), ClaimOutcome::Done));
    assert_eq!(s.verify.nudges_used, 0);
    assert!(s.verify.hold.is_none());
}

#[test]
fn verify_nudge_exec_pytest_counts_but_plain_exec_does_not() {
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    ok_round(&mut s, "x1", "exec", serde_json::json!({"cmd": "ls"}));
    assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    ok_round(
        &mut s,
        "x1",
        "exec",
        serde_json::json!({"cmd": "pytest -q"}),
    );
    assert!(matches!(declare(&mut s), ClaimOutcome::Done));
}

#[test]
fn verify_nudge_failed_test_is_not_passing() {
    let mut s = LoopState::new();
    let outcome = s.step_claim(
        assistant(vec![ToolCallRef {
            id: "e1".into(),
            name: "edit".into(),
            args: Value::Null,
        }]),
        StopReason::ToolUse,
    );
    assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
    assert!(s.record_tool_result(ToolMsg {
        call_id: "e1".into(),
        result: result(),
    }));
    s.edits += 1;
    let outcome = s.step_claim(
        assistant(vec![ToolCallRef {
            id: "t1".into(),
            name: "test".into(),
            args: Value::Null,
        }]),
        StopReason::ToolUse,
    );
    assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
    assert!(s.record_tool_result(ToolMsg {
        call_id: "t1".into(),
        result: ToolResult {
            content: "1 failed".into(),
            is_error: true,
        },
    }));
    assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
}

#[test]
fn verify_nudge_failed_test_outcome_does_not_verify() {
    // FIX 1 shape: the tools lane reports a failing `test` run as
    // `ToolOutcome { success: false, .. }` with FAIL content; run()
    // maps it to an error ToolResult, which must not verify the write.
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    let outcome = s.step_claim(
        assistant(vec![ToolCallRef {
            id: "t1".into(),
            name: "test".into(),
            args: serde_json::json!({"cmd": "pytest -q"}),
        }]),
        StopReason::ToolUse,
    );
    assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
    let failing = outcome_to_result(ToolOutcome {
        content: "FAIL: pytest -q\n1 failed".into(),
        truncated: false,
        success: false,
    });
    assert!(failing.is_error);
    assert!(s.record_tool_result(ToolMsg {
        call_id: "t1".into(),
        result: failing,
    }));
    assert!(!s.verify.verified_since_write);
    assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
}

#[test]
fn is_verification_call_command_table() {
    // Only the command position counts: naming pytest as an argument
    // (`pip install pytest`, `grep pytest`) is not a verification run.
    let cases = [
        ("test", None, true),
        ("exec", Some("pytest"), true),
        ("exec", Some("pytest -q"), true),
        ("exec", Some(".venv/bin/pytest -q"), true),
        ("exec", Some("cargo test"), true),
        ("exec", Some("go test ./..."), true),
        ("exec", Some("npm test"), true),
        ("exec", Some("npm run test"), true),
        ("exec", Some("pip install pytest"), false),
        ("exec", Some("grep pytest"), false),
        ("exec", Some("ls"), false),
        ("exec", None, false),
        ("edit", Some("pytest"), false),
    ];
    for (name, cmd, expected) in cases {
        let args = match cmd {
            Some(c) => serde_json::json!({"cmd": c}),
            None => Value::Null,
        };
        assert_eq!(
            is_verification_call(name, &args),
            expected,
            "name={name} cmd={cmd:?}"
        );
    }
}

#[test]
fn verify_nudge_silent_on_second_declare_without_write() {
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
    // "Blocked" answer: text again, no intervening write -> Done.
    assert!(matches!(declare(&mut s), ClaimOutcome::Done));
    assert_eq!(s.verify.nudges_used, 1);
}

#[test]
fn verify_nudge_silent_in_base_and_contract() {
    for level in [IncentivesLevel::Base, IncentivesLevel::Contract] {
        let mut s = LoopState::new();
        s.incentives = level;
        ok_round(&mut s, "e1", "edit", Value::Null);
        assert!(matches!(declare(&mut s), ClaimOutcome::Done));
        assert_eq!(s.verify.nudges_used, 0);
        assert!(s.verify.hold.is_none());
    }
}

#[test]
fn verify_nudge_silent_inside_cap_tail_tenth() {
    let mut s = LoopState::new();
    let cap = s.budget.config().actions_per_trial;
    assert!(cap >= 10, "tail math needs a non-trivial cap");
    // Just below the last 10%: still fires.
    ok_round(&mut s, "e1", "edit", Value::Null);
    s.budget.counters_mut().actions_this_trial = cap * 9 / 10 - 1;
    assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
    // Inside the last 10%: silent.
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    s.budget.counters_mut().actions_this_trial = cap * 9 / 10;
    assert!(matches!(declare(&mut s), ClaimOutcome::Done));
    assert!(s.verify.hold.is_none());
}

#[test]
fn verify_nudge_caps_at_two_per_run() {
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
    ok_round(&mut s, "e2", "edit", Value::Null); // intervening write
    assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
    assert_eq!(s.verify.nudges_used, 2);
    ok_round(&mut s, "e3", "edit", Value::Null); // budget spent
    assert!(matches!(declare(&mut s), ClaimOutcome::Done));
    assert_eq!(s.verify.nudges_used, 2);
}

#[test]
fn verify_hold_rides_next_request_tail_once_and_leaves_log() {
    let root = run_tmp("verify-hold");
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    state.phase = Phase::Running;
    state.apply_input(Input::User("build it".into()));
    state.admit_steering();
    ok_round(&mut state, "e1", "edit", Value::Null);
    assert!(matches!(declare(&mut state), ClaimOutcome::VerifyHold));
    // Durable hold record for replay: the declare's Attempt row.
    let rows = state.items.len();
    assert!(state.items.iter().any(|i| matches!(
        &i.kind,
        ItemKind::Attempt { error, will_retry: true } if error == VERIFY_NUDGE
    )));
    let cfg = RunConfig::default();
    let r1 = build_request(&mut state, &registry, &root, &cfg);
    // Attribution: the hold is its own user-role row, never merged into
    // the model's assistant declare text. Budgets stay on the prior tail.
    assert!(r1.messages.len() >= 3);
    let last = r1.messages.last().unwrap();
    let prev = &r1.messages[r1.messages.len() - 2];
    assert_eq!(last.role, "user");
    assert_eq!(last.content, VERIFY_NUDGE);
    assert!(
        prev.content.contains("budgets remaining:"),
        "{}",
        prev.content
    );
    assert!(!prev.content.contains(VERIFY_NUDGE));
    // Peek, not consume: a provider Err+retry must re-arm.
    assert!(state.verify.hold.is_some());
    assert_eq!(state.items.len(), rows); // request rows never persisted
                                         // Simulate `run`'s take-on-successful-send.
    state.verify.hold.take();
    let r2 = build_request(&mut state, &registry, &root, &cfg);
    assert!(r2
        .messages
        .iter()
        .all(|m| !m.content.contains(VERIFY_NUDGE)));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn lessons_ride_the_tail_once_each() {
    let mut s = LoopState::new();
    s.push_lesson("vary the approach".into());
    s.turn = 1;
    assert!(matches!(
        s.step_claim(assistant(vec![call("a")]), StopReason::ToolUse),
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

#[test]
fn collapse_stub_preview_caps_at_120_chars() {
    let mut s = LoopState::new();
    for i in 0..6 {
        s.items.push(Item {
            seq: s.items.len() as u64,
            id: format!("t{i}"),
            parent_id: None,
            recorded_at: SystemTime::now(),
            kind: ItemKind::ToolResult {
                call_id: format!("c{i}"),
                content: format!("{}-{i}", "x".repeat(200)),
                is_error: false,
                recovery: None,
            },
        });
    }
    let msgs = s.derived_messages();
    let head = &msgs[0].content;
    let prefix = "[collapsed: 202b — re-open to edit] ";
    assert!(head.starts_with(prefix), "{head}");
    assert_eq!(head.chars().count(), prefix.chars().count() + 120);
}

#[test]
fn build_request_system_is_static_and_budgets_ride_the_tail() {
    let root = run_tmp("contract");
    std::fs::write(root.join("a.rs"), "v1\n").unwrap();
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    state.phase = Phase::Running;
    state.apply_input(Input::User("build it".into()));
    state.admit_steering();
    state.budget.counters_mut().steps = 3;
    state.budget.counters_mut().actions_this_trial = 5;
    state.budget.counters_mut().tokens = 100;
    let r1 = build_request(&mut state, &registry, &root, &RunConfig::default());
    // Mid-run edit + moved counters: neither may rotate the cached head.
    std::fs::write(root.join("b.rs"), "new\n").unwrap();
    state.budget.counters_mut().steps = 4;
    state.budget.counters_mut().actions_this_trial = 6;
    state.budget.counters_mut().tokens = 300;
    let r2 = build_request(&mut state, &registry, &root, &RunConfig::default());

    assert_eq!(r1.messages[0].role, "system");
    let sys = &r1.messages[0].content;
    assert!(sys.starts_with("--- workflow-contract"), "{sys}");
    for needle in [
        "budgets remaining are printed on the last line of the newest message; read them there.",
        "tool results older than the last 5 are collapsed to one line; re-open a file immediately before editing it.",
        "work file-by-file: view -> edit immediately -> next file.",
        "`edit` and `write` are the ONLY patch mechanisms — never write files via exec; exec/test are for checks only.",
        "when your patch is complete, reply with a text message and NO tool calls — that finishes the run.",
    ] {
        assert!(sys.contains(needle), "missing {needle:?} in {sys}");
    }
    assert!(
        !sys.contains("budgets remaining:"),
        "live digits in system: {sys}"
    );
    assert!(
        sys.contains("--- file-map") && sys.contains("a.rs"),
        "{sys}"
    );
    assert!(!sys.contains("b.rs"), "map froze at run start: {sys}");
    assert_eq!(sys, &r2.messages[0].content);
    assert_eq!(r1.messages.len(), 2); // system + the one input

    // Identical prefix up to the final (budget-carrying) message.
    for (m1, m2) in r1.messages[..r1.messages.len() - 1]
        .iter()
        .zip(&r2.messages[..r2.messages.len() - 1])
    {
        assert_eq!(format!("{m1:?}"), format!("{m2:?}"));
    }
    assert_eq!(
        r1.messages.last().unwrap().content,
        "build it\nbudgets remaining: steps 17/20; actions 25/30; tokens 49900/50000"
    );
    assert_eq!(
        r2.messages.last().unwrap().content,
        "build it\nbudgets remaining: steps 16/20; actions 24/30; tokens 49700/50000"
    );
    // Request-scoped only: the durable transcript never carries counters.
    assert!(!state
        .items
        .iter()
        .any(|i| format!("{:?}", i.kind).contains("budgets remaining")));
    let _ = std::fs::remove_dir_all(&root);
}

/// Pins follow the file-map rule: frozen at run start (byte-stable head)
/// and dropped once their file changes, so the model never sees pre-edit
/// bytes the `edit` result already superseded.
#[test]
fn pins_freeze_at_run_start_and_drop_when_edited() {
    let root = run_tmp("pins");
    std::fs::write(root.join("goal.md"), "keep me\n").unwrap();
    std::fs::write(root.join("note.md"), "v1\n").unwrap();
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    state.phase = Phase::Running;
    state.apply_input(Input::User("go".into()));
    state.admit_steering();
    let cfg = RunConfig {
        context_files: vec!["goal.md".into(), "note.md".into()],
        ..RunConfig::default()
    };
    // First request freezes the pins, as `run` does before its first call.
    let r1 = build_request(&mut state, &registry, &root, &cfg);
    assert!(r1.messages[0].content.contains("--- goal.md"));
    assert!(r1.messages[0].content.contains("v1"));

    // Unrelated write: pin bytes stay identical across requests.
    std::fs::write(root.join("other.txt"), "x\n").unwrap();
    let r2 = build_request(&mut state, &registry, &root, &cfg);
    assert_eq!(r2.messages[0].content, r1.messages[0].content);

    // Mid-run edit to one pinned file: that pin disappears, the other stays.
    std::fs::write(root.join("note.md"), "v2\n").unwrap();
    let r3 = build_request(&mut state, &registry, &root, &cfg);
    let sys = &r3.messages[0].content;
    assert!(
        sys.contains("--- goal.md") && sys.contains("keep me"),
        "{sys}"
    );
    assert!(!sys.contains("--- note.md"), "edited pin must drop: {sys}");
    assert!(
        !sys.contains("v1") && !sys.contains("v2"),
        "stale or fresh pin bytes leaked: {sys}"
    );
    let _ = std::fs::remove_dir_all(&root);
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

struct FakeEdit;

#[async_trait::async_trait]
impl tool_core::Tool for FakeEdit {
    fn definition(&self) -> tool_core::ToolDefinition {
        tool_core::ToolDefinition {
            name: "edit".into(),
            description: "fake edit".into(),
            schema: serde_json::json!({}),
        }
    }
    fn prepare(&self, call: &ToolCall) -> tool_core::CallStatus {
        tool_core::CallStatus::Dispatch(tool_core::Invocation {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            args: call.args.clone(),
        })
    }
    async fn execute(
        &self,
        inv: tool_core::Invocation,
        _cancel: CancellationToken,
    ) -> Result<tool_core::ToolOutcome, tool_core::ToolError> {
        Ok(tool_core::ToolOutcome {
            content: format!("edited {}", inv.call_id),
            truncated: false,
            success: true,
        })
    }
}

fn edit_registry() -> tool_core::Registry {
    let gate = std::sync::Arc::new(tool_core::GrantGate::new(
        [("agent".to_string(), vec!["edit".to_string()])].into(),
    ));
    let mut r = tool_core::Registry::new(gate);
    r.register(std::sync::Arc::new(FakeEdit));
    r
}

fn tool_use_message() -> AssistantMessage {
    AssistantMessage {
        content: "fixing".into(),
        tool_calls: vec![provider_core::ToolCallRef {
            id: "c1".into(),
            name: "edit".into(),
            args: serde_json::json!({"path": "f"}),
        }],
        thinking: None,
    }
}

fn usage() -> Usage {
    Usage {
        input: 100,
        output: 50,
        cache_read: 0,
        cache_write: 0,
        reasoning: None,
        cost_usd: Some(0.02),
    }
}

fn event_order(history: &[AgentEvent]) -> Vec<&'static str> {
    history
        .iter()
        .map(|e| match e {
            AgentEvent::TurnStart { .. } => "TurnStart",
            AgentEvent::MessageStart { .. } => "MessageStart",
            AgentEvent::MessageUpdate { .. } => "MessageUpdate",
            AgentEvent::MessageEnd { .. } => "MessageEnd",
            AgentEvent::ToolStart { .. } => "ToolStart",
            AgentEvent::ToolEnd { .. } => "ToolEnd",
            AgentEvent::TurnEnd { .. } => "TurnEnd",
            AgentEvent::Control(_) => "Control",
            _ => "other",
        })
        .collect()
}

#[tokio::test]
async fn e2e_full_turn_tool_call_orders_log_before_events() {
    use agent_event::check_pairing;
    let mut s = LoopState::new();
    let mut emitter = Emitter::new();
    let (itx, mut irx) = mpsc::channel(8);
    let (ptx, mut prx) = mpsc::channel(8);
    let (ttx, mut trx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let registry = edit_registry();

    itx.send(Input::User("build it".into())).await.unwrap();
    let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(v, PhaseVerdict::Continue));
    s.admit_steering(); // pre-boundary: the only place steering enters the log
    let root = CancellationToken::new();
    assert!(s.start_provider_call(&root).is_some()); // step head: may_step + record
    ptx.send(ProviderMsg::Partial {
        turn: 1,
        text: "thinking ".into(),
    })
    .await
    .unwrap();
    let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(v, PhaseVerdict::Continue));
    ptx.send(ProviderMsg::Settled {
        turn: 1,
        message: tool_use_message(),
        stop: StopReason::ToolUse,
        usage: Some(usage()),
    })
    .await
    .unwrap();
    let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(v, PhaseVerdict::Continue));
    // Pre-effect: the ToolCall row is durable before anything executes.
    let call_seq = s
        .items
        .iter()
        .position(|i| matches!(&i.kind, ItemKind::ToolCall { call_id, .. } if call_id == "c1"))
        .expect("ToolCall logged at claim");
    let prepared = registry.prepare(
        "agent",
        ToolCall {
            call_id: "c1".into(),
            name: "edit".into(),
            args: serde_json::json!({"path": "f"}),
        },
    );
    let inv = match prepared {
        tool_core::CallStatus::Dispatch(inv) => inv,
        tool_core::CallStatus::Result(r) => panic!("gate must allow the fake: {}", r.content),
    };
    let outcome = registry
        .resolve("edit")
        .unwrap()
        .execute(inv, CancellationToken::new())
        .await
        .expect("fake tool runs");
    ttx.send(ToolMsg {
        call_id: "c1".into(),
        result: ToolResult {
            content: outcome.content,
            is_error: false,
        },
    })
    .await
    .unwrap();
    let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    // Batch done, results owed back to the model: the tick parks on.
    assert!(matches!(v, PhaseVerdict::Continue));
    // A hard stop drains the parked idle and closes the turn.
    itx.send(Input::StopHard).await.unwrap();
    let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(v, PhaseVerdict::Return(Outcome::Cancelled)));

    // Events arrive ordered; every Start pairs with its End.
    let order = event_order(emitter.history());
    for pair in [
        ("TurnStart", "MessageStart"),
        ("MessageStart", "MessageEnd"),
        ("MessageEnd", "ToolStart"),
        ("ToolStart", "ToolEnd"),
        ("ToolEnd", "TurnEnd"),
    ] {
        let (a, b) = (
            order.iter().position(|k| *k == pair.0),
            order.iter().position(|k| *k == pair.1),
        );
        assert!(a < b, "{pair:?} out of order in {order:?}");
    }
    assert!(order.contains(&"Control")); // the steer ack rides the stream
    assert!(check_pairing(emitter.history()));
    // Partial + settle share one message id: a single Start, two Updates.
    assert_eq!(order.iter().filter(|k| **k == "MessageStart").count(), 1);
    assert_eq!(order.iter().filter(|k| **k == "MessageUpdate").count(), 2);
    // Log rows precede their frames, in transcript order.
    let kinds: Vec<&str> = s
        .items
        .iter()
        .map(|i| match &i.kind {
            ItemKind::TurnStart { .. } => "TurnStart",
            ItemKind::Input { .. } => "Input",
            ItemKind::Assistant { .. } => "Assistant",
            ItemKind::ToolCall { .. } => "ToolCall",
            ItemKind::ToolResult { .. } => "ToolResult",
            ItemKind::TurnEnd { .. } => "TurnEnd",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "TurnStart",
            "Input",
            "Assistant",
            "ToolCall",
            "ToolResult",
            "TurnEnd"
        ]
    );
    let result_seq = s
        .items
        .iter()
        .position(|i| matches!(&i.kind, ItemKind::ToolResult { .. }))
        .unwrap();
    assert!(call_seq < result_seq); // pre-effect, never inverted
                                    // Budget consumed from the fake Usage.
    assert_eq!(s.budget.counters().steps, 1);
    assert_eq!(s.budget.counters().tokens, 150);
    assert_eq!(s.budget.counters().spent_cents, 2);
    // F1a: the settle's usage rides MessageEnd; TurnEnd carries the totals.
    assert!(emitter.history().iter().any(|e| matches!(
        e,
        AgentEvent::MessageEnd { usage: Some(u), .. }
            if u.input_tokens == 100 && u.output_tokens == 50 && u.cost_usd == Some(0.02)
    )));
    assert!(emitter.history().iter().any(|e| matches!(
        e,
        AgentEvent::TurnEnd { usage_totals, .. }
            if usage_totals.input_tokens == 100 && usage_totals.cost_usd == Some(0.02)
    )));
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
    let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
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

// --- multi-tick run() assembly: fakes + tempdir git repo ---

use provider_core::{LlmClient, LlmError, Request, Response};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tool_core::{
    CallStatus as CoreCallStatus, GrantGate, Invocation as CoreInvocation,
    Registry as CoreRegistry, Tool as CoreTool, ToolCall as CoreToolCall,
    ToolDefinition as CoreToolDef, ToolError as CoreToolError, ToolOutcome as CoreToolOutcome,
};

static RUN_N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn run_tmp(name: &str) -> std::path::PathBuf {
    let n = RUN_N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("rof-run-{name}-{n}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct ScriptClient {
    order: Arc<Mutex<Vec<String>>>,
    queue: Mutex<VecDeque<Response>>,
    /// Every outgoing request, for shape assertions.
    requests: Arc<Mutex<Vec<Request>>>,
}

#[async_trait::async_trait]
impl LlmClient for ScriptClient {
    async fn complete(&self, _model: &str, req: &Request) -> Result<Response, LlmError> {
        self.order.lock().unwrap().push("model".into());
        self.requests.lock().unwrap().push(req.clone());
        self.queue
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(LlmError::Transport("script empty".into()))
    }
}

fn script_resp(calls: Vec<(&str, &str, serde_json::Value)>, stop: StopReason) -> Response {
    Response {
        message: AssistantMessage {
            content: "step".into(),
            tool_calls: calls
                .into_iter()
                .map(|(id, name, args)| provider_core::ToolCallRef {
                    id: id.into(),
                    name: name.into(),
                    args,
                })
                .collect(),
            thinking: None,
        },
        stop,
        usage: Usage {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: None,
            cost_usd: Some(0.01),
        },
        latency_ms: 0,
        attempts: 1,
        raw_stop_reason: None,
        retry_usage: None,
    }
}

fn text_resp(text: &str) -> Response {
    Response {
        message: AssistantMessage {
            content: text.into(),
            tool_calls: vec![],
            thinking: None,
        },
        stop: StopReason::Stop,
        usage: Usage {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: None,
            cost_usd: Some(0.01),
        },
        latency_ms: 0,
        attempts: 1,
        raw_stop_reason: None,
        retry_usage: None,
    }
}

struct WriteFile {
    root: std::path::PathBuf,
}

#[async_trait::async_trait]
impl CoreTool for WriteFile {
    fn definition(&self) -> CoreToolDef {
        CoreToolDef {
            name: "write".into(),
            description: "write a file under the temp root".into(),
            schema: serde_json::json!({
                "type": "object",
                "required": ["path", "content"],
                "additionalProperties": false,
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                }
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
        _cancel: CancellationToken,
    ) -> Result<CoreToolOutcome, CoreToolError> {
        let path = inv.args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let content = inv
            .args
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if path.is_empty() {
            return Err(CoreToolError::Failed("bad args".into()));
        }
        std::fs::write(self.root.join(path), content)
            .map_err(|e| CoreToolError::Failed(e.to_string()))?;
        Ok(CoreToolOutcome {
            content: format!("wrote {path}"),
            truncated: false,
            success: true,
        })
    }
}

struct Boom;

#[async_trait::async_trait]
impl CoreTool for Boom {
    fn definition(&self) -> CoreToolDef {
        CoreToolDef {
            name: "boom".into(),
            description: "always fails".into(),
            schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
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
        _inv: CoreInvocation,
        _cancel: CancellationToken,
    ) -> Result<CoreToolOutcome, CoreToolError> {
        Err(CoreToolError::Failed("boom went off".into()))
    }
}

struct FakeRead;

#[async_trait::async_trait]
impl CoreTool for FakeRead {
    fn definition(&self) -> CoreToolDef {
        CoreToolDef {
            name: "read".into(),
            description: "read-only probe with a fresh observation each call".into(),
            schema: serde_json::json!({
                "type": "object",
                "required": ["n"],
                "additionalProperties": false,
                "properties": {"n": {"type": "integer"}}
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
        _cancel: CancellationToken,
    ) -> Result<CoreToolOutcome, CoreToolError> {
        let n = inv.args.get("n").and_then(|v| v.as_i64()).unwrap_or(-1);
        Ok(CoreToolOutcome {
            content: format!("read {n}"),
            truncated: false,
            success: true,
        })
    }
}

fn run_registry(root: &std::path::Path) -> CoreRegistry {
    let mut r = CoreRegistry::new(Arc::new(GrantGate::new(
        [(
            "agent".to_string(),
            vec!["write".to_string(), "boom".to_string(), "read".to_string()],
        )]
        .into(),
    )));
    r.register(Arc::new(WriteFile {
        root: root.to_path_buf(),
    }));
    r.register(Arc::new(Boom));
    r.register(Arc::new(FakeRead));
    r
}

fn run_kinds(items: &[Item]) -> Vec<&'static str> {
    items
        .iter()
        .map(|i| match &i.kind {
            ItemKind::Header { .. } => "Header",
            ItemKind::TurnStart { .. } => "TurnStart",
            ItemKind::Input { .. } => "Input",
            ItemKind::Assistant { .. } => "Assistant",
            ItemKind::Attempt { .. } => "Attempt",
            ItemKind::ToolCall { .. } => "ToolCall",
            ItemKind::ToolResult { .. } => "ToolResult",
            ItemKind::TurnEnd { .. } => "TurnEnd",
        })
        .collect()
}

struct RecBets {
    order: Arc<Mutex<Vec<String>>>,
}

impl BetsHook for RecBets {
    fn on_step(&self) -> PhaseVerdict {
        self.order.lock().unwrap().push("bets".into());
        PhaseVerdict::Continue
    }
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
    let client = ScriptClient {
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
    let mut settled = text_resp("done");
    settled.usage.reasoning = Some(3);
    let client = ScriptClient {
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
    let mut queue: VecDeque<Response> = VecDeque::new();
    for i in 1..=15 {
        let id = format!("r{i}");
        queue.push_back(script_resp(
            vec![(id.as_str(), "read", serde_json::json!({"n": i}))],
            StopReason::ToolUse,
        ));
    }
    queue.push_back(text_resp("done"));
    let client = ScriptClient {
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
    let client = ScriptClient {
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
    let client = ScriptClient {
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

#[tokio::test]
async fn run_halt_budget_steps_after_grace_with_budget_frame() {
    assert_eq!(RunConfig::default().drain_timeout, Duration::from_secs(30));
    assert_eq!(LoopState::new().drain_timeout, Duration::from_secs(30));
    let root = run_tmp("halt");
    let order = Arc::new(Mutex::new(Vec::new()));
    let client = ScriptClient {
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
    assert_eq!(state.drain_timeout, Duration::from_secs(5));
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
    let client = ScriptClient {
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
    let client = ScriptClient {
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
    let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(v, PhaseVerdict::Return(Outcome::Halted(ref s)) if s == "steps"));
    match emitter.history().last().unwrap() {
        AgentEvent::TurnEnd { reason, .. } => {
            assert_eq!(*reason, agent_event::TurnEndReason::BudgetExceeded)
        }
        other => panic!("expected TurnEnd, got {other:?}"),
    }
}

#[tokio::test]
async fn run_request_shape_history_delivered_once_collapse_5() {
    use agent_event::check_pairing;
    let root = run_tmp("shape");
    let order = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(Mutex::new(Vec::new()));
    // 7 write rounds, then a finishing text: the 8th request carries 7 tool
    // results, so collapse-5 has older material to shrink.
    let mut queue: VecDeque<Response> = VecDeque::new();
    for i in 1..=7 {
        let id = format!("c{i}");
        let path = format!("f{i}.txt");
        // Unique marker: the generic "step" would substring-match the
        // "steps N/M" budget tail on every request.
        let mut resp = script_resp(
            vec![(
                id.as_str(),
                "write",
                serde_json::json!({"path": path, "content": "x\n"}),
            )],
            StopReason::ToolUse,
        );
        resp.message.content = format!("script-step-{i}");
        resp.message.thinking = Some(format!("reason-{i}"));
        queue.push_back(resp);
    }
    queue.push_back(text_resp("all done"));
    // Held-declare round trip: the first done is held (unverified
    // writes), the second lands Done.
    queue.push_back(text_resp("done again"));
    let client = ScriptClient {
        order,
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
            cfg: RunConfig::default(),
        },
        vec![Input::User("shape-pin-goal".into())],
        &cancel,
    )
    .await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 9); // 7 tool rounds + held declare + final text
    for (i, req) in reqs.iter().enumerate() {
        assert_eq!(req.messages[0].role, "system");
        let wire = serde_json::to_string(req).unwrap();
        // Unique input appears exactly once in the whole wire request:
        // a fitted system copy would make it twice.
        assert_eq!(
            wire.matches("shape-pin-goal").count(),
            1,
            "history duplicated in request {i}"
        );
        // No history message is echoed into the system string.
        let sys = &req.messages[0].content;
        for m in &req.messages[1..] {
            assert!(
                m.content.is_empty() || !sys.contains(&m.content),
                "request {i}: {:?} also fitted into system",
                m.content
            );
        }
    }
    // Collapse-5 rides the raw path: oldest 2 of 7 tool results arrive
    // collapsed, each result delivered exactly once overall.
    let wire_last = serde_json::to_string(&reqs[7]).unwrap();
    for n in 1..=7 {
        assert_eq!(
            wire_last.matches(&format!("wrote f{n}.txt")).count(),
            1,
            "tool result f{n} must appear exactly once"
        );
    }
    let content_of = |id: &str| {
        reqs[7]
            .messages
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("missing tool message {id}"))
            .content
            .clone()
    };
    assert_eq!(
        content_of("c1"),
        "[collapsed: 12b — re-open to edit] wrote f1.txt"
    );
    assert_eq!(
        content_of("c2"),
        "[collapsed: 12b — re-open to edit] wrote f2.txt"
    );
    assert_eq!(content_of("c3"), "wrote f3.txt");
    // c7 is the request's final message: the request-scoped budget line
    // rides its tail, so only the durable part matches exactly.
    assert!(
        content_of("c7").starts_with("wrote f7.txt\nbudgets remaining:"),
        "{:?}",
        content_of("c7")
    );
    // Collapse-5 collapses tool observations only; assistant reasoning
    // follows the keep-last-2 policy, so only steps 6-7 still carry it
    // into the 8th request.
    for i in 1..=7 {
        let want = format!("reason-{i}");
        let step = format!("script-step-{i}");
        let got = reqs[7]
            .messages
            .iter()
            .find(|m| m.role == "assistant" && m.content == step)
            .and_then(|m| m.thinking.as_deref());
        let expect = if i >= 6 { Some(want.as_str()) } else { None };
        assert_eq!(got, expect, "reasoning for step {i}");
    }
    assert!(check_pairing(emitter.history()));
    let _ = std::fs::remove_dir_all(&root);
}

// --- compaction checkpoint (default off) ---

use agent_budget::BudgetConfig;

fn compaction(frac: f64, keep_tokens: usize) -> context::CompactionConfig {
    context::CompactionConfig {
        enabled: true,
        frac,
        keep_tokens,
    }
}

/// A scripted summary response: `input` is what the totals read.
fn summary_resp(text: &str, stop: StopReason, input: u64) -> Response {
    let mut r = text_resp(text);
    r.stop = stop;
    r.usage.input = input;
    r.usage.cost_usd = Some(0.02);
    r
}

/// One scripted run; hands back everything the checkpoint assertions need.
/// `followups` open later turns (a checkpoint is per turn).
async fn run_script(
    name: &str,
    queue: Vec<Response>,
    cfg: RunConfig,
    budget_tokens: u64,
    followups: Vec<&str>,
    goal: &str,
) -> (Outcome, Arc<Mutex<Vec<Request>>>, LoopState, Emitter) {
    let root = run_tmp(name);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let client = ScriptClient {
        order: Arc::new(Mutex::new(Vec::new())),
        requests: requests.clone(),
        queue: Mutex::new(VecDeque::from(queue)),
    };
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    state.budget = BudgetGuard::new(
        BudgetConfig {
            max_tokens: budget_tokens,
            ..config_for(Capability::UnattendedBatch)
        },
        Instant::now(),
    );
    for f in followups {
        state.followups.push_back(f.into());
    }
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
            cfg,
        },
        vec![Input::User(goal.into())],
        &cancel,
    )
    .await;
    let _ = std::fs::remove_dir_all(&root);
    (outcome, requests, state, emitter)
}

fn last_totals(emitter: &Emitter) -> UsageReport {
    emitter
        .history()
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::TurnEnd { usage_totals, .. } => Some(usage_totals.clone()),
            _ => None,
        })
        .expect("a TurnEnd frame")
}

/// One write round billed `input` prompt tokens: the estimate (anchor +
/// tail chars/4) crosses `budget_tokens * frac` at the next step head.
fn write_round(calls: Vec<(&str, &str, serde_json::Value)>, content: &str, input: u64) -> Response {
    let mut r = script_resp(calls, StopReason::ToolUse);
    r.message.content = content.into();
    r.usage.input = input;
    r
}

#[tokio::test]
async fn run_checkpoint_fires_once_keeps_the_tail_and_meters_the_summary() {
    use agent_event::check_pairing;
    // Round 1 is below the trigger (100 + tail), round 2 crosses it (1000 +
    // tail > 2000 * 0.3): the checkpoint summarizes round 1 and keeps
    // round 2 verbatim.
    let queue = vec![
        write_round(
            vec![(
                "c1",
                "write",
                serde_json::json!({"path": "f1.txt", "content": "x\n"}),
            )],
            "write-f1",
            100,
        ),
        write_round(
            vec![(
                "c2",
                "write",
                serde_json::json!({"path": "f2.txt", "content": "y\n"}),
            )],
            "write-f2",
            1_000,
        ),
        summary_resp("## Goal\nfinish the task", StopReason::Stop, 100),
        text_resp("all done"),
        // Held-declare round trip: the first done is held (unverified
        // writes), the second lands Done.
        text_resp("done again"),
    ];
    let cfg = RunConfig {
        compaction: compaction(0.3, 15),
        ..RunConfig::default()
    };
    let (outcome, requests, state, emitter) =
        run_script("compact-on", queue, cfg, 2_000, Vec::new(), "goal-1").await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    let reqs = requests.lock().unwrap();
    assert_eq!(
        reqs.len(),
        5,
        "two agent calls + summary + held + finishing call"
    );
    // The summary call happens exactly once: no tools, summarizer system
    // row, the older round inside the payload, the kept tail outside it.
    let summary_calls: Vec<&Request> = reqs.iter().filter(|r| r.tools.is_empty()).collect();
    assert_eq!(summary_calls.len(), 1);
    let sreq = summary_calls[0];
    assert_eq!(sreq.messages[0].content, context::SUMMARY_SYSTEM);
    let payload = &sreq.messages[1].content;
    assert!(
        payload.starts_with("<conversation>\n[user]: goal-1"),
        "{payload}"
    );
    assert!(payload.contains("write(") && payload.contains("f1.txt"));
    assert!(payload.contains("[tool]: wrote f1.txt"));
    assert!(
        !payload.contains("wrote f2.txt"),
        "kept tail is not summarized"
    );
    assert!(payload.ends_with(context::SUMMARY_PROMPT));
    // Post-checkpoint request: one summary row replaces the old prefix,
    // the newest assistant call + its result ride verbatim.
    let post = &reqs[3];
    assert!(post.messages[1].content.starts_with(CHECKPOINT_PREFIX));
    assert!(post.messages[1]
        .content
        .contains("## Goal\nfinish the task"));
    let wire = serde_json::to_string(post).unwrap();
    assert!(
        !wire.contains("goal-1"),
        "the summarized prefix was dropped"
    );
    assert!(!wire.contains("wrote f1.txt"), "replaced, not duplicated");
    assert_eq!(post.messages[2].role, "assistant");
    assert_eq!(post.messages[2].tool_calls[0].id, "c2");
    assert_eq!(post.messages[3].tool_call_id.as_deref(), Some("c2"));
    assert!(post.messages[3].content.starts_with("wrote f2.txt"));
    // Real spend, priced like any other request: run totals (and so every
    // TurnEnd frame) carry the summary call.
    let totals = last_totals(&emitter);
    assert_eq!(totals.input_tokens, 100 + 1_000 + 100 + 10 + 10);
    assert_eq!(totals.cost_usd, Some(0.01 + 0.01 + 0.02 + 0.01 + 0.01));
    assert!(state.checkpoint.is_some());
    assert_eq!(state.compacted_turn, Some(1));
    assert!(check_pairing(emitter.history()));
}

#[tokio::test]
async fn run_checkpoint_cut_never_splits_a_call_from_its_results() {
    // Two results in one batch, each 3 estimated tokens: with keep_tokens
    // 3 the crossing lands ON the newest tool result, so the cut has to
    // back up to the assistant call that produced the group.
    let queue = vec![
        write_round(
            vec![
                (
                    "c1",
                    "write",
                    serde_json::json!({"path": "f1.txt", "content": "x\n"}),
                ),
                (
                    "c2",
                    "write",
                    serde_json::json!({"path": "f2.txt", "content": "y\n"}),
                ),
            ],
            "batch",
            1_000,
        ),
        summary_resp("## Goal\nfinish the task", StopReason::Stop, 100),
        text_resp("all done"),
        // Held-declare round trip: the first done is held (unverified
        // writes), the second lands Done.
        text_resp("done again"),
    ];
    let cfg = RunConfig {
        compaction: compaction(0.5, 3),
        ..RunConfig::default()
    };
    let (outcome, requests, state, _emitter) =
        run_script("compact-cut", queue, cfg, 1_200, Vec::new(), "goal-1").await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    assert!(state.checkpoint.is_some());
    let reqs = requests.lock().unwrap();
    let post = &reqs[2];
    // [system, summary, assistant(c1,c2), tool c1, tool c2]
    assert_eq!(post.messages[2].role, "assistant");
    assert_eq!(post.messages[2].tool_calls.len(), 2);
    assert_eq!(post.messages[3].tool_call_id.as_deref(), Some("c1"));
    assert_eq!(post.messages[4].tool_call_id.as_deref(), Some("c2"));
    for (i, m) in post.messages.iter().enumerate() {
        if m.role == "tool" {
            let id = m.tool_call_id.as_deref().unwrap();
            assert!(
                post.messages[..i]
                    .iter()
                    .any(|p| p.tool_calls.iter().any(|c| c.id == id)),
                "tool result {id} kept without its call"
            );
        }
    }
}

#[tokio::test]
async fn run_checkpoint_refuses_a_length_stopped_summary_and_keeps_the_window() {
    use agent_event::check_pairing;
    let queue = vec![
        write_round(
            vec![(
                "c1",
                "write",
                serde_json::json!({"path": "f1.txt", "content": "x\n"}),
            )],
            "write-f1",
            1_000,
        ),
        summary_resp("## Goal\npartial", StopReason::MaxTokens, 200),
        text_resp("all done"),
        // Held-declare round trip: the first done is held (unverified
        // write), the second lands Done.
        text_resp("done again"),
    ];
    let cfg = RunConfig {
        compaction: compaction(0.2, 15),
        ..RunConfig::default()
    };
    let (outcome, requests, state, emitter) =
        run_script("compact-refuse", queue, cfg, 1_500, Vec::new(), "goal-1").await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 4);
    assert_eq!(
        reqs.iter().filter(|r| r.tools.is_empty()).count(),
        1,
        "a refused summary is not retried every step"
    );
    // Window unchanged: the original prefix, no checkpoint row.
    let wire = serde_json::to_string(&reqs[2]).unwrap();
    assert!(wire.contains("goal-1"));
    assert!(!wire.contains("## Goal\npartial"));
    assert!(state.checkpoint.is_none());
    // Refused, but still billed: the spend is in the totals and the
    // refusal is visible in the event stream.
    assert_eq!(last_totals(&emitter).input_tokens, 1_000 + 200 + 10 + 10);
    assert!(emitter.history().iter().any(|e| matches!(
        e,
        AgentEvent::Error { error } if error.code == "compaction-refused"
    )));
    assert!(check_pairing(emitter.history()));
}

#[tokio::test]
async fn run_checkpoint_never_twice_without_a_new_turn() {
    // Round 1 triggers the checkpoint. Round 2's settle re-anchors the
    // estimate above the threshold again, still inside turn 1: the latch
    // is the only thing stopping a second summary call.
    let queue = vec![
        write_round(
            vec![(
                "c1",
                "write",
                serde_json::json!({"path": "f1.txt", "content": "x\n"}),
            )],
            "write-f1",
            1_000,
        ),
        summary_resp("## Goal\nfinish the task", StopReason::Stop, 100),
        write_round(
            vec![(
                "c2",
                "write",
                serde_json::json!({"path": "f2.txt", "content": "y\n"}),
            )],
            "write-f2",
            1_000,
        ),
        text_resp("all done"),
        // Held-declare round trip: the first done is held (unverified
        // writes), the second lands Done.
        text_resp("done again"),
    ];
    let cfg = RunConfig {
        compaction: compaction(0.2, 15),
        ..RunConfig::default()
    };
    let (outcome, requests, state, _emitter) =
        run_script("compact-latch", queue, cfg, 3_000, Vec::new(), "goal-1").await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    let reqs = requests.lock().unwrap();
    assert_eq!(
        reqs.len(),
        5,
        "two agent calls + summary + held + finishing call"
    );
    assert_eq!(
        reqs.iter().filter(|r| r.tools.is_empty()).count(),
        1,
        "one checkpoint per turn"
    );
    assert_eq!(state.compacted_turn, Some(1));
    let post = &reqs[3];
    assert!(post.messages[1].content.starts_with(CHECKPOINT_PREFIX));
    assert_eq!(
        serde_json::to_string(post)
            .unwrap()
            .matches(CHECKPOINT_PREFIX)
            .count(),
        1,
        "one checkpoint row, not a chain"
    );
}

#[tokio::test]
async fn run_checkpoint_composes_on_a_new_turn() {
    use agent_event::check_pairing;
    // Turn 1 checkpoints rounds 1-2 (keep_from 3). The followup opens turn
    // 2, where a second checkpoint absorbs the first summary plus round 3:
    // `keep_from` must map the folded cut back onto raw history.
    let mut turn_one_done = text_resp("turn one done");
    turn_one_done.usage.input = 1_000; // arms turn 2's estimate
                                       // Held-declare round trip: turn one done is held (unverified
                                       // writes); the repeat lands Done. input 1000 re-arms turn 2's
                                       // checkpoint estimate exactly like the held response did; the short
                                       // text keeps the keep_tokens cut on the same row.
    let mut turn_one_again = text_resp("done again");
    turn_one_again.usage.input = 1_000;
    let queue = vec![
        write_round(
            vec![(
                "c1",
                "write",
                serde_json::json!({"path": "f1.txt", "content": "x\n"}),
            )],
            "write-f1",
            100,
        ),
        write_round(
            vec![(
                "c2",
                "write",
                serde_json::json!({"path": "f2.txt", "content": "y\n"}),
            )],
            "write-f2",
            1_000,
        ),
        summary_resp("## Goal\ncheckpoint one", StopReason::Stop, 100),
        write_round(
            vec![(
                "c3",
                "write",
                serde_json::json!({"path": "f3.txt", "content": "z\n"}),
            )],
            "write-f3",
            1_000,
        ),
        turn_one_done,
        turn_one_again,
        summary_resp("## Goal\ncheckpoint two", StopReason::Stop, 100),
        text_resp("turn two done"),
    ];
    let cfg = RunConfig {
        compaction: compaction(0.2, 15),
        ..RunConfig::default()
    };
    let (outcome, requests, state, emitter) = run_script(
        "compact-turn2",
        queue,
        cfg,
        5_000,
        vec!["keep going"],
        "goal-1",
    )
    .await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    let reqs = requests.lock().unwrap();
    assert_eq!(
        reqs.len(),
        8,
        "two checkpoints + five agent calls + one finish"
    );
    assert_eq!(reqs.iter().filter(|r| r.tools.is_empty()).count(), 2);
    // The second summary is an update: the first summary is inside its
    // payload, not lost.
    let payload2 = &reqs[6].messages[1].content;
    assert!(payload2.contains("checkpoint one"), "{payload2}");
    assert!(payload2.contains("[tool]: wrote f2.txt"));
    // Final request: [system, summary2, round 3, turn-1 finishes, followup].
    let post = &reqs[7];
    assert_eq!(post.messages.len(), 7);
    assert!(post.messages[1].content.contains("checkpoint two"));
    assert_eq!(post.messages[2].role, "assistant");
    assert_eq!(post.messages[3].tool_call_id.as_deref(), Some("c3"));
    assert_eq!(post.messages[4].content, "turn one done");
    assert_eq!(post.messages[5].content, "done again");
    assert!(post.messages[6].content.starts_with("keep going"));
    let wire = serde_json::to_string(post).unwrap();
    assert!(!wire.contains("goal-1"));
    assert!(!wire.contains("wrote f1.txt") && !wire.contains("wrote f2.txt"));
    assert!(wire.contains("wrote f3.txt"), "round 3 stayed verbatim");
    assert!(
        !wire.contains("checkpoint one"),
        "absorbed by checkpoint two"
    );
    assert_eq!(state.checkpoint.as_ref().unwrap().keep_from, 5);
    assert_eq!(state.compacted_turn, Some(2));
    assert!(check_pairing(emitter.history()));
}

#[tokio::test]
async fn run_compaction_off_is_byte_identical_to_collapse_5() {
    use agent_event::check_pairing;
    // Trigger-ready: budget 8000, frac 0.1 (threshold 800), keep_tokens 3
    // and every round billed 1000 prompt tokens — every knob but `enabled`
    // is set to fire. Default `enabled: false` is the only thing holding.
    let mut queue: VecDeque<Response> = VecDeque::new();
    for i in 1..=7 {
        let id = format!("c{i}");
        let path = format!("f{i}.txt");
        queue.push_back(write_round(
            vec![(
                id.as_str(),
                "write",
                serde_json::json!({"path": path, "content": "x\n"}),
            )],
            &format!("script-step-{i}"),
            1_000,
        ));
    }
    queue.push_back(text_resp("all done"));
    // Held-declare round trip: the first done is held (unverified
    // writes), the second lands Done.
    queue.push_back(text_resp("done again"));
    let cfg = RunConfig {
        compaction: context::CompactionConfig {
            frac: 0.1,
            keep_tokens: 3,
            ..context::CompactionConfig::default()
        },
        ..RunConfig::default()
    };
    let (outcome, requests, state, emitter) = run_script(
        "compact-off",
        queue.into(),
        cfg,
        8_000,
        Vec::new(),
        "goal-1",
    )
    .await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 9, "no summary call on the off path");
    assert!(reqs.iter().all(|r| !r.tools.is_empty()));
    assert!(state.checkpoint.is_none() && state.compacted_turn.is_none());
    assert!(state.anchor.is_some(), "the trigger was armed and held");
    // Collapse-5 bytes, exactly as the disabled path always produced them.
    let content_of = |id: &str| {
        reqs[7]
            .messages
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("missing tool message {id}"))
            .content
            .clone()
    };
    assert_eq!(
        content_of("c1"),
        "[collapsed: 12b — re-open to edit] wrote f1.txt"
    );
    assert_eq!(
        content_of("c2"),
        "[collapsed: 12b — re-open to edit] wrote f2.txt"
    );
    assert_eq!(content_of("c3"), "wrote f3.txt");
    assert!(!serde_json::to_string(&reqs[7])
        .unwrap()
        .contains(CHECKPOINT_PREFIX));
    assert!(check_pairing(emitter.history()));
}

/// Prefix-cache stability end to end: the system head is byte-identical
/// across requests and frozen at run start, and the only per-request bytes
/// are the live budgets on the final message.
#[tokio::test]
async fn run_system_prefix_is_static_and_file_map_frozen() {
    let root = run_tmp("prefix");
    std::fs::write(root.join("a.rs"), "v1\n").unwrap();
    let client = ScriptClient {
        order: Arc::new(Mutex::new(Vec::new())),
        requests: Arc::new(Mutex::new(Vec::new())),
        queue: Mutex::new(VecDeque::from([
            script_resp(
                vec![(
                    "w1",
                    "write",
                    serde_json::json!({"path": "b.rs", "content": "new\n"}),
                )],
                StopReason::ToolUse,
            ),
            script_resp(
                vec![("r1", "read", serde_json::json!({"n": 1}))],
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
    assert!(root.join("b.rs").exists(), "mid-run write happened");
    let reqs = client.requests.lock().unwrap();
    assert_eq!(reqs.len(), 4);
    // File map frozen at run start: b.rs was written mid-run and never
    // shows up; the head bytes match across all requests.
    let sys = &reqs[0].messages[0].content;
    assert!(sys.contains("a.rs") && !sys.contains("b.rs"), "{sys}");
    assert_eq!(sys, &reqs[1].messages[0].content);
    assert_eq!(sys, &reqs[2].messages[0].content);
    assert_eq!(sys, &reqs[3].messages[0].content); // Identical prefix up to each request's final message, the only one
                                                   // the budget line touches.
    for pair in reqs.windows(2) {
        let head = &pair[0].messages[..pair[0].messages.len() - 1];
        assert!(pair[1].messages.len() > head.len(), "request lost history");
        for (m1, m2) in head.iter().zip(&pair[1].messages) {
            assert_eq!(format!("{m1:?}"), format!("{m2:?}"), "prefix rotated");
        }
    }
    // Fresh counters on the tail of every request: one step per call, one
    // action per executed tool, 15 tokens metered per settle. The fourth
    // request carries the held nudge as its own user-role row past the
    // budgets (never merged into the assistant declare text).
    for (req, tail) in reqs.iter().take(3).zip([
        "budgets remaining: steps 19/20; actions 30/30; tokens 50000/50000",
        "budgets remaining: steps 18/20; actions 29/30; tokens 49985/50000",
        "budgets remaining: steps 17/20; actions 28/30; tokens 49970/50000",
    ]) {
        let last = &req.messages.last().unwrap().content;
        assert!(last.contains(tail), "{last:?} must carry {tail:?}");
        assert!(
            last.ends_with(tail),
            "{last:?} must end with the budget tail"
        );
        assert!(!last.contains(VERIFY_NUDGE));
    }
    let hold_req = &reqs[3];
    let tail = "budgets remaining: steps 16/20; actions 28/30; tokens 49955/50000";
    let last = hold_req.messages.last().unwrap();
    let prev = &hold_req.messages[hold_req.messages.len() - 2];
    assert_eq!(last.role, "user");
    assert_eq!(last.content, VERIFY_NUDGE);
    assert!(
        prev.content.contains(tail),
        "{} must carry {tail:?}",
        prev.content
    );
    assert!(prev.content.ends_with(tail));
    let _ = std::fs::remove_dir_all(&root);
}

/// DeepSeek thinking mode: turn 2's request must echo turn 1's assistant
/// `thinking`, or the API 400s mid-run (measured, api.deepseek.com).
#[tokio::test]
async fn run_multi_turn_request_echoes_assistant_thinking() {
    let root = run_tmp("think");
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut first = script_resp(
        vec![(
            "c1",
            "write",
            serde_json::json!({"path": "a.txt", "content": "bye\n"}),
        )],
        StopReason::ToolUse,
    );
    first.message.thinking = Some("must edit a.txt".into());
    let client = ScriptClient {
        order: Arc::new(Mutex::new(Vec::new())),
        requests: requests.clone(),
        queue: Mutex::new(VecDeque::from([
            first,
            text_resp("done"),
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
    // The stored assistant row keeps the thinking for replay.
    let stored = state
        .items
        .iter()
        .find_map(|i| match &i.kind {
            ItemKind::Assistant { message, .. } => Some(message),
            _ => None,
        })
        .expect("assistant row stored");
    assert_eq!(stored["thinking"], "must edit a.txt");
    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 3);
    // Turn 1 has no assistant history: nothing to echo there.
    assert!(reqs[0].messages.iter().all(|m| m.thinking.is_none()));
    let echoed = reqs[1]
        .messages
        .iter()
        .find(|m| m.role == "assistant")
        .expect("assistant history in turn 2");
    assert_eq!(echoed.thinking.as_deref(), Some("must edit a.txt"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn run_provider_failure_emits_provider_failed_then_fails() {
    use agent_event::check_pairing;
    let root = run_tmp("provfail");
    // Empty script: every complete() call returns Err(Transport(...)).
    let client = ScriptClient {
        order: Arc::new(Mutex::new(Vec::new())),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::new()),
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
        vec![Input::User("go".into())],
        &cancel,
    )
    .await;
    // Production outcome path: retries exhaust, the gate closes, the
    // fatal error surfaces as Outcome::Failed.
    match &outcome {
        Outcome::Failed(msg) => assert!(msg.contains("script empty"), "got {msg}"),
        other => panic!("expected Failed, got {other:?}"),
    }
    let history = emitter.history();
    let errors = history
        .iter()
        .filter(|e| matches!(e, AgentEvent::Error { error } if error.code == "provider-failed"))
        .count();
    assert_eq!(errors, 3, "1 initial + 2 in-step retries: {history:?}");
    // Durable attempt trail: will_retry, will_retry, exhausted.
    let attempts: Vec<bool> = state
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::Attempt { will_retry, .. } => Some(*will_retry),
            _ => None,
        })
        .collect();
    assert_eq!(attempts, vec![true, true, false]);
    assert!(check_pairing(history));
    match history.last().unwrap() {
        AgentEvent::RunEnd {
            outcome: agent_event::RunOutcome::Failed(msg),
            ..
        } => assert!(msg.contains("script empty"), "got {msg}"),
        other => panic!("expected failed RunEnd, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// First call fails with a metered truncation, then succeeds: F1b through
/// the real `run()` path (extraction + budget + retry).
struct MeteredFailThenOk {
    calls: Mutex<u32>,
    usage: Usage,
}

#[async_trait::async_trait]
impl LlmClient for MeteredFailThenOk {
    async fn complete(&self, _model: &str, _req: &Request) -> Result<Response, LlmError> {
        let mut n = self.calls.lock().unwrap();
        *n += 1;
        if *n == 1 {
            return Err(LlmError::Metered {
                source: Box::new(LlmError::Transport("output truncated at 64 tokens".into())),
                usage: Some(self.usage.clone()),
                exhausted: false,
            });
        }
        drop(n);
        Ok(text_resp("done"))
    }
}

#[tokio::test]
async fn run_meters_failed_attempt_usage_then_retries() {
    let root = run_tmp("meteredfail");
    let client = MeteredFailThenOk {
        calls: Mutex::new(0),
        usage: Usage {
            input: 100,
            output: 20,
            cache_read: 0,
            cache_write: 0,
            reasoning: None,
            cost_usd: Some(0.02),
        },
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
    // Failed attempt (120) + settled attempt (15) both metered.
    assert_eq!(state.budget.counters().tokens, 135);
    assert_eq!(state.usage_totals.input_tokens, 110);
    assert_eq!(state.usage_totals.output_tokens, 25);
    assert_eq!(state.usage_totals.cost_usd, Some(0.03));
    let attempts: Vec<bool> = state
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::Attempt { will_retry, .. } => Some(*will_retry),
            _ => None,
        })
        .collect();
    assert_eq!(attempts, vec![true], "one failed attempt, retried");
    let errors = emitter
        .history()
        .iter()
        .filter(|e| matches!(e, AgentEvent::Error { error } if error.code == "provider-failed"))
        .count();
    assert_eq!(errors, 1);
    let _ = std::fs::remove_dir_all(&root);
}

// --- bets site 2: proof-gated post-batch verdicts ---

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
    state.incentives = IncentivesLevel::Base;
    state.push_directive("go".into());
    assert!(state.pending_directives.is_empty(), "Base drops directives");
    // Contract: contract on, directives still off.
    let cfg = RunConfig {
        incentives: IncentivesLevel::Contract,
        ..RunConfig::default()
    };
    let r = build_request(&mut state, &registry, &root, &cfg);
    assert!(r.messages[0].content.contains("WORKFLOW CONTRACT"));
    state.incentives = IncentivesLevel::Contract;
    state.push_directive("go".into());
    assert!(
        state.pending_directives.is_empty(),
        "Contract drops directives"
    );
    // Full (default = current behavior): both live.
    state.incentives = IncentivesLevel::Full;
    state.push_directive("go".into());
    assert_eq!(state.pending_directives.len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

/// Bet A gate (mirror of rof's `Gate`): keep the proven leading prefix.
struct GateBatch;

impl BetsHook for GateBatch {
    fn on_post_batch(&self, claim: &bets::Claim, hunks: &[(String, bool)]) -> bets::CommitVerdict {
        bets::gate_batch_commit(claim, hunks)
    }
}

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
    let client = ScriptClient {
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
    assert_eq!(state.ablation.proven_hunks, 1);
    assert_eq!(state.ablation.rollbacks, 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn empty_hunk_batches_never_inflate_proven_hunks() {
    let root = run_tmp("nohunks");
    std::fs::write(root.join("a.rs"), "one\n").unwrap();
    let client = ScriptClient {
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
        state.ablation.proven_hunks, 0,
        "read-only batch: nothing proven, nothing counted"
    );
    assert_eq!(state.ablation.rollbacks, 0);
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
    let client = ScriptClient {
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
    let client = ScriptClient {
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

// --- shared-scenario drift guard: both drivers, one script, one ordering ---

/// Frame vocabulary in emission order (driver chrome filtered out).
fn frame_order(history: &[AgentEvent]) -> Vec<&'static str> {
    history
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TurnStart { .. } => Some("TurnStart"),
            AgentEvent::MessageStart { .. } => Some("MessageStart"),
            AgentEvent::MessageUpdate { .. } => Some("MessageUpdate"),
            AgentEvent::MessageEnd { .. } => Some("MessageEnd"),
            AgentEvent::ToolStart { .. } => Some("ToolStart"),
            AgentEvent::ToolEnd { .. } => Some("ToolEnd"),
            AgentEvent::TurnEnd { .. } => Some("TurnEnd"),
            _ => None,
        })
        .collect()
}

/// The frame order the durable rows demand: every terminal frame follows
/// the append of its fact.
fn project_durable(rows: &[&'static str]) -> Vec<&'static str> {
    rows.iter()
        .flat_map(|k| match *k {
            "TurnStart" => vec!["TurnStart"],
            "Assistant" => vec!["MessageStart", "MessageUpdate", "MessageEnd"],
            "ToolCall" => vec!["ToolStart"],
            "ToolResult" => vec!["ToolEnd"],
            "TurnEnd" => vec!["TurnEnd"],
            _ => vec![],
        })
        .collect()
}

#[tokio::test]
async fn shared_scenario_both_drivers_agree_on_durable_before_emit_order() {
    use agent_event::check_pairing;

    // The shared script: user turn, one write tool call, its result, final text.
    fn write_call(id: &str) -> (&str, &str, serde_json::Value) {
        (
            id,
            "write",
            serde_json::json!({"path": "w.txt", "content": "x\n"}),
        )
    }

    // --- leg 1: run() on real siblings ---
    let run_root = run_tmp("shared-run");
    let run_client = ScriptClient {
        order: Arc::new(Mutex::new(Vec::new())),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([
            script_resp(vec![write_call("c1")], StopReason::ToolUse),
            text_resp("done"),
            // Held-declare round trip: the first done is held (unverified
            // write), the second lands Done.
            text_resp("done"),
        ])),
    };
    let registry = run_registry(&run_root);
    let mut run_state = LoopState::new();
    let mut run_emitter = Emitter::new();
    let cancel = CancellationToken::new();
    let outcome = run(
        &mut run_state,
        Run {
            provider: &run_client,
            registry: &registry,
            agent: "agent",
            workdir: &run_root,
            emitter: &mut run_emitter,
            bets: &NoBets,
            cfg: RunConfig::default(),
        },
        vec![Input::User("go".into())],
        &cancel,
    )
    .await;
    assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
    let _ = std::fs::remove_dir_all(&run_root);

    // --- leg 2: drive_tick on the same script ---
    let tick_root = run_tmp("shared-tick");
    let registry = run_registry(&tick_root);
    let mut s = LoopState::new();
    s.stop_when_idle = true; // the scripted run is done after the final text
    let mut emitter = Emitter::new();
    let (itx, mut irx) = mpsc::channel(8);
    let (ptx, mut prx) = mpsc::channel(8);
    let (ttx, mut trx) = mpsc::channel(8);
    let cancel = CancellationToken::new();

    itx.send(Input::User("go".into())).await.unwrap();
    let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(v, PhaseVerdict::Continue));
    s.admit_steering(); // pre-boundary: the only place steering enters the log
    let root = CancellationToken::new();
    assert!(s.start_provider_call(&root).is_some());
    ptx.send(ProviderMsg::Settled {
        turn: 1,
        message: script_resp(vec![write_call("c1")], StopReason::ToolUse).message,
        stop: StopReason::ToolUse,
        usage: None,
    })
    .await
    .unwrap();
    let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(v, PhaseVerdict::Continue));
    // The ToolCall row is durable (at claim) before the tool runs.
    let prepared = registry.prepare(
        "agent",
        ToolCall {
            call_id: "c1".into(),
            name: "write".into(),
            args: serde_json::json!({"path": "w.txt", "content": "x\n"}),
        },
    );
    let inv = match prepared {
        tool_core::CallStatus::Dispatch(inv) => inv,
        tool_core::CallStatus::Result(r) => panic!("gate must allow the fake: {}", r.content),
    };
    let out = registry
        .resolve("write")
        .unwrap()
        .execute(inv, CancellationToken::new())
        .await
        .expect("write tool runs");
    ttx.send(ToolMsg {
        call_id: "c1".into(),
        result: ToolResult {
            content: out.content,
            is_error: false,
        },
    })
    .await
    .unwrap();
    let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(v, PhaseVerdict::Continue));
    // Shared `settle_tool_msg` already counted the write (`edits += 1`)
    // like `run`'s `note_tool_execution`: no manual increment here.
    assert!(s.start_provider_call(&root).is_some());
    ptx.send(ProviderMsg::Settled {
        turn: 1,
        message: text_resp("done").message,
        stop: StopReason::Stop,
        usage: None,
    })
    .await
    .unwrap();
    // Unverified declare held: the turn stays alive, no Done yet.
    let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(v, PhaseVerdict::Continue));
    assert!(s.start_provider_call(&root).is_some());
    ptx.send(ProviderMsg::Settled {
        turn: 1,
        message: text_resp("done").message,
        stop: StopReason::Stop,
        usage: None,
    })
    .await
    .unwrap();
    let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(v, PhaseVerdict::Return(Outcome::Done)));
    let _ = std::fs::remove_dir_all(&tick_root);

    // --- drift guard: same durable order, same frame order, frames follow rows ---
    let rows_of = |items: &[Item]| -> Vec<&'static str> {
        run_kinds(items)
            .into_iter()
            .filter(|k| *k != "Header")
            .collect()
    };
    let run_rows = rows_of(&run_state.items);
    let tick_rows = rows_of(&s.items);
    assert_eq!(run_rows, tick_rows, "durable rows drifted between drivers");
    assert_eq!(
        run_rows,
        vec![
            "TurnStart",
            "Input",
            "Assistant",
            "ToolCall",
            "ToolResult",
            "Assistant",
            "Attempt",
            "Assistant",
            "TurnEnd"
        ]
    );
    let run_frames = frame_order(run_emitter.history());
    let tick_frames = frame_order(emitter.history());
    assert_eq!(
        run_frames, tick_frames,
        "frame order drifted between drivers"
    );
    assert_eq!(
        project_durable(&run_rows),
        run_frames,
        "run(): frames must follow the durable rows"
    );
    assert_eq!(
        project_durable(&tick_rows),
        tick_frames,
        "drive_tick(): frames must follow the durable rows"
    );
    assert!(check_pairing(run_emitter.history()));
    assert!(check_pairing(emitter.history()));
}

/// `drive_tick` VerifyHold parity: an unverified declare holds the turn
/// alive through the shared `step_claim` path, exactly like `run`'s
/// `VerifyHold => continue` (no `ToolStart`, `call_model` latched,
/// `verify.hold` set for the next request tail).
#[tokio::test]
async fn drive_tick_verify_hold_matches_run() {
    let mut s = LoopState::new();
    s.stop_when_idle = true;
    let mut emitter = Emitter::new();
    let (_itx, mut irx) = mpsc::channel(8);
    let (ptx, mut prx) = mpsc::channel(8);
    let (ttx, mut trx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    s.apply_input(Input::User("go".into()));
    s.admit_steering();
    let root = CancellationToken::new();
    assert!(s.start_provider_call(&root).is_some());
    // One successful write via the shared harness path: `edits` and the
    // `observe_action` tripwire increment exactly like `run`.
    ptx.send(ProviderMsg::Settled {
        turn: 1,
        message: assistant(vec![ToolCallRef {
            id: "e1".into(),
            name: "write".into(),
            args: serde_json::json!({"path": "w.txt"}),
        }]),
        stop: StopReason::ToolUse,
        usage: None,
    })
    .await
    .unwrap();
    let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(v, PhaseVerdict::Continue));
    ttx.send(ToolMsg {
        call_id: "e1".into(),
        result: ToolResult {
            content: "wrote w.txt".into(),
            is_error: false,
        },
    })
    .await
    .unwrap();
    let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(v, PhaseVerdict::Continue));
    assert_eq!(s.edits, 1);
    assert_eq!(s.budget.counters().actions_this_trial, 1);
    assert!(!s.verify.verified_since_write);
    // Unverified declare: held, not Done. No `ToolStart` for the empty
    // declare, `call_model` keeps the turn alive for the next request.
    assert!(s.start_provider_call(&root).is_some());
    let frames_before = emitter.history().len();
    ptx.send(ProviderMsg::Settled {
        turn: 1,
        message: assistant(vec![]),
        stop: StopReason::Stop,
        usage: None,
    })
    .await
    .unwrap();
    let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
    assert!(matches!(v, PhaseVerdict::Continue));
    assert!(s.call_model);
    assert_eq!(s.verify.hold.as_deref(), Some(VERIFY_NUDGE));
    assert_eq!(s.verify.nudges_used, 1);
    assert!(emitter.history()[frames_before..]
        .iter()
        .all(|e| !matches!(e, AgentEvent::ToolStart { .. })));
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

/// Per-step budget: a fresh step head resets to [`STEP_RETRY_BUDGET`]
/// (init 2), an in-step retry continuation keeps its remaining budget so
/// 3 consecutive fails still exhaust. Run-wide would exhaust after any 2
/// fails across successes; unconditional reset would never exhaust.
#[test]
fn step_retries_resets_on_fresh_step_head_not_retry_continuation() {
    let mut s = LoopState::new();
    assert_eq!(s.step_retries, STEP_RETRY_BUDGET);
    assert_eq!(STEP_RETRY_BUDGET, 2);
    s.turn = 1;
    let root = CancellationToken::new();
    assert!(s.start_provider_call(&root).is_some());
    assert_eq!(s.step_retries, 2);
    assert!(s.finish_provider_msg(ProviderMsg::Failed {
        turn: 1,
        err: "e1".into(),
        cancelled: false,
        usage: None,
    }));
    assert_eq!(s.step_retries, 1);
    assert!(s.call_model); // retry latched
                           // Retry continuation keeps the remaining 1 (no reset).
    assert!(s.start_provider_call(&root).is_some());
    assert_eq!(s.step_retries, 1, "retry must not reset");
    // Settle the retry successfully; the next fresh step resets to 2.
    assert!(s.finish_provider_msg(ProviderMsg::Settled {
        turn: 1,
        message: assistant(vec![]),
        stop: StopReason::Stop,
        usage: None,
    }));
    assert_eq!(s.step_retries, 1, "success does not itself reset");
    assert!(s.start_provider_call(&root).is_some());
    assert_eq!(s.step_retries, 2, "fresh step after success resets");
}

/// Hanging provider that never answers unless raced: without the
/// parent-side `select!` the run would wait out the full sleep.
struct HangingClient;

#[async_trait::async_trait]
impl LlmClient for HangingClient {
    async fn complete(&self, _model: &str, _req: &Request) -> Result<Response, LlmError> {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(text_resp("never"))
    }
}

/// Cancel during `provider.complete` aborts promptly: the step records a
/// `Failed { cancelled: true }` (budget refunded, no `Attempt` row) and
/// the run returns `Cancelled` in ~50ms instead of 30s. The per-turn
/// child token is cancelled when the race loses (stop_hard + aborting
/// gate propagate it to any tool children).
#[tokio::test]
async fn cancel_during_provider_complete_aborts_promptly_as_cancelled() {
    let root = run_tmp("cancel-race");
    let client = HangingClient;
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    let mut emitter = Emitter::new();
    let cancel = CancellationToken::new();
    let fired = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        fired.cancel();
    });
    let started = Instant::now();
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
        &cancel,
    )
    .await;
    let elapsed = started.elapsed();
    assert!(
        matches!(outcome, Outcome::Cancelled),
        "cancel must abort as Cancelled, got {outcome:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "must not wait out the 30s provider, took {elapsed:?}"
    );
    // Step-level `Failed { cancelled: true }`: refunded, never retried.
    assert_eq!(
        state.budget.counters().steps,
        0,
        "cancelled refunds the step"
    );
    assert!(
        !state
            .items
            .iter()
            .any(|i| matches!(&i.kind, ItemKind::Attempt { .. })),
        "cancelled records no Attempt row"
    );
    assert!(state.stop_hard);
    assert_eq!(state.gate.status(), GateStatus::Aborting);
    assert!(emitter.history().iter().any(|e| matches!(
        e,
        AgentEvent::Error { error } if error.code == "provider-failed"
            && error.message.contains("cancelled")
    )));
    let _ = std::fs::remove_dir_all(&root);
}

// --- PHASE 5 fixes: attribution, headroom, replay, termination, log boundary ---

/// (1) consumption: `build_request` peeks without consuming; `run` takes
/// only on successful send, so a provider Err+retry re-arms the hold.
#[test]
fn verify_hold_peek_survives_retry_and_takes_on_success() {
    let root = run_tmp("hold-peek");
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    state.phase = Phase::Running;
    state.apply_input(Input::User("go".into()));
    state.admit_steering();
    ok_round(&mut state, "e1", "edit", Value::Null);
    assert!(matches!(declare(&mut state), ClaimOutcome::VerifyHold));
    assert!(state.verify.hold.is_some());
    let cfg = RunConfig::default();
    // Peek: still armed after the build.
    let r1 = build_request(&mut state, &registry, &root, &cfg);
    assert!(r1.messages.iter().any(|m| m.content == VERIFY_NUDGE));
    assert!(state.verify.hold.is_some(), "peek must not consume");
    // Provider Err path in `run` keeps it: rebuild still carries it.
    let r_retry = build_request(&mut state, &registry, &root, &cfg);
    assert!(r_retry.messages.iter().any(|m| m.content == VERIFY_NUDGE));
    // `run`'s take-on-Ok: delivered once, never twice.
    state.verify.hold.take();
    let r2 = build_request(&mut state, &registry, &root, &cfg);
    assert!(r2.messages.iter().all(|m| m.content != VERIFY_NUDGE));
    let _ = std::fs::remove_dir_all(&root);
}

/// (1) attribution: the hold is its own user-role row, never merged into
/// the model's assistant declare text.
#[test]
fn verify_hold_own_row_not_merged_into_assistant_declare() {
    let root = run_tmp("hold-attr");
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    state.phase = Phase::Running;
    state.apply_input(Input::User("go".into()));
    state.admit_steering();
    ok_round(&mut state, "e1", "edit", Value::Null);
    assert!(matches!(declare(&mut state), ClaimOutcome::VerifyHold));
    let req = build_request(&mut state, &registry, &root, &RunConfig::default());
    // The declare row is assistant; the hold row is a separate user row.
    let assistant_declares: Vec<&ProviderMessage> = req
        .messages
        .iter()
        .filter(|m| m.role == "assistant" && m.tool_calls.is_empty())
        .collect();
    let _ = assistant_declares;
    let last = req.messages.last().unwrap();
    assert_eq!(last.role, "user");
    assert_eq!(last.content, VERIFY_NUDGE);
    for m in &req.messages[1..req.messages.len() - 1] {
        assert!(!m.content.contains(VERIFY_NUDGE), "merged into {m:?}");
    }
    // The assistant declare text itself never carries the nudge.
    let declares: Vec<&ProviderMessage> = req
        .messages
        .iter()
        .filter(|m| m.role == "assistant")
        .collect();
    assert!(!declares.is_empty());
    for d in declares {
        assert!(!d.content.contains(VERIFY_NUDGE));
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// (2) reserve: fewer than 2 steps remaining vetoes the hold (test +
/// re-declare need two calls).
#[test]
fn verify_nudge_no_hold_without_two_step_headroom() {
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    let max = s.budget.config().max_steps.get();
    // Two remaining: hold fires.
    s.budget.counters_mut().steps = max - 2;
    assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
    // One remaining: silent Done, no grace hold.
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    s.budget.counters_mut().steps = max - 1;
    assert!(matches!(declare(&mut s), ClaimOutcome::Done));
    assert!(s.verify.hold.is_none());
    // Zero remaining (already at max): silent Done.
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    s.budget.counters_mut().steps = max;
    assert!(matches!(declare(&mut s), ClaimOutcome::Done));
}

/// (2) tokens cap vetoes the hold.
#[test]
fn verify_nudge_no_hold_when_tokens_exhausted() {
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    s.budget.counters_mut().tokens = s.budget.config().max_tokens;
    assert!(matches!(declare(&mut s), ClaimOutcome::Done));
    assert!(s.verify.hold.is_none());
}

/// (2) spend cap vetoes the hold.
#[test]
fn verify_nudge_no_hold_when_spend_exhausted() {
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    let limit = s.budget.config().max_spend_cents.unwrap();
    s.budget.counters_mut().spent_cents = limit;
    assert!(matches!(declare(&mut s), ClaimOutcome::Done));
    assert!(s.verify.hold.is_none());
}

/// (2) wallclock cap vetoes the hold.
#[test]
fn verify_nudge_no_hold_when_wallclock_exhausted() {
    use agent_budget::{config_for, BudgetGuard, Capability};
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    // Elapsed past the wallclock cap: consults the same gate `terminate` halts on.
    s.budget = BudgetGuard::new(
        config_for(Capability::UnattendedBatch),
        Instant::now() - Duration::from_secs(10_000),
    );
    // Re-apply the write (new guard reset the counters): one edit, unverified.
    // `ok_round` already burned edits=1 on the old guard; restore it.
    s.edits = 1;
    assert!(matches!(declare(&mut s), ClaimOutcome::Done));
    assert!(s.verify.hold.is_none());
}

/// (2) exhausted budget (any cap, e.g. actions) gets no grace hold:
/// consults `terminate`'s AND-gate instead of bypassing it via `continue`.
#[test]
fn verify_nudge_no_hold_when_budget_exceeded() {
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    s.budget.counters_mut().actions_this_trial = s.budget.config().actions_per_trial;
    assert!(s.budget.exceeded().is_some());
    assert!(matches!(declare(&mut s), ClaimOutcome::Done));
    assert!(s.verify.hold.is_none());
}

/// (3) replay: the hold fire persists a durable Attempt record so the log
/// (and a file replay via `read_log`) reproduces the fire, while the
/// model-visible copy rides the next request as its own row.
#[test]
fn verify_hold_persisted_as_attempt_for_replay() {
    let mut s = LoopState::new();
    ok_round(&mut s, "e1", "edit", Value::Null);
    assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
    let holds: Vec<&str> = s
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::Attempt {
                error,
                will_retry: true,
            } if error == VERIFY_NUDGE => Some(error.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(holds.len(), 1);
    // Replay from the items vec reproduces the fire.
    let replayed = s.items.clone();
    assert!(replayed.iter().any(|i| matches!(
        &i.kind,
        ItemKind::Attempt { error, .. } if error == VERIFY_NUDGE
    )));
    // `derived_messages` still folds items only (Attempt is log-only),
    // so the durable row is the replay source, not a folded message.
    assert!(s
        .derived_messages()
        .iter()
        .all(|m| m.content != VERIFY_NUDGE));
}

/// (4) `same_action_cycles == 0` disables the tripwire lesson, mirroring
/// `observe_action`'s `> 0` guard.
#[test]
fn terminate_same_action_zero_disables_lesson() {
    use agent_budget::{config_for, BudgetConfig, BudgetGuard, Capability};
    let mut s = LoopState::new();
    let cfg = BudgetConfig {
        same_action_cycles: 0,
        ..config_for(Capability::UnattendedBatch)
    };
    s.budget = BudgetGuard::new(cfg, Instant::now());
    // Streak 0 >= cycles 0 would fire without the guard.
    assert_eq!(s.budget.counters().same_action_streak, 0);
    assert!(matches!(s.terminate(), PhaseVerdict::Continue));
    assert!(s.lessons.is_empty(), "cycles=0 must not lesson");
}

/// (5) already-started final step: `record_step` hit max at the head, the
/// admitted step's Done still lands Done instead of Halted(steps).
#[tokio::test]
async fn last_step_done_wins_over_steps_halt() {
    let root = run_tmp("last-done");
    let client = ScriptClient {
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

/// (7) stop labels are explicit matches, never `Debug`.
#[test]
fn stop_label_is_explicit_not_debug() {
    assert_eq!(stop_label(StopReason::Pending), "Pending");
    assert_eq!(stop_label(StopReason::Stop), "Stop");
    assert_eq!(stop_label(StopReason::ToolUse), "ToolUse");
    assert_eq!(stop_label(StopReason::MaxTokens), "MaxTokens");
    assert_eq!(stop_label(StopReason::Refused), "Refused");
    assert_eq!(stop_label(StopReason::Error), "Error");
    assert_eq!(stop_label(StopReason::Aborted), "Aborted");
    assert_eq!(stop_label(StopReason::Deferred), "Deferred");
}

/// (7) corrupt assistant rows surface an explicit marker, never silent
/// defaults or raw JSON.
#[test]
fn assistant_corrupt_row_is_explicit_marker_not_raw_json() {
    // Corrupt stored rows (Null and object-without-content): explicit
    // corrupt marker, never silent defaults or raw JSON (`Null` -> "null",
    // `{"bad":1}` -> raw object text).
    for bad in [Value::Null, serde_json::json!({"bad": 1})] {
        let mut s = LoopState::new();
        s.items.push(Item {
            seq: 1,
            id: "x".into(),
            parent_id: None,
            recorded_at: SystemTime::now(),
            kind: ItemKind::Assistant {
                message: bad,
                stop_reason: "Stop".into(),
                interrupted: false,
            },
        });
        let msgs = s.derived_messages();
        assert_eq!(msgs.len(), 1);
        assert!(
            msgs[0].content.starts_with("[corrupt assistant row:"),
            "{}",
            msgs[0].content
        );
        // Fail-closed marker, not silent wrong data: never the bare
        // raw-JSON fallback nor an empty default.
        assert_ne!(msgs[0].content, "null");
        assert!(!msgs[0].content.contains("bad"));
        assert!(!msgs[0].content.is_empty());
    }
}

/// (7) `push_assistant` never stores a silent Null.
#[test]
fn push_assistant_never_stores_null() {
    let mut s = LoopState::new();
    s.push_assistant(
        &AssistantMessage {
            content: "hi".into(),
            tool_calls: Vec::new(),
            thinking: None,
        },
        "Stop",
    );
    match &s.items[0].kind {
        ItemKind::Assistant {
            message,
            stop_reason,
            ..
        } => {
            assert_ne!(message, &Value::Null);
            assert_eq!(message["content"], "hi");
            assert_eq!(stop_reason, "Stop");
        }
        other => panic!("expected Assistant, got {other:?}"),
    }
}

/// Production-shaped check tool: mirrors `TestTool` — command failure is
/// `Ok` with FAIL content and `success: false`, never `Err`.
struct ProdCheck;

#[async_trait::async_trait]
impl CoreTool for ProdCheck {
    fn definition(&self) -> CoreToolDef {
        CoreToolDef {
            name: "test".into(),
            description: "production-shaped check".into(),
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
        _cancel: CancellationToken,
    ) -> Result<CoreToolOutcome, CoreToolError> {
        let cmd = inv.args.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
        // Always fails: the FAIL-content shape the old code mistook for a pass.
        Ok(CoreToolOutcome {
            content: format!("FAIL: {cmd}\n1 failed"),
            truncated: false,
            success: false,
        })
    }
}

/// End-to-end through `run()`: a FAIL-content `test` outcome maps to an
/// error result and verifies nothing, so the declare is held live (the 4th
/// request carries the nudge) instead of landing Done.
#[tokio::test]
async fn run_e2e_failing_test_does_not_verify_but_holds_declare() {
    let root = run_tmp("verify-e2e");
    std::fs::write(root.join("a.rs"), "v1\n").unwrap();
    let client = ScriptClient {
        order: Arc::new(Mutex::new(Vec::new())),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([
            script_resp(
                vec![(
                    "w1",
                    "write",
                    serde_json::json!({"path": "a.rs", "content": "v2\n"}),
                )],
                StopReason::ToolUse,
            ),
            script_resp(
                vec![("t1", "test", serde_json::json!({"cmd": "check"}))],
                StopReason::ToolUse,
            ),
            text_resp("done"),
            text_resp("done again"),
        ])),
    };
    let mut registry = CoreRegistry::new(Arc::new(GrantGate::new(
        [("agent".to_string(), vec!["write".into(), "test".into()])].into(),
    )));
    registry.register(Arc::new(WriteFile { root: root.clone() }));
    registry.register(Arc::new(ProdCheck));
    let mut state = LoopState::new();
    let mut emitter = Emitter::new();
    let cancel = CancellationToken::new();
    // Queue runs dry after the second declare: the run ends on transport,
    // but the hold must have fired first — that is what this test proves.
    let _outcome = run(
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
        vec![Input::User("ship it".into())],
        &cancel,
    )
    .await;
    // The hold added a round trip and delivered the nudge live.
    let reqs = client.requests.lock().unwrap();
    assert!(reqs.len() >= 4, "held declare adds a round trip");
    let hold_req = serde_json::to_string(&reqs[3]).unwrap();
    assert!(hold_req.contains(VERIFY_NUDGE), "nudge delivered live");
    drop(reqs);
    // The FAIL-content result is an error row, and it verified nothing.
    let fail_rows: Vec<_> = state
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::ToolResult {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
        .collect();
    assert!(
        fail_rows.iter().any(|(c, e)| *e && c.contains("FAIL")),
        "FAIL outcome is an error row: {fail_rows:?}"
    );
    assert!(
        state.items.iter().any(|i| matches!(
            &i.kind,
            ItemKind::Attempt { error, will_retry: true } if error == VERIFY_NUDGE
        )),
        "hold recorded as a retryable attempt"
    );
}
