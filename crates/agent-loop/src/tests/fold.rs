use crate::request::CHECKPOINT_PREFIX;
use crate::*;
use agent_event::Emitter;
use agent_log::Item;
use agent_log::ItemKind;
use provider_core::AssistantMessage;
use provider_core::ProviderMessage;
use provider_core::StopReason;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tokio_util::sync::CancellationToken;

use super::{
    response, run_registry, run_script, run_tmp, text_resp, write_round, FakeLlm, Scripted,
};

#[test]
fn derived_messages_fold_history() {
    let mut s = LoopState::new();
    s.phase = Phase::Running;
    s.apply_input(Input::User("build it".into()));
    s.admit_steering();
    let msgs = s.derived_messages(&RunConfig::default());
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
    let msgs = s.derived_messages(&RunConfig::default());
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
    let msgs = s.raw_messages_with(&RunConfig::default(), 0);
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
        serde_json::to_string(&s.raw_messages(&RunConfig::default())).unwrap(),
        serde_json::to_string(&msgs).unwrap()
    );
}

/// H=5 over a growing history: one boundary move per 5 added rows, each
/// moving a 5-row batch (H=0 moves every row: 25 moves vs 5). The verbatim
/// window stays inside [COLLAPSE_KEEP, COLLAPSE_KEEP + H] throughout.
#[test]
fn collapse_hysteresis_moves_boundary_once_per_h_rows_in_one_batch() {
    let folds: Vec<(usize, usize)> = (1..=30)
        .map(|t| {
            (
                t,
                stub_boundary(&tool_history(t).raw_messages_with(&RunConfig::default(), 5)),
            )
        })
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
        .map(|t| stub_boundary(&tool_history(t).raw_messages_with(&RunConfig::default(), 0)))
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

/// Fold knobs come from `RunConfig`, never the environment: the same state
/// folds differently under different configs, and no env var is read.
#[test]
fn fold_knobs_come_from_run_config_not_the_environment() {
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
    let wide = RunConfig {
        thinking_keep: 99,
        collapse_hysteresis: 5,
        ..RunConfig::default()
    };
    // H=5 over 8 rows: boundary 5*((8-5)/5) = 0, no stubs; keep 99:
    // every thinking row survives.
    assert_eq!(stub_boundary(&s.derived_messages(&wide)), 0);
    let before = serde_json::to_string(&s.derived_messages(&wide)).unwrap();
    assert_eq!(before.matches("reason-").count(), 4);
    // Same state, tighter config: the fold moves with the config.
    let tight = RunConfig {
        thinking_keep: 0,
        collapse_hysteresis: 0,
        ..RunConfig::default()
    };
    let after = serde_json::to_string(&s.derived_messages(&tight)).unwrap();
    assert_ne!(after, before, "tighter config must move the fold");
    assert_eq!(stub_boundary(&s.derived_messages(&tight)), 3); // H=0 collapse-5
}

/// The config fields alone steer the fold: no env touched at all.
#[test]
fn fold_uses_config_knobs_not_the_environment() {
    let mut s = tool_history(8);
    let cfg = RunConfig {
        collapse_hysteresis: 5,
        ..RunConfig::default()
    };
    assert_eq!(stub_boundary(&s.derived_messages(&cfg)), 0);
    let cfg = RunConfig {
        collapse_hysteresis: 0,
        ..RunConfig::default()
    };
    assert_eq!(stub_boundary(&s.derived_messages(&cfg)), 3);
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
    let thinking = |s: &LoopState, cfg: &RunConfig| {
        s.derived_messages(cfg)
            .iter()
            .filter(|m| m.role == "assistant")
            .filter_map(|m| m.thinking.clone())
            .collect::<Vec<_>>()
    };
    let cfg = RunConfig {
        thinking_keep: 1,
        ..RunConfig::default()
    };
    assert_eq!(thinking(&s, &cfg), vec!["reason-3".to_string()]);
    let cfg = RunConfig {
        thinking_keep: 99,
        ..RunConfig::default()
    };
    assert_eq!(thinking(&s, &cfg).len(), 4);
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
    let msgs = s.derived_messages(&RunConfig::default());
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
    let msgs = s.derived_messages(&RunConfig::default());
    let head = &msgs[0].content;
    let prefix = "[collapsed: 202b — re-open to edit] ";
    assert!(head.starts_with(prefix), "{head}");
    assert_eq!(head.chars().count(), prefix.chars().count() + 120);
}

#[tokio::test]
async fn run_request_shape_history_delivered_once_collapse_5() {
    use agent_event::check_pairing;
    let root = run_tmp("shape");
    let order = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(Mutex::new(Vec::new()));
    // 7 write rounds, then a finishing text: the 8th request carries 7 tool
    // results, so collapse-5 has older material to shrink.
    let mut queue: VecDeque<Scripted> = VecDeque::new();
    for i in 1..=7 {
        let id = format!("c{i}");
        let path = format!("f{i}.txt");
        // Unique marker: the generic "step" would substring-match the
        // "steps N/M" budget tail on every request.
        let mut resp = response(
            vec![(
                id.as_str(),
                "write",
                serde_json::json!({"path": path, "content": "x\n"}),
            )],
            StopReason::ToolUse,
        );
        resp.message.content = format!("script-step-{i}");
        resp.message.thinking = Some(format!("reason-{i}"));
        queue.push_back(Scripted::Respond(resp));
    }
    queue.push_back(text_resp("all done"));
    // Held-declare round trip: the first done is held (unverified
    // writes), the second lands Done.
    queue.push_back(text_resp("done again"));
    let client = FakeLlm {
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

#[tokio::test]
async fn run_compaction_off_is_byte_identical_to_collapse_5() {
    use agent_event::check_pairing;
    // Trigger-ready: budget 8000, frac 0.1 (threshold 800), keep_tokens 3
    // and every round billed 1000 prompt tokens — every knob but `enabled`
    // is set to fire. Default `enabled: false` is the only thing holding.
    let mut queue: VecDeque<Scripted> = VecDeque::new();
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
