use crate::proof::outcome_to_result;
use crate::request::build_request;
use crate::run::drive_tick;
use crate::state::ProviderMsg;
use crate::state::ToolMsg;
use crate::verify::is_verification_call;
use crate::verify::VERIFY_NUDGE;
use crate::*;
use agent_event::AgentEvent;
use agent_event::Emitter;
use agent_log::ItemKind;
use provider_core::ProviderMessage;
use provider_core::StopReason;
use provider_core::ToolCallRef;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tool_core::{
    CallStatus as CoreCallStatus, GrantGate, Invocation as CoreInvocation,
    Registry as CoreRegistry, Tool as CoreTool, ToolCall as CoreToolCall,
    ToolDefinition as CoreToolDef, ToolError as CoreToolError, ToolOutcome as CoreToolOutcome,
};
use tool_core::{ToolOutcome, ToolResult};

use super::{
    assistant, declare, full_event_order, ok_round, result, run_kinds, run_registry, run_tmp,
    script_resp, text_resp, FakeLlm, WriteFile,
};

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
        &RunConfig::default(),
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
        &RunConfig::default(),
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
        &RunConfig::default(),
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
        let cfg = RunConfig {
            incentives: level,
            ..RunConfig::default()
        };
        // One edit round under this level's config, mirroring `ok_round`.
        let outcome = s.step_claim(
            assistant(vec![ToolCallRef {
                id: "e1".into(),
                name: "edit".into(),
                args: Value::Null,
            }]),
            StopReason::ToolUse,
            &cfg,
        );
        assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
        assert!(s.record_tool_result(ToolMsg {
            call_id: "e1".into(),
            result: result(),
        }));
        s.edits += 1;
        assert!(matches!(
            s.step_claim(assistant(vec![]), StopReason::Stop, &cfg),
            ClaimOutcome::Done
        ));
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
    assert!(matches!(v, PhaseVerdict::Continue));
    assert!(s.call_model);
    assert_eq!(s.verify.hold.as_deref(), Some(VERIFY_NUDGE));
    assert_eq!(s.verify.nudges_used, 1);
    assert!(emitter.history()[frames_before..]
        .iter()
        .all(|e| !matches!(e, AgentEvent::ToolStart { .. })));
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
        .derived_messages(&RunConfig::default())
        .iter()
        .all(|m| m.content != VERIFY_NUDGE));
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
    let client = FakeLlm {
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

#[tokio::test]
async fn char_verify_hold_event_and_log_sequence() {
    use agent_event::check_pairing;
    let root = run_tmp("char-hold");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let client = FakeLlm {
        order: Default::default(),
        requests: requests.clone(),
        queue: Mutex::new(VecDeque::from([
            script_resp(
                vec![(
                    "c1",
                    "write",
                    serde_json::json!({"path": "w.txt", "content": "x\n"}),
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
    // The held declare adds a durable Attempt row and a third round trip.
    assert_eq!(
        run_kinds(&state.items),
        vec![
            "Header",
            "TurnStart",
            "Input",
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
    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 3, "held declare adds one round trip");
    assert!(check_pairing(emitter.history()));
    let _ = std::fs::remove_dir_all(&root);
}
