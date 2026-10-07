use crate::types::Usage;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Fatal auth misconfiguration: missing/empty key, or a 401/403 the
    /// credential source cannot refresh. Never retried.
    Auth(String),
    /// One non-2xx HTTP round trip, with the server's retry hint and a
    /// truncated body for classification.
    Http {
        status: u16,
        retry_after: Option<Duration>,
        body: String,
    },
    Cancelled,
    AllFailed(String),
    /// A failure that still carried billed usage (truncated 200 body,
    /// parseable non-2xx body). `source` is the typed failure the ladder
    /// gave up on; `usage` sums every failed attempt inside one `complete`
    /// call. None = the failure reported nothing: never a fabricated zero,
    /// so the metering loop can tell "unreported" from "free".
    /// `exhausted` marks a retryable ladder that ran out of rungs: the
    /// outer loop must not re-run it. Non-retryable failures report false.
    Metered {
        source: Box<LlmError>,
        usage: Option<Usage>,
        exhausted: bool,
    },
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Transport(m) | LlmError::Auth(m) | LlmError::AllFailed(m) => {
                write!(f, "{m}")
            }
            LlmError::Http { status, body, .. } => write!(f, "{status}: {body}"),
            LlmError::Cancelled => write!(f, "cancelled"),
            LlmError::Metered { source, .. } => write!(f, "{source}"),
        }
    }
}

/// Typed-first classification: an HTTP status decides before any body
/// text. The string table below runs only for errors that carry no status
/// (transport failures and gateways that ship none).
pub fn classify_error(e: &LlmError) -> ErrorClass {
    match e {
        LlmError::Cancelled | LlmError::Auth(_) => ErrorClass::Fatal,
        LlmError::AllFailed(_) => ErrorClass::Retryable,
        LlmError::Transport(msg) => classify_message(msg),
        LlmError::Http { status, body, .. } => classify_http(*status, body),
        LlmError::Metered { source, .. } => classify_error(source),
    }
}

/// Status-first table: overflow text wins (a 400 can carry it), 401/403 are
/// fatal even when the body cries stale (staleness retries only via a
/// refreshable source, which never surfaces as `Http`), overload text wins
/// over a bare 429, then the retryable statuses, then the body table.
fn classify_http(status: u16, body: &str) -> ErrorClass {
    let t = body.to_lowercase();
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
        return ErrorClass::ContextOverflow;
    }
    if status == 401 || status == 403 {
        return ErrorClass::Fatal;
    }
    if has(&[
        "server_is_overloaded",
        "slow_down",
        "server overloaded",
        "overloaded",
    ]) {
        return ErrorClass::Overload;
    }
    if status == 429 || status == 402 || (500..=599).contains(&status) {
        return ErrorClass::Retryable;
    }
    classify_message(body)
}

/// Fails-open string table (blacklist, not allowlist): unknown errors retry.
/// ContextOverflow never retries at the same shape; 402 retries (OpenRouter
/// sends it on transient credit/queue states).
fn classify_message(s: &str) -> ErrorClass {
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
