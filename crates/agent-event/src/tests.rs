use super::*;
use serde_json::json;
use std::sync::{Arc, Mutex};

fn report() -> UsageReport {
    UsageReport {
        input_tokens: 10,
        output_tokens: 5,
        cache_read_tokens: 2,
        reasoning_tokens: Some(1),
        cost_usd: Some(0.01),
    }
}

fn all_variants() -> Vec<AgentEvent> {
    vec![
        AgentEvent::RunStart {
            run_id: 0,
            goal: "g".to_string(),
        },
        AgentEvent::RunEnd {
            outcome: RunOutcome::Passed,
            messages: vec![Message {
                role: Role::User,
                content: "hi".to_string(),
            }],
        },
        AgentEvent::TurnStart { turn: 0 },
        AgentEvent::TurnEnd {
            turn: 0,
            reason: TurnEndReason::Completed,
            usage_totals: report(),
        },
        AgentEvent::MessageStart {
            id: 0,
            role: Role::Assistant,
            partial: Message {
                role: Role::Assistant,
                content: String::new(),
            },
        },
        AgentEvent::MessageUpdate {
            id: 0,
            delta: MessageDelta {
                kind: DeltaKind::Text,
                text: Some("hi".to_string()),
            },
            partial: Message {
                role: Role::Assistant,
                content: "hi".to_string(),
            },
        },
        AgentEvent::MessageEnd {
            id: 0,
            message: Message {
                role: Role::Assistant,
                content: "hi".to_string(),
            },
            interrupted: false,
            usage: Some(report()),
        },
        AgentEvent::ToolStart {
            id: "c1".to_string(),
            name: "read".to_string(),
            args: json!({}),
        },
        AgentEvent::ToolEnd {
            id: "c1".to_string(),
            result: json!("ok"),
            is_error: false,
        },
        AgentEvent::Control(ControlAck {
            id: 1,
            kind: ControlKind::Stop,
            status: ControlStatus::Applied,
            note: "n".to_string(),
        }),
        AgentEvent::Error {
            error: AgentError {
                code: "E".to_string(),
                message: "m".to_string(),
            },
        },
    ]
}

#[test]
fn serde_round_trip_all_variants() {
    for ev in all_variants() {
        let s = serde_json::to_string(&ev).unwrap();
        assert!(
            s.contains("\"type\":"),
            "flat tagged enum must carry a type tag: {s}"
        );
        let back: AgentEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(ev, back);
    }
}

#[test]
fn pairing_invariant() {
    let mut good = vec![
        AgentEvent::RunStart {
            run_id: 0,
            goal: "g".to_string(),
        },
        AgentEvent::TurnStart { turn: 0 },
        AgentEvent::MessageStart {
            id: 0,
            role: Role::Assistant,
            partial: Message {
                role: Role::Assistant,
                content: "hi".to_string(),
            },
        },
        AgentEvent::MessageEnd {
            id: 0,
            message: Message {
                role: Role::Assistant,
                content: "hi".to_string(),
            },
            interrupted: false,
            usage: None,
        },
        AgentEvent::ToolStart {
            id: "c1".to_string(),
            name: "read".to_string(),
            args: json!({}),
        },
        AgentEvent::ToolEnd {
            id: "c1".to_string(),
            result: json!("ok"),
            is_error: false,
        },
        AgentEvent::TurnEnd {
            turn: 0,
            reason: TurnEndReason::Completed,
            usage_totals: UsageReport::default(),
        },
        AgentEvent::RunEnd {
            outcome: RunOutcome::Passed,
            messages: vec![],
        },
        AgentEvent::Control(ControlAck {
            id: 1,
            kind: ControlKind::Stop,
            status: ControlStatus::Applied,
            note: "n".to_string(),
        }),
        AgentEvent::Error {
            error: AgentError {
                code: "E".to_string(),
                message: "m".to_string(),
            },
        },
    ];
    assert!(check_pairing(&good));
    // One-directional: an End without a Start passes.
    assert!(check_pairing(&[AgentEvent::MessageEnd {
        id: 9,
        message: Message {
            role: Role::Assistant,
            content: String::new()
        },
        interrupted: true,
        usage: None,
    }]));
    good.remove(7);
    assert!(!check_pairing(&good));
}

#[test]
fn listener_isolation_and_bounded_tap() {
    let mut bus = Emitter::new();
    let seen = Arc::new(Mutex::new(0u32));
    let s = seen.clone();
    bus.on(move |_| {
        *s.lock().unwrap() += 1;
    });
    bus.on(|_| panic!("boom"));
    let rx = bus.tap(8);
    bus.emit(AgentEvent::TurnStart { turn: 1 });
    // The panicking listener became one Error event; the guard held (no loop).
    assert_eq!(bus.history().len(), 2);
    assert!(matches!(bus.history()[1], AgentEvent::Error { .. }));
    assert_eq!(*seen.lock().unwrap(), 2);
    assert!(matches!(
        rx.try_recv().unwrap(),
        AgentEvent::TurnStart { .. }
    ));
    assert!(matches!(rx.try_recv().unwrap(), AgentEvent::Error { .. }));
    // A dead tap never errors the loop.
    drop(rx);
    bus.emit(AgentEvent::TurnEnd {
        turn: 1,
        reason: TurnEndReason::Aborted,
        usage_totals: UsageReport::default(),
    });
    assert_eq!(bus.history().len(), 4);
    assert_eq!(*seen.lock().unwrap(), 4);
}

#[test]
fn pre_usage_dump_lines_still_deserialize() {
    // Dumps written before the usage fields keep parsing: `usage` is an
    // Option (serde fills None) and `usage_totals` carries serde(default).
    let end: AgentEvent = serde_json::from_str(
        r#"{"type":"MessageEnd","id":1,"message":{"role":"Assistant","content":"hi"},"interrupted":false}"#,
    )
    .unwrap();
    assert!(matches!(end, AgentEvent::MessageEnd { usage: None, .. }));
    let turn: AgentEvent =
        serde_json::from_str(r#"{"type":"TurnEnd","turn":1,"reason":"Completed"}"#).unwrap();
    let AgentEvent::TurnEnd { usage_totals, .. } = turn else {
        panic!("expected TurnEnd");
    };
    assert_eq!(usage_totals, UsageReport::default());
}
