use crate::request::build_request;
use crate::verify::VERIFY_NUDGE;
use crate::*;
use agent_event::Emitter;
use provider_core::StopReason;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use super::{run_registry, run_tmp, script_resp, text_resp, FakeLlm};

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

/// Prefix-cache stability end to end: the system head is byte-identical
/// across requests and frozen at run start, and the only per-request bytes
/// are the live budgets on the final message.
#[tokio::test]
async fn run_system_prefix_is_static_and_file_map_frozen() {
    let root = run_tmp("prefix");
    std::fs::write(root.join("a.rs"), "v1\n").unwrap();
    let client = FakeLlm {
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
