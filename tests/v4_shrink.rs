// Shrink-and-retry: when a truncation shipped zero content (reasoning ate the
// whole budget), the spent ladder gets one final rung that HALVES max_tokens
// with reasoning back on — reasoning expands to fill any budget, so a smaller
// budget forces a shorter pass. bug02 shape: 32-34k reasoning chars, 0 content.
use rof::llm::{base_req_for_test, reshape_for_truncation_for_test};

#[test]
fn contentless_truncation_walks_to_shrink_then_stops() {
    let mut req = base_req_for_test(8192);
    // Rungs 1-3: low -> thinking_off -> reasoning_off (existing behavior).
    assert!(reshape_for_truncation_for_test(&mut req, 0));
    assert!(req.reasoning_low);
    assert!(reshape_for_truncation_for_test(&mut req, 0));
    assert!(req.thinking_off);
    assert!(reshape_for_truncation_for_test(&mut req, 0));
    assert!(req.reasoning_off);
    // Rung 4: roomier is refused (content == 0), shrink fires instead.
    // reasoning_off stays on so the ladder is terminal afterwards.
    assert!(reshape_for_truncation_for_test(&mut req, 0));
    assert!(req.shrunk);
    assert_eq!(req.max_tokens, 4096);
    assert!(!req.reasoning_low && !req.thinking_off && req.reasoning_off);
    // Rung 5: everything spent -> stop, no guaranteed-empty call.
    assert!(!reshape_for_truncation_for_test(&mut req, 0));
    assert_eq!(req.max_tokens, 4096);
}

#[test]
fn shrink_fires_once_and_respects_the_floor() {
    // Content present: roomier still wins (existing behavior preserved).
    let mut req = base_req_for_test(8192);
    req.reasoning_low = true;
    req.thinking_off = true;
    req.reasoning_off = true;
    assert!(reshape_for_truncation_for_test(&mut req, 500));
    assert!(!req.shrunk);
    assert_eq!(req.max_tokens, 16384);
    // At the floor with no content: no no-op shrink, stop instead.
    let mut small = base_req_for_test(1024);
    small.reasoning_low = true;
    small.thinking_off = true;
    small.reasoning_off = true;
    assert!(!reshape_for_truncation_for_test(&mut small, 0));
    assert_eq!(small.max_tokens, 1024);
}
