// tests/tui_render.rs
use rof::obs::TraceEvent;
use rof::tui::render::{parse_lenient_line, render_line, Counters};

#[test]
fn goal_and_verdict_render_as_transcript_lines() {
    let g = render_line(&TraceEvent::SessionStart {
        session_id: "s1".to_string(),
        goal: "fix it".to_string(),
    });
    assert!(g.contains("fix it"), "goal visible: {g}");
    let v = render_line(&TraceEvent::ReviewVerdict {
        pass: false,
        feedback: "prose, no writes".to_string(),
    });
    assert!(v.contains("fail"), "verdict visible: {v}");
}

#[test]
fn counters_fold_tokens_and_verdicts() {
    let evs = vec![
        TraceEvent::ModelCall {
            agent: "implementer".to_string(),
            model: "m".to_string(),
            input_tokens: 100,
            output_tokens: 20,
            latency_ms: 1,
            cost_usd: Some(0.001),
            cached_input_tokens: 0,
            attempts: 1,
        },
        TraceEvent::ReviewVerdict {
            pass: true,
            feedback: "ok".to_string(),
        },
    ];
    let c = Counters::fold(&evs);
    assert_eq!((c.model_calls, c.in_tokens, c.out_tokens), (1, 100, 20));
    assert_eq!((c.pass, c.fail), (1, 0));
}

#[test]
fn lenient_reader_never_fails() {
    let good = r#"{"SessionStart":{"session_id":"s","goal":"g"}}"#;
    assert!(parse_lenient_line(good).contains('g'));
    let future = r#"{"QuantumFlux":{"qubits":42}}"#;
    assert_eq!(parse_lenient_line(future), "· (unrecognized event)");
}
