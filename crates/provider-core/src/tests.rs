use super::*;
use async_trait::async_trait;
use serde_json::json;
use std::time::Duration;

#[test]
fn billed_usage_folds_failed_attempts_into_the_bill() {
    let usage = |i: u64, o: u64, cost: Option<f64>| Usage {
        input: i,
        output: o,
        cache_read: 0,
        cache_write: 0,
        reasoning: None,
        cost_usd: cost,
    };
    let mut r = Response {
        message: msg_with_tools(),
        stop: StopReason::Stop,
        usage: usage(100, 50, Some(0.01)),
        latency_ms: 0,
        attempts: 1,
        raw_stop_reason: None,
        retry_usage: None,
    };
    assert_eq!(r.billed_usage().input, 100, "no retries: identical");
    r.retry_usage = Some(Box::new(usage(400, 10, Some(0.02))));
    let b = r.billed_usage();
    assert_eq!((b.input, b.output), (500, 60));
    assert!((b.cost_usd.unwrap() - 0.03).abs() < 1e-12);
}

#[test]
fn plus_keeps_reasoning_subset_and_known_cost() {
    let a = Usage {
        input: 1,
        output: 10,
        cache_read: 2,
        cache_write: 0,
        reasoning: Some(4),
        cost_usd: None,
    };
    let b = Usage {
        input: 1,
        output: 10,
        cache_read: 2,
        cache_write: 0,
        reasoning: Some(3),
        cost_usd: Some(0.5),
    };
    let s = a.plus(&b);
    assert_eq!((s.input, s.output, s.cache_read), (2, 20, 4));
    assert_eq!(s.reasoning, Some(7));
    assert_eq!(s.cost_usd, Some(0.5), "known side wins over unknown");
}

fn msg_with_tools() -> AssistantMessage {
    AssistantMessage {
        content: String::new(),
        tool_calls: vec![ToolCallRef {
            id: "1".into(),
            name: "read".into(),
            args: json!({}),
        }],
        thinking: None,
    }
}
fn msg_plain() -> AssistantMessage {
    AssistantMessage {
        content: "hi".into(),
        tool_calls: vec![],
        thinking: None,
    }
}

#[test]
fn provider_message_thinking_absent_parses_and_serializes_away() {
    // Rows written before the field existed (and None-thinking rows) must
    // keep parsing, and None must not invent a `thinking` key.
    let m: ProviderMessage = serde_json::from_value(json!({
        "role": "assistant", "content": "x", "tool_calls": [], "tool_call_id": null
    }))
    .unwrap();
    assert_eq!(m.thinking, None);
    assert!(serde_json::to_value(&m).unwrap().get("thinking").is_none());
}

