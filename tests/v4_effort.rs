// Top-level reasoning_effort (OpenAI-style): the knob Go honors.
#[test]
fn top_level_effort_present_when_set_absent_otherwise() {
    let c = rof::llm::effort_client_for_test(Some("medium".to_string()));
    let wire = rof::llm::wire_body_for_test(&c, "m");
    assert!(
        wire.contains("\"reasoning_effort\":\"medium\""),
        "top-level effort must be on the wire: {wire}"
    );
    let c2 = rof::llm::effort_client_for_test(None);
    let wire2 = rof::llm::wire_body_for_test(&c2, "m");
    assert!(
        !wire2.contains("reasoning_effort"),
        "unset effort must leave the wire byte-identical: {wire2}"
    );
}
