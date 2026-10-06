use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use provider_core::{Request, Usage};

use crate::client::EndpointProfile;

#[derive(Debug, Clone)]
pub(crate) struct WireKnobs {
    pub(crate) max_tokens: usize,
    /// Endpoint's thinking-off fragment, if it has a real one.
    pub(crate) thinking_off: Option<serde_json::Value>,
    pub(crate) thinking_suppressed: bool,
}

impl WireKnobs {
    pub(crate) fn from_req(req: &Request, profile: &EndpointProfile) -> Self {
        // Thinking::Off applies the endpoint's real suppression knob up front;
        // the ladder then only has token-raising left for that call.
        let thinking_off = profile.thinking_off.clone();
        let thinking_suppressed =
            matches!(req.thinking, provider_core::Thinking::Off) && thinking_off.is_some();
        Self {
            max_tokens: req.max_tokens,
            thinking_off,
            thinking_suppressed,
        }
    }
}

/// Ceiling the overflow ladder may raise `max_tokens` to. Chosen (no anchor):
/// bounds one ladder's spend; the ladder unit test pins the bound.
pub(crate) const MAX_TOKENS_CEILING: usize = 32_768;

/// Cheapest-first reshape for one truncated (`finish_reason=length`) attempt.
/// Overflow needs room, never less of it: (1) thinking off — reasoning ate
/// the whole budget (content=0, reasoning>0) and the endpoint has a real
/// knob, applied once; (2) raise `max_tokens` — double, ceiling-bounded.
/// Returns whether a reshape applied; false ends the ladder.
pub(crate) fn apply_ladder(
    k: &mut WireKnobs,
    content_chars: usize,
    reasoning_chars: usize,
) -> bool {
    if !k.thinking_suppressed
        && content_chars == 0
        && reasoning_chars > 0
        && k.thinking_off.is_some()
    {
        k.thinking_suppressed = true;
        return true;
    }
    let raised = k.max_tokens.saturating_mul(2).min(MAX_TOKENS_CEILING);
    if raised > k.max_tokens {
        k.max_tokens = raised;
        return true;
    }
    false
}

/// Fold a failed attempt's usage into the running total. Attempts are
/// independent requests, so billed tokens sum (`Usage::plus`).
pub(crate) fn merge_usage(acc: Option<Usage>, next: Option<Usage>) -> Option<Usage> {
    match (acc, next) {
        (Some(a), Some(n)) => Some(a.plus(&n)),
        (a, n) => a.or(n),
    }
}

/// Cap on a server-requested `Retry-After` delay. Pi fails fast above its
/// 60 s `maxRetryDelayMs` default (`provider-retry.ts:44-58`): a longer ask is
/// terminal, never slept on.
const RETRY_AFTER_CAP: Duration = Duration::from_secs(60);

/// Process-local draw counter, mixed into the clock stamp so two draws in the
/// same clock tick (or on a coarse clock) still differ.
static JITTER_SEQ: AtomicU64 = AtomicU64::new(0);

/// 0–25% multiplicative jitter for the exponential rungs
/// (pi `provider-retry.ts:66`). Entropy: wall-clock nanoseconds at retry time
/// (parallel workers reach a retry at distinct instants) mixed with a
/// process-local draw counter; splitmix64 finalizer so coarse clock granularity
/// cannot bias the low bits. No `rand` dependency.
fn backoff_jitter_factor() -> f64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() ^ u64::from(d.subsec_nanos()));
    let seq = JITTER_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut x = now ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x % 250) as f64 / 1000.0
}

/// Backoff before retry `attempt` (1-based): the pre-existing 2^attempt-second
/// rung, clamped at 2^10, plus 0–25% multiplicative jitter.
fn backoff_delay(attempt: u32) -> Duration {
    let base = 2u64.pow(attempt.min(10));
    Duration::from_secs_f64(base as f64 * (1.0 + backoff_jitter_factor()))
}

/// Delay before retrying. `None` = terminal: the server asked for more than
/// `RETRY_AFTER_CAP`. Without a header, cold-start 503s keep the 60 s rung
/// (8 attempts) and other errors keep the jittered 2^attempt rung.
pub(crate) fn schedule_delay(
    after: Option<Duration>,
    last_err: &str,
    attempt: u32,
) -> Option<Duration> {
    match after {
        Some(d) if d > RETRY_AFTER_CAP => None,
        Some(d) => Some(d),
        None if is_cold_start(last_err) => Some(Duration::from_secs(60)),
        None => Some(backoff_delay(attempt)),
    }
}

/// 503 from serverless GPU endpoints usually means cold start (scale-from-zero
/// takes minutes for big models), not rejection. Worth out-waiting; other
/// errors keep the short ladder.
pub(crate) fn is_cold_start(msg: &str) -> bool {
    msg.contains("503")
}

