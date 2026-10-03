//! LLM boundary vocabulary. See research/crate-provider-core.md.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Thinking {
    Off,
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderMessage {
    pub role: String,
    pub content: String,
    /// Assistant tool calls to echo back (strict providers 422 without them).
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRef>,
    /// Tool-result linkage id (strict providers 422 without it).
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// Assistant thinking the endpoint demands echoed back (DeepSeek thinking
    /// mode 400s without `reasoning_content` on prior assistant messages).
    /// None = the provider sent none; never fabricated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub messages: Vec<ProviderMessage>,
    pub tools: Vec<tool_core::ToolDeclaration>,
    pub max_tokens: usize,
    pub thinking: Thinking,
    pub extras: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    Pending,
    Stop,
    ToolUse,
    MaxTokens,
    Refused,
    Error,
    Aborted,
    Deferred,
}

impl StopReason {
    /// Everything but `Pending` is terminal.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, StopReason::Pending)
    }
    /// Terminal-with-error. Refused counts; MaxTokens does not (it is a
    /// reshape-and-retry signal, not a failure). Deferred is type-only.
    pub fn is_error(&self) -> bool {
        matches!(
            self,
            StopReason::Refused | StopReason::Error | StopReason::Aborted
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Subset of output, not additional. None = provider doesn't report.
    pub reasoning: Option<u64>,
    pub cost_usd: Option<f64>,
}

impl Usage {
    /// Billed total. Reasoning is already inside `output`: never add it again.
    pub fn total_tokens(&self) -> u64 {
        self.input + self.output
    }
    /// Output minus reasoning (the visible content share). Saturates: a
    /// provider that over-reports reasoning yields 0, never underflow.
    pub fn content_tokens(&self) -> u64 {
        self.output
            .saturating_sub(self.reasoning.unwrap_or(0).min(self.output))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRef {
    pub id: String,
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub content: String,
    pub tool_calls: Vec<ToolCallRef>,
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub message: AssistantMessage,
    pub stop: StopReason,
    pub usage: Usage,
    pub latency_ms: u64,
    pub attempts: u64,
    pub raw_stop_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Capabilities {}

#[derive(Debug, Clone)]
pub struct Credentials {
    pub api_key: String,
}

#[derive(Debug, Clone)]
pub enum ErrorClass {
    Retryable,
    Overload,
    AuthStale,
    ContextOverflow,
    Fatal,
}

#[derive(Debug, Clone)]
pub enum LlmError {
    Transport(String),
    AllFailed(String),
    /// A failure that still carried billed usage (truncated 200 body,
    /// parseable non-2xx body). `source` is the message `Transport` would
    /// carry; `usage` sums every failed attempt inside one `complete` call.
    /// None = the failure reported nothing: never a fabricated zero, so the
    /// metering loop can tell "unreported" from "free".
    Metered {
        source: String,
        usage: Option<Usage>,
    },
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    /// One logical request. Retry, backoff, and reshaping live in the
    /// implementor. The default fails loudly instead of fabricating zeros
    /// (the budget meters from `Usage`; zeros would be unlimited free
    /// tokens). Adapters must override with a metered implementation.
    async fn complete(&self, _model: &str, _req: &Request) -> Result<Response, LlmError> {
        Err(LlmError::Transport(
            "default LlmClient::complete cannot report usage: override it with a metered implementation".into(),
        ))
    }
    /// Capabilities negotiated per call; shapes the request from the answer.
    fn capabilities(&self, model: &str) -> Capabilities;
    /// Per-call credential resolve, not once-at-startup: tokens expire mid-run.
    /// Err only on fatal auth misconfiguration; staleness is retryable.
    async fn resolve_key(&self, provider: &str) -> Result<Credentials, LlmError>;
}

/// Map a provider finish reason to `StopReason`. `None`/empty/unknown means
/// the provider sent nothing: infer from tool calls (non-empty -> ToolUse).
/// A length/max-tokens reason is surfaced as `MaxTokens` even when tool calls
/// are present — never coerced to `ToolUse`, or the loop would execute
/// truncated arguments.
pub fn infer_stop(raw: Option<&str>, message: &AssistantMessage) -> StopReason {
    let infer = || {
        if message.tool_calls.is_empty() {
            StopReason::Stop
        } else {
            StopReason::ToolUse
        }
    };
    let r = match raw.map(str::trim) {
        Some(s) if !s.is_empty() => s.to_lowercase(),
        _ => return infer(),
    };
    let n: String = r.chars().filter(|c| c.is_alphanumeric()).collect();
    match n.as_str() {
        "length" | "maxtokens" | "truncated" | "outputlimit" => StopReason::MaxTokens,
        s if s.contains("tool") || s.contains("function") => StopReason::ToolUse,
        s if s.contains("refus")
            || s.contains("contentfilter")
            || s.contains("safety")
            || s.contains("policy") =>
        {
            StopReason::Refused
        }
        s if s.contains("defer") => StopReason::Deferred,
        s if s.contains("abort") || s.contains("cancel") => StopReason::Aborted,
        s if s.contains("error") || s.contains("fail") => StopReason::Error,
        s if s.contains("pending") => StopReason::Pending,
        "stop" | "endturn" | "stopsequence" | "eos" | "done" | "complete" | "finished" => {
            StopReason::Stop
        }
        _ => infer(),
    }
}

/// Fails-open classification (blacklist, not allowlist): unknown errors retry.
/// ContextOverflow never retries at the same shape; 402 retries (OpenRouter
/// sends it on transient credit/queue states).
pub fn classify_error(s: &str) -> ErrorClass {
    let t = s.to_lowercase();
    let has = |p: &[&str]| p.iter().any(|k| t.contains(k));
    if has(&[
        "context_length_exceeded",
        "context_overflow",
        "context overflow",
        "context window",
        "prompt too long",
        "input too long",
        "too many tokens",
        "out of context",
        "token limit",
    ]) {
        ErrorClass::ContextOverflow
    } else if has(&[
        "server_is_overloaded",
        "slow_down",
        "server overloaded",
        "overloaded",
    ]) {
        ErrorClass::Overload
    } else if has(&["expired", "stale"]) {
        ErrorClass::AuthStale
    } else if t.contains("402") {
        ErrorClass::Retryable
    } else if has(&[
        "invalid_prompt",
        "invalid_api_key",
        "invalid_token",
        "authentication_error",
        "permission_error",
        "insufficient_quota",
        "usage_limit_reached",
        "credit_balance_exhausted",
        "billing_hard_limit",
        "unauthorized",
        "forbidden",
        " 401",
        " 403",
        "(401",
        "(403",
        "account_disabled",
    ]) {
        ErrorClass::Fatal
    } else {
        ErrorClass::Retryable
    }
}

/// `Retry-After` in both encodings: delta-seconds, or an HTTP-date
/// (IMF-fixdate) differenced against now. Past dates yield zero, not `None`.
pub fn parse_retry_after(header: &str) -> Option<Duration> {
    let h = header.trim();
    if let Ok(s) = h.parse::<u64>() {
        return Some(Duration::from_secs(s));
    }
    let epoch = parse_http_date(h)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(Duration::from_secs(epoch.saturating_sub(now)))
}

fn month_num(m: &str) -> Option<i64> {
    match m {
        "Jan" | "jan" => Some(1),
        "Feb" | "feb" => Some(2),
        "Mar" | "mar" => Some(3),
        "Apr" | "apr" => Some(4),
        "May" | "may" => Some(5),
        "Jun" | "jun" => Some(6),
        "Jul" | "jul" => Some(7),
        "Aug" | "aug" => Some(8),
        "Sep" | "sep" => Some(9),
        "Oct" | "oct" => Some(10),
        "Nov" | "nov" => Some(11),
        "Dec" | "dec" => Some(12),
        _ => None,
    }
}

/// IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) -> unix seconds. stdlib only.
fn parse_http_date(s: &str) -> Option<u64> {
    let mut parts: Vec<&str> = s.split_whitespace().collect();
    if parts.first().is_some_and(|p| p.ends_with(',')) {
        parts.remove(0);
    }
    let [day, mon, year, clock, tz] = parts.as_slice() else {
        return None;
    };
    if !tz.eq_ignore_ascii_case("GMT") {
        return None;
    }
    let d: i64 = day.parse().ok()?;
    let m = month_num(mon)?;
    let y: i64 = year.parse().ok()?;
    let t: Vec<&str> = clock.split(':').collect();
    let [hh, mm, ss] = t.as_slice() else {
        return None;
    };
    let (hh, mm, ss): (i64, i64, i64) = (hh.parse().ok()?, mm.parse().ok()?, ss.parse().ok()?);
    // days_from_civil (Hinnant), valid for the whole unix range.
    let mut y2 = y;
    y2 -= i64::from(m <= 2);
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(days * 86400 + hh * 3600 + mm * 60 + ss).ok()
}

/// Streaming-JSON salvage: valid parse -> repaired parse -> partial ->
/// partial-of-repaired -> `{}`. Truncated tool args must still decode, and a
/// hopeless payload must not throw: it yields an empty object while the
/// `MaxTokens` guard (not the parser) blocks execution.
pub fn parse_streaming_json(s: &str) -> Value {
    if let Ok(v) = serde_json::from_str(s) {
        return v;
    }
    let repaired = repair_json(s);
    if let Ok(v) = serde_json::from_str(&repaired) {
        return v;
    }
    if let Some(v) = partial_parse(s) {
        return v;
    }
    if let Some(v) = partial_parse(&repaired) {
        return v;
    }
    Value::Object(serde_json::Map::new())
}

/// Escape raw control chars inside strings; double backslashes that precede
/// an invalid escape (`\p` -> `\\p`) so a path never kills the parse.
fn repair_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut in_str = false;
    let mut esc = false;
    while let Some(c) = chars.next() {
        if in_str {
            if esc {
                out.push(c);
                esc = false;
                continue;
            }
            if c == '\\' {
                match chars.peek() {
                    Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') => {
                        out.push('\\');
                        esc = true;
                    }
                    _ => out.push_str("\\\\"),
                }
                continue;
            }
            match c {
                '"' => {
                    in_str = false;
                    out.push(c);
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                }
                _ => out.push(c),
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else {
            out.push(c);
        }
    }
    out
}

/// Close open braces/brackets/strings, drop dangling `,`/`:` first. Then one
/// retry with a trailing partial literal stripped (`{"a": tru` -> `{"a"` is
/// still unparseable, so this only rescues values cut at a clean boundary).
fn partial_parse(s: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(&close_truncated(s)) {
        return Some(v);
    }
    let t = s.trim_end();
    let cut = t
        .trim_end_matches(|c: char| c.is_alphanumeric() || matches!(c, '.' | '+' | '-' | '_'))
        .trim_end()
        .trim_end_matches(',')
        .trim_end_matches(':')
        .trim_end();
    if cut.len() < t.len() {
        serde_json::from_str::<Value>(&close_truncated(cut)).ok()
    } else {
        None
    }
}

fn close_truncated(s: &str) -> String {
    let mut stack: Vec<char> = Vec::new();
    let mut in_str = false;
    let mut esc = false;
    for c in s.chars() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
        } else if c == '{' {
            stack.push('}');
        } else if c == '[' {
            stack.push(']');
        } else if (c == '}' || c == ']') && stack.last() == Some(&c) {
            stack.pop();
        }
    }
    let mut out = s.trim_end().to_string();
    loop {
        let t = out.trim_end();
        if t.ends_with(',') || t.ends_with(':') {
            out = t[..t.len() - 1].to_string();
        } else {
            out = t.to_string();
            break;
        }
    }
    if in_str {
        out.push('"');
    }
    while let Some(c) = stack.pop() {
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    fn class_of(s: &str) -> &'static str {
        match classify_error(s) {
            ErrorClass::Retryable => "retry",
            ErrorClass::Overload => "overload",
            ErrorClass::AuthStale => "stale",
            ErrorClass::ContextOverflow => "overflow",
            ErrorClass::Fatal => "fatal",
        }
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
            assert_eq!(class_of(input), want, "input: {input}");
        }
    }
    #[test]
    fn classifier_402_is_retryable() {
        assert_eq!(class_of("402 Payment Required: queue full"), "retry");
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

    /// Adapter that implements no methods but the keys: exactly who would
    /// inherit the default `complete`.
    struct KeyOnlyClient;
    #[async_trait]
    impl LlmClient for KeyOnlyClient {
        fn capabilities(&self, _model: &str) -> Capabilities {
            Capabilities {}
        }
        async fn resolve_key(&self, _provider: &str) -> Result<Credentials, LlmError> {
            Err(LlmError::Transport("no key in test".into()))
        }
    }

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
}
