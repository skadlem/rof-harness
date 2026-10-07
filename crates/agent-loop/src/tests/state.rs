use crate::state::InFlight;
use crate::state::ProviderMsg;
use crate::state::{turn_end_reason_to_event, STEP_RETRY_BUDGET};
use crate::verify::stop_label;
use crate::*;
use agent_budget::BudgetHalt;
use agent_log::{InputSource, Item, ItemKind, TurnEndReason};
use provider_core::AssistantMessage;
use provider_core::StopReason;
use provider_core::Usage;
use serde_json::Value;
use std::time::{Duration, Instant, SystemTime};
use tokio_util::sync::CancellationToken;

use super::{assistant, settled};

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
    assert!(matches!(
        s.terminate(&RunConfig::default()),
        PhaseVerdict::Continue
    ));
    let mut s = LoopState::new(); // cancel beats budget
    s.stop_hard = true;
    s.budget.counters_mut().steps = s.budget.config().max_steps.get();
    assert!(matches!(
        s.terminate(&RunConfig::default()),
        PhaseVerdict::Return(Outcome::Cancelled)
    ));
    let mut s = LoopState::new(); // hard error beats budget
    s.fatal_error = Some("e".into());
    let max = s.budget.config().max_steps.get();
    s.budget.counters_mut().steps = max;
    assert!(matches!(
        s.terminate(&RunConfig::default()),
        PhaseVerdict::Return(Outcome::Failed { .. })
    ));
    let mut s = LoopState::new(); // budget beats done-shaped state
    s.stop_when_idle = true;
    let max = s.budget.config().max_steps.get();
    s.budget.counters_mut().steps = max;
    assert!(matches!(
        s.terminate(&RunConfig::default()),
        PhaseVerdict::Return(Outcome::Halted(_))
    ));
    let mut s = LoopState::new(); // refusal is terminal-with-error
    s.turn_reason = Some(TurnEndReason::Error("refused".into()));
    assert!(matches!(
        s.terminate(&RunConfig::default()),
        PhaseVerdict::Return(Outcome::Failed { .. })
    ));
    let mut s = LoopState::new(); // idle + StopWhenIdle exits
    s.stop_when_idle = true;
    assert!(matches!(
        s.terminate(&RunConfig::default()),
        PhaseVerdict::Return(Outcome::Done)
    ));
    // guard halt labels surface verbatim
    let mut s = LoopState::new();
    let max = s.budget.config().max_steps.get();
    s.budget.counters_mut().steps = max;
    assert!(matches!(
        s.terminate(&RunConfig::default()),
        PhaseVerdict::Return(Outcome::Halted(ref s)) if s == "steps"
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

/// RAII turn guard: appends TurnEnd on drop, always, even on unwind.
/// TEST-ONLY substrate: prod turns use `open_turn`/`append_to(TurnEnd)`;
/// only the test below constructs this. Kept for the unwind-safety
/// property, not wired into `run()`.
struct TurnGuard<'a> {
    items: Option<&'a mut Vec<Item>>,
    turn_id: String,
    reason: TurnEndReason,
}

impl<'a> TurnGuard<'a> {
    fn open(items: &'a mut Vec<Item>, turn: u64) -> Self {
        let turn_id = format!("turn-{turn}");
        append_to(
            items,
            ItemKind::TurnStart {
                turn_id: turn_id.clone(),
                prev_turn_id: None,
            },
        );
        Self {
            items: Some(items),
            turn_id,
            reason: TurnEndReason::Completed,
        }
    }
    fn end(mut self, reason: TurnEndReason) {
        self.reason = reason;
    }
}

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        if let Some(items) = self.items.take() {
            append_to(
                items,
                ItemKind::TurnEnd {
                    turn_id: self.turn_id.clone(),
                    reason: self.reason.clone(),
                },
            );
        }
    }
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
    assert!(matches!(
        s.terminate(&RunConfig::default()),
        PhaseVerdict::Break
    ));
    // deadline passes: terminal now.
    let zero_drain = RunConfig {
        drain_timeout: Duration::ZERO,
        ..RunConfig::default()
    };
    s.drain_until = None;
    assert!(matches!(
        s.terminate(&zero_drain),
        PhaseVerdict::Return(Outcome::Cancelled)
    ));
    let mut s = LoopState::new(); // idle cancels at once
    s.stop_hard = true;
    assert!(matches!(
        s.terminate(&RunConfig::default()),
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
    assert!(matches!(
        s.terminate(&RunConfig::default()),
        PhaseVerdict::Continue
    ));
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
        s.terminate(&RunConfig::default()),
        PhaseVerdict::Return(Outcome::Failed { .. })
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
        s.terminate(&RunConfig::default()),
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
    assert!(matches!(
        s.terminate(&RunConfig::default()),
        PhaseVerdict::Continue
    ));
    assert!(s.lessons.is_empty(), "cycles=0 must not lesson");
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
        let msgs = s.derived_messages(&RunConfig::default());
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
