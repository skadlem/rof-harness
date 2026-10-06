/// USD cost at DeepSeek's published PEAK rates ($/1Mtok, api-docs.deepseek.com
/// "Models & Pricing", fetched 2026-10-03): flash in-hit $0.006 / in-miss
/// $0.30 / out $1.20; v4-pro $0.044 / $1.32 / $3.96. PEAK is a conservative
/// upper bound — DeepSeek bills off-peak at exactly half (weekends and CN
/// holidays in full; the holiday calendar is not modeled). `input` includes
/// cache-hit tokens (OpenAI wire semantics), so they split at the hit rate.
/// The go-routed id `deepseek-v4.1-flash` is the same model at the same price
/// book: OpenCode Go's DeepSeek V4.1 Flash rows are DeepSeek's off-peak pair
/// (hit $0.003 / miss $0.15; research/refresh-cache-economics.md:9-10) and the
/// identical peak tuple ($0.006 / $0.30 / $1.20; Go page Peak row, captured
/// in the same research session), so it takes the flash PEAK rates. The table
/// is keyed by model id with no endpoint dimension — if Go's rates diverge
/// from DeepSeek's, this needs a per-endpoint price book.
/// Unknown models price `None`: never fabricate a cost.
pub(crate) fn price_usd(model: &str, input: u64, cache_read: u64, output: u64) -> Option<f64> {
    let (hit, miss, out) = match model {
        // Legacy alias `deepseek-v4-flash` is served by the same model billed
        // at the Flash price (research/DECISIONS.md:74: "deepseek-flash IS
        // DeepSeek-V4.1-Flash (legacy deepseek-v4-flash served by it, billed
        // at Flash price — api-docs footnote 2026-10-03)").
        m if m.starts_with("deepseek-flash")
            || m.starts_with("deepseek-v4-flash")
            || m.starts_with("deepseek-v4.1-flash") =>
        {
            (0.006, 0.30, 1.20)
        }
        m if m.starts_with("deepseek-v4-pro") => (0.044, 1.32, 3.96),
        _ => return None,
    };
    let miss_in = input.saturating_sub(cache_read);
    Some((cache_read as f64 * hit + miss_in as f64 * miss + output as f64 * out) / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::parse_body;
    #[test]
    fn price_usd_splits_hit_miss_and_never_fabricates() {
        // flash PEAK: hit $0.006/M, miss $0.30/M, out $1.20/M (api-docs 2026-10-03)
        let c = price_usd("deepseek-flash", 1_000_000, 500_000, 250_000).unwrap();
        assert!((c - 0.453).abs() < 1e-9, "{c}"); // 0.003 + 0.15 + 0.30
        let all_hit = price_usd("deepseek-flash", 1_000_000, 1_000_000, 0).unwrap();
        assert!(
            (all_hit - 0.006).abs() < 1e-12,
            "cache hits never pay miss rate"
        );
        let pro = price_usd("deepseek-v4-pro", 1_000_000, 0, 1_000_000).unwrap();
        assert!((pro - (1.32 + 3.96)).abs() < 1e-9, "pro PEAK sum");
        // go-routed matched-run id: same price book as direct flash
        let go = price_usd("deepseek-v4.1-flash", 1_000_000, 500_000, 250_000).unwrap();
        assert!(
            (go - 0.453).abs() < 1e-9,
            "go flash prices like direct flash: {go}"
        );
        // legacy alias: same model billed at the Flash price
        // (research/DECISIONS.md:74)
        let legacy = price_usd("deepseek-v4-flash", 1_000_000, 500_000, 250_000).unwrap();
        assert!(
            (legacy - 0.453).abs() < 1e-9,
            "legacy alias prices like flash: {legacy}"
        );
        assert_eq!(
            price_usd("gpt-4o", 10, 0, 10),
            None,
            "unknown models never fabricate a cost"
        );
    }
    #[test]
    fn parse_body_prices_usage_and_starts_without_retries() {
        let v = serde_json::json!({
            "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1000, "completion_tokens": 500}
        });
        let r = parse_body(&v, "deepseek-flash", 64).unwrap();
        let cost = r.usage.cost_usd.expect("known model is priced");
        assert!((cost - (1000.0 * 0.30 + 500.0 * 1.20) / 1e6).abs() < 1e-12);
        assert!(r.retry_usage.is_none());
    }
}
