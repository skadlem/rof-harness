use crate::request::CHECKPOINT_PREFIX;
use crate::run::drive_tick;
use crate::state::ProviderMsg;
use crate::state::ToolMsg;
use crate::*;
use agent_event::AgentEvent;
use agent_event::Emitter;
use agent_log::Item;
use agent_log::ItemKind;
use provider_core::LlmError;
use provider_core::Request;
use provider_core::StopReason;
use provider_core::Usage;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tool_core::ToolCall;
use tool_core::ToolResult;
use tool_core::{
    CallStatus as CoreCallStatus, GrantGate, Invocation as CoreInvocation,
    Registry as CoreRegistry, Tool as CoreTool, ToolCall as CoreToolCall,
    ToolDefinition as CoreToolDef, ToolError as CoreToolError, ToolOutcome as CoreToolOutcome,
};

use super::{
    compaction, edit_registry, event_order, full_event_order, last_totals, response, run_kinds,
    run_registry, run_script, run_tmp, script_resp, summary_resp, text_resp, text_response,
    tool_use_message, usage, write_round, FakeLlm, Scripted,
};

#[tokio::test]
async fn drive_tick_select_spine() {
    let mut s = LoopState::new();
    let (itx, mut irx) = mpsc::channel(8);
    let (_ptx, mut prx) = mpsc::channel(8);
    let (_ttx, mut trx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let mut emitter = Emitter::new();
    itx.send(Input::User("go".into())).await.unwrap();
    let verdict = drive_tick(
        &mut s,
        &mut irx,
        &mut prx,
        &mut trx,
        &cancel,
        &mut emitter,
        &RunConfig::default(),
    )
    .await;
    assert!(matches!(verdict, PhaseVerdict::Continue));
    assert_eq!(s.turn, 1);
    itx.send(Input::StopHard).await.unwrap();
    let verdict = drive_tick(
        &mut s,
        &mut irx,
        &mut prx,
        &mut trx,
        &cancel,
        &mut emitter,
        &RunConfig::default(),
    )
    .await;
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
    let verdict = drive_tick(
        &mut s,
        &mut irx,
        &mut prx,
        &mut trx,
        &cancel,
        &mut emitter,
        &RunConfig::default(),
    )
    .await;
    assert!(matches!(verdict, PhaseVerdict::Return(Outcome::Cancelled)));
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
    s.admit_steering(); // pre-boundary: the only place steering enters the log
    let root = CancellationToken::new();
    assert!(s.start_provider_call(&root).is_some()); // step head: may_step + record
    ptx.send(ProviderMsg::Partial {
        turn: 1,
        text: "thinking ".into(),
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
    ptx.send(ProviderMsg::Settled {
        turn: 1,
        message: tool_use_message(),
        stop: StopReason::ToolUse,
        usage: Some(usage()),
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
    // Batch done, results owed back to the model: the tick parks on.
    assert!(matches!(v, PhaseVerdict::Continue));
    // A hard stop drains the parked idle and closes the turn.
    itx.send(Input::StopHard).await.unwrap();
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
    let mut turn_one_done = text_response("turn one done");
    turn_one_done.usage.input = 1_000; // arms turn 2's estimate
                                       // Held-declare round trip: turn one done is held (unverified
                                       // writes); the repeat lands Done. input 1000 re-arms turn 2's
                                       // checkpoint estimate exactly like the held response did; the short
                                       // text keeps the keep_tokens cut on the same row.
    let turn_one_done = Scripted::Respond(turn_one_done);
    let mut turn_one_again = text_response("done again");
    turn_one_again.usage.input = 1_000;
    let turn_one_again = Scripted::Respond(turn_one_again);
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

/// DeepSeek thinking mode: turn 2's request must echo turn 1's assistant
/// `thinking`, or the API 400s mid-run (measured, api.deepseek.com).
#[tokio::test]
async fn run_multi_turn_request_echoes_assistant_thinking() {
    let root = run_tmp("think");
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut first = response(
        vec![(
            "c1",
            "write",
            serde_json::json!({"path": "a.txt", "content": "bye\n"}),
        )],
        StopReason::ToolUse,
    );
    first.message.thinking = Some("must edit a.txt".into());
    let first = Scripted::Respond(first);
    let client = FakeLlm {
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
    let client = FakeLlm {
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
        Outcome::Failed { message: msg, .. } => assert!(msg.contains("script empty"), "got {msg}"),
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
fn metered_fail_then_ok(usage: Usage, exhausted: bool) -> FakeLlm {
    FakeLlm {
        order: Default::default(),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([
            Scripted::Fail(LlmError::Metered {
                source: Box::new(LlmError::Transport("output truncated at 64 tokens".into())),
                usage: Some(usage),
                exhausted,
            }),
            text_resp("done"),
        ])),
    }
}

#[tokio::test]
async fn run_meters_failed_attempt_usage_then_retries() {
    let root = run_tmp("meteredfail");
    let client = metered_fail_then_ok(
        Usage {
            input: 100,
            output: 20,
            cache_read: 0,
            cache_write: 0,
            reasoning: None,
            cost_usd: Some(0.02),
        },
        false,
    );
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

/// Worst case: an `exhausted` ladder must not be re-run in-step. One model
/// call, one non-retryable Attempt, one Error frame, then fatal.
#[tokio::test]
async fn run_exhausted_ladder_is_never_retried_in_step() {
    let root = run_tmp("exhausted");
    let order = Arc::new(Mutex::new(Vec::new()));
    let client = FakeLlm {
        order: order.clone(),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([Scripted::Fail(LlmError::Metered {
            source: Box::new(LlmError::Transport("output truncated at 64 tokens".into())),
            usage: Some(Usage {
                input: 100,
                output: 20,
                cache_read: 0,
                cache_write: 0,
                reasoning: None,
                cost_usd: Some(0.02),
            }),
            exhausted: true,
        })])),
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
    // The billed attempt is still metered, but the ladder runs exactly once.
    assert_eq!(order.lock().unwrap().len(), 1, "exhausted ladder re-ran");
    assert_eq!(state.budget.counters().tokens, 120);
    let attempts: Vec<bool> = state
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::Attempt { will_retry, .. } => Some(*will_retry),
            _ => None,
        })
        .collect();
    assert_eq!(attempts, vec![false], "exhausted must not retry");
    assert!(!state.call_model);
    match run_end_of(emitter.history()) {
        agent_event::RunOutcome::Failed(msg) => assert!(msg.contains("truncated"), "got {msg}"),
        other => panic!("expected failed RunEnd, got {other:?}"),
    }
    drop(outcome);
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
    let run_client = FakeLlm {
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
    s.admit_steering(); // pre-boundary: the only place steering enters the log
    let root = CancellationToken::new();
    assert!(s.start_provider_call(&root).is_some());
    ptx.send(ProviderMsg::Settled {
        turn: 1,
        message: response(vec![write_call("c1")], StopReason::ToolUse).message,
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
    // Shared `settle_tool_msg` already counted the write (`edits += 1`)
    // like `run`'s `note_tool_execution`: no manual increment here.
    assert!(s.start_provider_call(&root).is_some());
    ptx.send(ProviderMsg::Settled {
        turn: 1,
        message: text_response("done").message,
        stop: StopReason::Stop,
        usage: None,
    })
    .await
    .unwrap();
    // Unverified declare held: the turn stays alive, no Done yet.
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
    assert!(s.start_provider_call(&root).is_some());
    ptx.send(ProviderMsg::Settled {
        turn: 1,
        message: text_response("done").message,
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

/// Cancel during `provider.complete` aborts promptly: the step records a
/// `Failed { cancelled: true }` (budget refunded, no `Attempt` row) and
/// the run returns `Cancelled` in ~50ms instead of 30s. The per-turn
/// child token is cancelled when the race loses (stop_hard + aborting
/// gate propagate it to any tool children).
#[tokio::test]
async fn cancel_during_provider_complete_aborts_promptly_as_cancelled() {
    let root = run_tmp("cancel-race");
    // Hanging provider: without the parent-side `select!` the run would
    // wait out the full sleep.
    let client = FakeLlm {
        order: Default::default(),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([Scripted::Hang])),
    };
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

// --- S5 Step 0 characterization: event + log sequences pinned before refactor ---
//
// These pin the observable behavior (emitted event order, durable log order,
// terminal outcome) for six scenarios. They assert through the event
// vocabulary (`RunEnd`) and the log vocabulary (`ItemKind`), which the
// refactor preserves, so they pass unchanged before and after.

/// The terminal `RunEnd` outcome, cloned out for assertions that must survive
/// the `Outcome` shape refactor (events are the stable vocabulary).
fn run_end_of(history: &[AgentEvent]) -> agent_event::RunOutcome {
    history
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::RunEnd { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .expect("run ends with RunEnd")
}

#[tokio::test]
async fn char_done_event_and_log_sequence() {
    use agent_event::check_pairing;
    let root = run_tmp("char-done");
    let client = FakeLlm {
        order: Default::default(),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([text_resp("finished")])),
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
    assert_eq!(
        run_kinds(&state.items),
        vec!["Header", "TurnStart", "Input", "Assistant", "TurnEnd"]
    );
    assert_eq!(
        full_event_order(emitter.history()),
        vec![
            "RunStart",
            "TurnStart",
            "MessageStart",
            "MessageUpdate",
            "MessageEnd",
            "TurnEnd",
            "RunEnd",
        ]
    );
    assert!(matches!(
        run_end_of(emitter.history()),
        agent_event::RunOutcome::Passed
    ));
    assert!(check_pairing(emitter.history()));
    let _ = std::fs::remove_dir_all(&root);
}

/// Tool that never answers unless the cancel race drops it.
struct HangingTool;

#[async_trait::async_trait]
impl CoreTool for HangingTool {
    fn definition(&self) -> CoreToolDef {
        CoreToolDef {
            name: "hang".into(),
            description: "sleeps past any test deadline".into(),
            schema: serde_json::json!({}),
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
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(CoreToolOutcome {
            content: "never".into(),
            truncated: false,
            success: true,
        })
    }
}

#[tokio::test]
async fn char_mid_tool_cancel_aborts_as_cancelled() {
    let root = run_tmp("char-toolcancel");
    std::fs::write(root.join("a.rs"), "v1\n").unwrap();
    let client = FakeLlm {
        order: Default::default(),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([script_resp(
            vec![("c1", "hang", serde_json::json!({}))],
            StopReason::ToolUse,
        )])),
    };
    let mut registry = CoreRegistry::new(Arc::new(GrantGate::new(
        [("agent".to_string(), vec!["hang".to_string()])].into(),
    )));
    registry.register(Arc::new(HangingTool));
    let mut state = LoopState::new();
    let mut emitter = Emitter::new();
    let cancel = CancellationToken::new();
    let fired = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        fired.cancel();
    });
    let started = Instant::now();
    // Bounded drain: the cancelled tool refunds to Cancelled at the deadline.
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
                drain_timeout: Duration::from_millis(100),
                ..RunConfig::default()
            },
        },
        vec![Input::User("go".into())],
        &cancel,
    )
    .await;
    let elapsed = started.elapsed();
    assert!(matches!(outcome, Outcome::Cancelled), "got {outcome:?}");
    assert!(
        elapsed < Duration::from_secs(5),
        "must not wait out the 30s tool, took {elapsed:?}"
    );
    // The cancelled call is a durable error result; the failed batch's
    // rollback notice follows before the terminal TurnEnd.
    assert_eq!(
        run_kinds(&state.items),
        vec![
            "Header",
            "TurnStart",
            "Input",
            "Assistant",
            "ToolCall",
            "ToolResult",
            "Attempt",
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
            "TurnEnd",
            "RunEnd",
        ]
    );
    assert!(matches!(
        run_end_of(emitter.history()),
        agent_event::RunOutcome::Aborted
    ));
    let _ = std::fs::remove_dir_all(&root);
}

/// A log that opens but never persists: every append fails closed.
#[cfg(unix)]
#[tokio::test]
async fn char_log_append_failure_fails_closed() {
    let root = run_tmp("char-logfail");
    let client = FakeLlm {
        order: Default::default(),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([text_resp("finished")])),
    };
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    let mut emitter = Emitter::new();
    let _outcome = run(
        &mut state,
        Run {
            provider: &client,
            registry: &registry,
            agent: "agent",
            workdir: &root,
            emitter: &mut emitter,
            bets: &NoBets,
            cfg: RunConfig {
                log_path: Some(std::path::PathBuf::from("/dev/full")),
                ..RunConfig::default()
            },
        },
        vec![Input::User("go".into())],
        &CancellationToken::new(),
    )
    .await;
    // Asserted through the stable event vocabulary (see `run_end_of`).
    match run_end_of(emitter.history()) {
        agent_event::RunOutcome::Failed(msg) => assert!(msg.contains("log append"), "got {msg}"),
        other => panic!("expected failed RunEnd, got {other:?}"),
    }
    // The failure lands before any model call: no Message frames, but the
    // terminal TurnEnd + RunEnd still emit so live sees a close.
    assert_eq!(
        full_event_order(emitter.history()),
        vec!["RunStart", "TurnEnd", "RunEnd"]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn char_provider_failure_event_and_log_sequence() {
    use agent_event::check_pairing;
    let root = run_tmp("char-provfail");
    // Empty script: every complete() call returns Err(Transport(...)).
    let client = FakeLlm {
        order: Default::default(),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::new()),
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
    match run_end_of(emitter.history()) {
        agent_event::RunOutcome::Failed(msg) => assert!(msg.contains("script empty"), "got {msg}"),
        other => panic!("expected failed RunEnd, got {other:?}"),
    }
    drop(outcome);
    // Three provider-failed Errors (1 initial + 2 in-step retries), then close.
    assert_eq!(
        full_event_order(emitter.history()),
        vec![
            "RunStart",
            "TurnStart",
            "Error",
            "Error",
            "Error",
            "TurnEnd",
            "RunEnd"
        ]
    );
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
    assert!(check_pairing(emitter.history()));
    let _ = std::fs::remove_dir_all(&root);
}

/// `FailureKind` pins: each run failure site reports its kind, so callers
/// react without parsing message text.
#[cfg(unix)]
#[tokio::test]
async fn failure_kinds_pin_log_open_and_no_input_sites() {
    // Log-open failure: the parent dir does not exist.
    let root = run_tmp("char-kindlog");
    let client = FakeLlm {
        order: Default::default(),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::from([text_resp("finished")])),
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
            cfg: RunConfig {
                log_path: Some(root.join("no-such-dir").join("run.jsonl")),
                ..RunConfig::default()
            },
        },
        vec![Input::User("go".into())],
        &CancellationToken::new(),
    )
    .await;
    match outcome {
        Outcome::Failed { kind, message } => {
            assert_eq!(kind, FailureKind::Log);
            assert!(message.contains("log open"), "got {message}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&root);
    // No input seeds no turn.
    let root = run_tmp("char-kindinput");
    let client = FakeLlm {
        order: Default::default(),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::new()),
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
        Vec::new(),
        &CancellationToken::new(),
    )
    .await;
    match outcome {
        Outcome::Failed { kind, message } => {
            assert_eq!(kind, FailureKind::Input);
            assert!(message.contains("at least one input"), "got {message}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn failure_kinds_pin_snapshot_ensure_site() {
    // Read-only workdir: `git init` fails, so `ensure` fails Snapshot.
    let root = run_tmp("char-kindensure");
    let client = FakeLlm {
        order: Default::default(),
        requests: Default::default(),
        queue: Mutex::new(VecDeque::new()),
    };
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    let mut emitter = Emitter::new();
    let mut perms = std::fs::metadata(&root).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&root, perms).unwrap();
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
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    match outcome {
        Outcome::Failed { kind, message } => {
            assert_eq!(kind, FailureKind::Snapshot);
            assert!(message.contains("snapshot ensure"), "got {message}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&root);
}