#[test]
fn salvage_valid_json_passes_through() {
    assert_eq!(parse_streaming_json(r#"{"a": 1}"#), json!({"a": 1}));
}
#[test]
fn salvage_repairs_raw_control_chars() {
    let v = parse_streaming_json("{\"a\": \"x\ny\"}");
    assert_eq!(v, json!({"a": "x\ny"}));
}
#[test]
fn salvage_repairs_invalid_escapes() {
    let v = parse_streaming_json(r#"{"a": "C:\path"}"#);
    assert_eq!(v, json!({"a": r"C:\path"}));
}
#[test]
fn salvage_closes_truncated_object() {
    let v = parse_streaming_json(r#"{"a": 1, "b": "xy"#);
    assert_eq!(v, json!({"a": 1, "b": "xy"}));
}
#[test]
fn salvage_partial_after_repair() {
    // Truncated AND bad escape: only partial-of-repaired decodes.
    let v = parse_streaming_json("{\"a\": \"C:\\pat");
    assert_eq!(v, json!({"a": r"C:\pat"}));
}
#[test]
fn salvage_total_failure_yields_empty_object() {
    for bad in ["", "not json at all", "{{{{", "{\"a\": tru"] {
        assert_eq!(parse_streaming_json(bad), json!({}), "input: {bad}");
    }
}

fn class_of(e: &LlmError) -> &'static str {
    match classify_error(e) {
        ErrorClass::Retryable => "retry",
        ErrorClass::Overload => "overload",
        ErrorClass::AuthStale => "stale",
        ErrorClass::ContextOverflow => "overflow",
        ErrorClass::Fatal => "fatal",
    }
}
fn class_of_str(s: &str) -> &'static str {
    class_of(&LlmError::Transport(s.into()))
}
#[test]
fn classifier_table() {
    let cases = [
        ("429 Too Many Requests", "retry"),
        ("500 Internal Server Error", "retry"),
        ("weird novel provider wobble", "retry"),
        ("context_length_exceeded: too many tokens", "overflow"),
        ("prompt is too long for context window", "overflow"),
        ("server_is_overloaded, try again", "overload"),
        ("client slow_down, back off", "overload"),
        ("token expired, refresh needed", "stale"),
        ("invalid_api_key rejected", "fatal"),
        ("request forbidden (403)", "fatal"),
        ("insufficient_quota: billing hard limit", "fatal"),
    ];
    for (input, want) in cases {
        assert_eq!(class_of_str(input), want, "input: {input}");
    }
}
#[test]
fn classifier_402_is_retryable() {
    assert_eq!(class_of_str("402 Payment Required: queue full"), "retry");
}
#[test]
fn classifier_status_first_then_string_fallback() {
    let http = |status: u16, body: &str| LlmError::Http {
        status,
        retry_after: None,
        body: body.into(),
    };
    // 429 with a Retry-After hint and the 503 cold-start shape retry.
    assert_eq!(class_of(&http(429, "rate limited")), "retry");
    assert_eq!(class_of(&http(503, "Service Unavailable")), "retry");
    // Status wins over stale text: a 401 that cries expired is still fatal.
    assert_eq!(
        class_of(&http(401, "token expired, refresh needed")),
        "fatal"
    );
    assert_eq!(class_of(&http(403, "forbidden")), "fatal");
    // Overflow text wins over a bare status.
    assert_eq!(
        class_of(&http(400, "context_length_exceeded: too many tokens")),
        "overflow"
    );
    // Auth and Cancelled are always fatal.
    assert_eq!(class_of(&LlmError::Auth("missing key".into())), "fatal");
    assert_eq!(class_of(&LlmError::Cancelled), "fatal");
    // Metered unwraps to its typed source.
    assert_eq!(
        class_of(&LlmError::Metered {
            source: Box::new(http(429, "slow down")),
            usage: None,
            exhausted: true,
        }),
        "retry"
    );
}

#[test]
fn max_tokens_surfaced_not_swallowed() {
    assert_eq!(
        infer_stop(Some("length"), &msg_with_tools()),
        StopReason::MaxTokens
    );
}
#[test]
fn reasoning_is_subset_never_double_counted() {
    let u = Usage {
        input: 10,
        output: 100,
        cache_read: 0,
        cache_write: 0,
        reasoning: Some(30),
        cost_usd: None,
    };
    assert_eq!(u.total_tokens(), 110);
    assert_eq!(u.content_tokens(), 70);
    let over = Usage {
        reasoning: Some(999),
        ..u.clone()
    };
    assert_eq!(over.content_tokens(), 0);
}
#[test]
fn absent_finish_reason_infers_stop_vs_tool_use() {
    assert_eq!(infer_stop(None, &msg_plain()), StopReason::Stop);
    assert_eq!(infer_stop(None, &msg_with_tools()), StopReason::ToolUse);
    assert_eq!(infer_stop(Some(""), &msg_with_tools()), StopReason::ToolUse);
    assert_eq!(
        infer_stop(Some("mystery-finish-v9"), &msg_plain()),
        StopReason::Stop
    );
    assert_eq!(
        infer_stop(Some("tool_calls"), &msg_with_tools()),
        StopReason::ToolUse
    );
}
#[test]
fn refused_is_terminal_error_distinct_from_max_tokens() {
    assert!(StopReason::Refused.is_terminal() && StopReason::Refused.is_error());
    assert!(StopReason::MaxTokens.is_terminal() && !StopReason::MaxTokens.is_error());
    assert!(!StopReason::Pending.is_terminal());
}

/// Adapter that implements nothing: exactly who would inherit the
/// default `complete`.
struct KeyOnlyClient;
#[async_trait]
impl LlmClient for KeyOnlyClient {}

#[test]
fn default_complete_fails_loud_instead_of_zero_usage_success() {
    let req = Request {
        messages: vec![],
        tools: vec![],
        max_tokens: 32,
        thinking: Thinking::Off,
        extras: json!({}),
    };
    let got = futures::executor::block_on(KeyOnlyClient.complete("m", &req));
    match got {
        Err(LlmError::Transport(m)) => assert!(m.contains("usage"), "msg: {m}"),
        Err(other) => panic!("wrong error variant: {other:?}"),
        Ok(r) => panic!("fabricated success with zero usage: {:?}", r.usage),
    }
}

#[test]
fn retry_after_seconds() {
    assert_eq!(parse_retry_after("120"), Some(Duration::from_secs(120)));
    assert_eq!(parse_retry_after("garbage"), None);
}
#[test]
fn retry_after_http_date() {
    // Far-future fixed date: must be a large positive delay.
    let d = parse_retry_after("Sun, 06 Nov 2034 08:49:37 GMT").unwrap();
    assert!(d.as_secs() > 200_000_000, "got {d:?}");
    // Past date: zero, not None.
    assert_eq!(
        parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT"),
        Some(Duration::ZERO)
    );
}
