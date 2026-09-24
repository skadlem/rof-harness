// Responses transport (Muse via .../responses): wire parsing without network.
// Canned shapes verified against live `zen/go/v1/responses` samples.
use rof::llm::{responses_parse_for_test, uses_responses_for_test};

const COMPLETED: &str = r#"{
  "status": "completed", "error": null,
  "output": [
    {"type": "reasoning", "status": "completed"},
    {"type": "message", "content": [{"type": "output_text", "text": "alive"}]}
  ],
  "usage": {"input_tokens": 12, "output_tokens": 60,
    "input_tokens_details": {"cached_tokens": 0},
    "output_tokens_details": {"reasoning_tokens": 47}}
}"#;

const INCOMPLETE_EMPTY: &str = r#"{
  "status": "incomplete", "error": null,
  "incomplete_details": {"reason": "max_output_tokens"},
  "output": [{"type": "reasoning", "status": "in_progress"}],
  "usage": {"input_tokens": 12, "output_tokens": 50,
    "output_tokens_details": {"reasoning_tokens": 47}}
}"#;

const FAILED: &str = r#"{
  "status": "failed", "error": {"code": "server_error", "message": "boom"},
  "output": [], "usage": {"input_tokens": 1, "output_tokens": 0}
}"#;

#[test]
fn responses_routing_is_model_prefixed() {
    assert!(uses_responses_for_test("muse-spark-1.3-contributor"));
    assert!(!uses_responses_for_test("deepseek-flash"));
    assert!(!uses_responses_for_test("deepseek-chat"));
}

#[test]
fn responses_completed_extracts_text_and_usage() {
    let body: serde_json::Value = serde_json::from_str(COMPLETED).unwrap();
    let (text, inp, out, cached, reasoning) =
        responses_parse_for_test(&body, 8192).expect("completed parses");
    assert_eq!(text, "alive");
    assert_eq!((inp, out, cached, reasoning), (12, 60, 0, 47));
}

#[test]
fn responses_incomplete_empty_reads_as_truncation() {
    let body: serde_json::Value = serde_json::from_str(INCOMPLETE_EMPTY).unwrap();
    let err = responses_parse_for_test(&body, 8192).expect_err("must be an error");
    let msg = err.to_string();
    // Same vocabulary the ladder classifies: truncation + reasoning signal.
    // `finish_reason=length` is the token `complete()` matches on — without
    // it the truncation misclassifies as non-retryable.
    assert!(
        msg.contains("output truncated at 8192 tokens"),
        "ladder vocab: {msg}"
    );
    assert!(msg.contains("finish_reason=length"), "ladder token: {msg}");
    assert!(
        msg.contains("reasoning_content=47 chars"),
        "budget signal: {msg}"
    );
}

#[test]
fn responses_failed_surfaces_transport_error() {
    let body: serde_json::Value = serde_json::from_str(FAILED).unwrap();
    let err = responses_parse_for_test(&body, 8192).expect_err("must be an error");
    assert!(
        err.to_string().contains("boom"),
        "server message kept: {err}"
    );
}