/// Digits after `marker` in a message (`"; content=4 chars"` -> 4). The
/// ladder needs the counts to pick its rung.
pub(crate) fn chars_after(text: &str, marker: &str) -> usize {
    let Some(i) = text.find(marker) else {
        return 0;
    };
    let rest = &text[i + marker.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::EndpointProfile;
    use crate::protocol::wire_body;
    use crate::test_support::req_with_tool;
    use provider_core::parse_retry_after;
    #[test]
    fn cold_start_is_503_only() {
        assert!(is_cold_start("503 Service Unavailable: "));
        assert!(!is_cold_start("429 Too Many Requests"));
        assert!(!is_cold_start("502 Bad Gateway"));
    }
    #[test]
    fn retry_after_both_encodings() {
        assert_eq!(parse_retry_after("0"), Some(Duration::from_secs(0)));
        assert!(
            parse_retry_after("Sun, 06 Nov 2034 08:49:37 GMT")
                .unwrap()
                .as_secs()
                > 200_000_000
        );
    }
    #[test]
    fn schedule_delay_caps_server_requests_at_60s() {
        // Above the cap: terminal, however large the ask — and both header
        // encodings (seconds, HTTP-date) funnel through here.
        assert!(schedule_delay(Some(Duration::from_secs(3600)), "429", 1).is_none());
        assert!(schedule_delay(Some(Duration::from_secs(61)), "429", 1).is_none());
        assert!(
            schedule_delay(parse_retry_after("Sun, 06 Nov 2034 08:49:37 GMT"), "429", 1).is_none()
        );
        // Exactly the cap is still honoured, verbatim.
        assert_eq!(
            schedule_delay(Some(Duration::from_secs(60)), "429", 1),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            schedule_delay(Some(Duration::from_secs(1)), "429", 1),
            Some(Duration::from_secs(1))
        );
        // No header: cold-start 503 keeps its 60 s rung, other errors the
        // jittered 2^attempt rung.
        assert_eq!(
            schedule_delay(None, "503 Service Unavailable", 3),
            Some(Duration::from_secs(60))
        );
        let d = schedule_delay(None, "429 Too Many Requests", 1).unwrap();
        assert!(
            d >= Duration::from_secs(2) && d <= Duration::from_millis(2500),
            "{d:?}"
        );
    }
    #[test]
    fn backoff_delay_jitter_stays_within_rung_bounds() {
        // 0–25% multiplicative jitter: every draw stays in [base, 1.25*base],
        // rung magnitudes and ordering are the pre-jitter 2^attempt ladder,
        // and the 2^10 clamp is unchanged.
        let mut prev_hi = Duration::ZERO;
        for attempt in 1..=11u32 {
            let base = Duration::from_secs(2u64.pow(attempt.min(10)));
            let mut lo = Duration::MAX;
            let mut hi = Duration::ZERO;
            let mut samples = Vec::new();
            for _ in 0..128 {
                let d = backoff_delay(attempt);
                assert!(d >= base, "attempt {attempt}: {d:?} below base {base:?}");
                assert!(
                    d.as_secs_f64() <= base.as_secs_f64() * 1.25,
                    "attempt {attempt}: {d:?} above 1.25*{base:?}"
                );
                lo = lo.min(d);
                hi = hi.max(d);
                samples.push(d);
            }
            assert!(
                samples.iter().any(|d| *d != samples[0]),
                "attempt {attempt}: jitter draws must vary"
            );
            if attempt <= 10 {
                assert!(lo > prev_hi, "attempt {attempt}: rungs must stay ordered");
            } else {
                // 2^10 clamp unchanged: same rung magnitude as attempt 10.
                assert_eq!(base, Duration::from_secs(1024));
            }
            prev_hi = hi;
        }
    }
    #[test]
    fn ladder_thinking_off_first_then_raises_never_shrinks() {
        let profile = EndpointProfile::default();
        let mut req = req_with_tool();
        req.max_tokens = 8_000;
        let mut k = WireKnobs::from_req(&req, &profile);
        // Zero-content reasoning overflow: thinking-off first, no token change.
        assert!(apply_ladder(&mut k, 0, 500));
        assert!(k.thinking_suppressed);
        assert_eq!(k.max_tokens, 8_000);
        // Same failure again: the one-shot rung is spent, so raise (double).
        assert!(apply_ladder(&mut k, 0, 500));
        assert_eq!(k.max_tokens, 16_000);
        assert!(apply_ladder(&mut k, 0, 500));
        assert_eq!(k.max_tokens, 32_000);
        assert!(apply_ladder(&mut k, 0, 500));
        assert_eq!(k.max_tokens, MAX_TOKENS_CEILING, "bounded by the ceiling");
        assert!(!apply_ladder(&mut k, 0, 500), "no rung left at the ceiling");
        // Content shipped: thinking is not the culprit, raise immediately.
        let mut c = WireKnobs::from_req(&req, &profile);
        assert!(apply_ladder(&mut c, 50, 500));
        assert!(!c.thinking_suppressed);
        assert_eq!(c.max_tokens, 16_000);
        // No real knob for this endpoint: the placebo rung is skipped.
        let knobless = EndpointProfile {
            thinking_off: None,
            ..profile
        };
        let mut n = WireKnobs::from_req(&req, &knobless);
        assert!(apply_ladder(&mut n, 0, 500));
        assert!(!n.thinking_suppressed);
        assert_eq!(n.max_tokens, 16_000);
        // Degenerate zero budget: no rung may claim progress (no endless ladder).
        let mut z = WireKnobs {
            max_tokens: 0,
            thinking_off: None,
            thinking_suppressed: true,
        };
        assert!(!apply_ladder(&mut z, 50, 0));
        assert_eq!(z.max_tokens, 0);
    }
    #[test]
    fn ladder_rung_wire_body_sends_thinking_disabled_and_keeps_max_tokens() {
        let profile = EndpointProfile::default();
        let req = req_with_tool();
        let mut k = WireKnobs::from_req(&req, &profile);
        let before = wire_body("m", &req, &k);
        assert!(before.get("thinking").is_none());
        assert!(apply_ladder(&mut k, 0, 12));
        let after = wire_body("m", &req, &k);
        assert_eq!(after["thinking"]["type"], "disabled");
        assert_eq!(after["max_tokens"], 64, "rung 1 spends no extra tokens");
        // The next rung raises and leaves the knob in place.
        assert!(apply_ladder(&mut k, 0, 12));
        let raised = wire_body("m", &req, &k);
        assert_eq!(raised["thinking"]["type"], "disabled");
        assert_eq!(raised["max_tokens"], 128);
    }
}
