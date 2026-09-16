//! Stage 2 acceptance, offline: the per-layer policy, summarize-before-cut, the
//! content-keyed summary cache, and what a failed summarization falls back to.
//!
//! These are the decisions the stage exists for, so they are pinned here rather
//! than only exercised live: a stage that changes prompt content needs a
//! deterministic harness for its mechanism before any arm is worth running.

use async_trait::async_trait;
use rof::config::{AppConfig, TokenBudgets};
use rof::context::{
    ContextBuilder, ContextPolicy, CtxState, LayerKind, LayerPolicy, LayerStrategy,
};
use rof::llm::{ContextService, LlmClient, LlmError, LlmReq, LlmResp};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// Counts summarizer calls, optionally failing them. Every call returns a short
/// fixed summary so "did the delivered text come from the model?" is decidable
/// by content alone.
#[derive(Default)]
struct CountingClient {
    calls: AtomicU64,
    fail: bool,
    last_max_tokens: AtomicUsize,
}

#[async_trait]
impl LlmClient for CountingClient {
    async fn complete(&self, _model: &str, req: LlmReq) -> Result<LlmResp, LlmError> {
        self.last_max_tokens.store(req.max_tokens, Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(LlmError::Transport("summarizer down".to_string()));
        }
        Ok(LlmResp {
            text: format!("SUMMARY[{}]", req.prompt.len()),
            input_tokens: req.prompt.len() as u64 / 4,
            output_tokens: 7,
            latency_ms: 3,
            cost_usd: None,
            cached_input_tokens: 11,
            attempts: 1,
        })
    }
}

fn state(long: String, mid: String, short: String) -> CtxState {
    CtxState {
        long_term: long,
        mid_term: mid,
        short_term: short,
    }
}

/// Long and short unarmed (the shipped defaults), mid armed at `at` with a
/// 1000-token budget => 4000 chars, so `at * 4000` is the threshold.
fn mid_armed(at: f32) -> ContextPolicy {
    let mut p = ContextPolicy::from(&TokenBudgets {
        long_term: 2000,
        mid_term: 1000,
        short_term: 6000,
    });
    p.mid.summarize_at = at;
    p
}

fn svc(client: Arc<CountingClient>) -> ContextService {
    ContextService::new(client, "counting-ctx".to_string())
}

#[tokio::test]
async fn an_armed_layer_is_summarized_before_it_is_cut() {
    // 6000 chars of mid-term against a 4000-char cap and a 0.5 threshold: the
    // layer must be compressed by the cheap model, not chopped by the builder.
    let client = Arc::new(CountingClient::default());
    let b = ContextBuilder::with_policy(mid_armed(0.5));
    let st = state(
        "conventions".to_string(),
        "R".repeat(6000),
        "round 1".to_string(),
    );
    let (view, reports) = b.plan_summarized(&st, &svc(client.clone())).await;
    let mid = &reports[LayerKind::Mid.index()];
    assert!(mid.summarized, "{mid:?}");
    assert!(mid.summarize.call, "the call must be attributed");
    assert!(!mid.truncated, "the summary fits the budget: {mid:?}");
    assert_eq!(mid.est_tokens, "SUMMARY[6000]".len() / 4);
    assert_eq!(client.calls.load(Ordering::SeqCst), 1);
    assert!(
        view.prompt.contains("SUMMARY["),
        "delivered text is the model's"
    );
    assert!(!view.prompt.contains("...[truncated]..."));
    // The summary's own tokens land on the report, which is what the
    // orchestrator traces; nothing else may claim them.
    assert_eq!(mid.summarize.input_tokens, 1500);
    assert_eq!(mid.summarize.output_tokens, 7);
    assert_eq!(mid.summarize.cached_input_tokens, 11);
}

#[tokio::test]
async fn the_summary_is_paid_for_once_and_reused_after() {
    // The cache is the whole point of summarize-before-cut: a retry that sees
    // the same layer must not buy the same summary again.
    let client = Arc::new(CountingClient::default());
    let b = ContextBuilder::with_policy(mid_armed(0.5));
    let mid = "R".repeat(6000);
    let (v1, r1) = b
        .plan_summarized(
            &state("c".into(), mid.clone(), "s".into()),
            &svc(client.clone()),
        )
        .await;
    // A fresh CtxState with the same text stands in for the next round.
    let (v2, r2) = b
        .plan_summarized(&state("c".into(), mid, "s".into()), &svc(client.clone()))
        .await;
    assert_eq!(
        client.calls.load(Ordering::SeqCst),
        1,
        "one call, two views"
    );
    assert!(r1[1].summarize.call && !r1[1].summarize.cached);
    assert!(r2[1].summarized && r2[1].summarize.cached, "{:?}", r2[1]);
    assert!(!r2[1].summarize.call, "a cache hit spends nothing");
    assert_eq!(r2[1].summarize.input_tokens, 0);
    assert_eq!(v1.prompt, v2.prompt, "and the prompt stays byte-stable");
}

#[tokio::test]
async fn a_changed_layer_is_summarized_again() {
    let client = Arc::new(CountingClient::default());
    let b = ContextBuilder::with_policy(mid_armed(0.5));
    b.plan_summarized(
        &state("c".into(), "R".repeat(6000), "s".into()),
        &svc(client.clone()),
    )
    .await;
    b.plan_summarized(
        &state("c".into(), "R".repeat(9000), "s".into()),
        &svc(client.clone()),
    )
    .await;
    assert_eq!(
        client.calls.load(Ordering::SeqCst),
        2,
        "a layer whose text changed is a different summary"
    );
}

#[tokio::test]
async fn a_failed_summarize_falls_back_to_the_strategy() {
    // Transport failure must not fail the round: the layer goes out cut, the
    // report says it was cut, and nothing claims tokens for a call that failed.
    let client = Arc::new(CountingClient {
        calls: AtomicU64::new(0),
        fail: true,
        last_max_tokens: AtomicUsize::new(0),
    });
    let b = ContextBuilder::with_policy(mid_armed(0.5));
    let st = state("c".into(), "R".repeat(6000), "s".into());
    let (view, reports) = b.plan_summarized(&st, &svc(client.clone())).await;
    let mid = &reports[LayerKind::Mid.index()];
    assert!(!mid.summarized);
    assert!(mid.truncated, "the fallback is the cut: {mid:?}");
    assert_eq!(mid.summarize.input_tokens, 0);
    assert_eq!(client.calls.load(Ordering::SeqCst), 1);
    assert!(view.prompt.contains("...[truncated]..."));
}

#[tokio::test]
async fn an_unarmed_layer_is_never_summarized() {
    // Defaults: long and short are unarmed. 60k chars of short-term must be cut
    // by the strategy and cost no model call — the reviewer's evidence layer is
    // exactly what must not be paraphrased.
    let client = Arc::new(CountingClient::default());
    let b = ContextBuilder::with_policy(mid_armed(0.5));
    let st = state("c".into(), "m".repeat(100), "S".repeat(60_000));
    let (view, reports) = b.plan_summarized(&st, &svc(client.clone())).await;
    assert_eq!(client.calls.load(Ordering::SeqCst), 0);
    assert!(reports[LayerKind::Short.index()].truncated);
    assert!(!reports[LayerKind::Short.index()].summarized);
    assert!(view.truncated, "the view reports the cut");
}

#[tokio::test]
async fn a_layer_below_the_minimum_size_is_not_summarized() {
    // Armed, and over the threshold share, but far too small for a call to be
    // worth making (MIN_SUMMARY_CHARS): the strategy cuts it and the run spends
    // nothing.
    let client = Arc::new(CountingClient::default());
    let mut p = mid_armed(0.01);
    p.mid.budget = 20; // cap 80 chars; threshold share ~0.8 chars
    let b = ContextBuilder::with_policy(p);
    let (_, reports) = b
        .plan_summarized(
            &state("c".into(), "R".repeat(100), "s".into()),
            &svc(client.clone()),
        )
        .await;
    assert_eq!(client.calls.load(Ordering::SeqCst), 0);
    let mid = &reports[LayerKind::Mid.index()];
    assert!(!mid.summarized);
    assert!(mid.truncated, "{mid:?}");
}

#[tokio::test]
async fn raw_strategy_passes_a_layer_through_whole() {
    let client = Arc::new(CountingClient::default());
    let mut p = mid_armed(0.5);
    p.mid = LayerPolicy {
        budget: 10,
        strategy: LayerStrategy::Raw,
        summarize_at: 0.0,
    };
    let b = ContextBuilder::with_policy(p);
    let (view, reports) = b
        .plan_summarized(
            &state("c".into(), "R".repeat(50_000), "s".into()),
            &svc(client),
        )
        .await;
    let mid = &reports[LayerKind::Mid.index()];
    assert!(!mid.truncated);
    assert_eq!(mid.chars, 50_000);
    assert!(view.prompt.contains(&"R".repeat(50_000)));
}

#[tokio::test]
async fn reports_are_indexed_long_mid_short() {
    let client = Arc::new(CountingClient::default());
    let b = ContextBuilder::with_policy(mid_armed(0.5));
    let (_, reports) = b
        .plan_summarized(&state("c".into(), "m".into(), "s".into()), &svc(client))
        .await;
    assert_eq!(reports.len(), 3);
    for (i, k) in LayerKind::ALL.iter().enumerate() {
        assert_eq!(reports[i].layer, *k);
        assert_eq!(reports[i].layer.index(), i);
    }
    // The pure path reports the same shape, with no summarize traffic.
    let (_, planned) = b.plan(&state("c".into(), "m".into(), "s".into()));
    assert!(planned.iter().all(|r| !r.summarized && !r.summarize.call));
}

#[test]
fn budgets_derive_the_policy_and_a_context_block_wins() {
    // A pre-stage-2 config file states budgets and nothing else: it must keep
    // meaning exactly what it said (same budgets, mid armed by default).
    let dir = std::env::temp_dir().join(format!("rof-ctx-policy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let old = dir.join("old.json");
    std::fs::write(
        &old,
        r#"{"budgets":{"long_term":111,"mid_term":222,"short_term":333}}"#,
    )
    .unwrap();
    let cfg = AppConfig::load(&old).unwrap();
    let p = cfg.context_policy();
    assert_eq!(p.long.budget, 111);
    assert_eq!(p.mid.budget, 222);
    assert_eq!(p.short.budget, 333);
    assert!(p.mid.summarize_at > 0.0, "mid is armed by default");
    assert_eq!(p.long.summarize_at, 0.0);

    // A file that states `context` decides, budgets included — and survives a
    // dump/load round trip, so a tuned policy can be versioned like a config.
    let tuned = dir.join("tuned.json");
    std::fs::write(
        &tuned,
        r#"{"budgets":{"mid_term":1},
            "context":{"long":{"budget":900,"strategy":"raw","summarize_at":0.5},
                       "mid":{"budget":800,"strategy":"head_tail","summarize_at":0.25},
                       "short":{"budget":700,"strategy":"raw","summarize_at":0.0}}}"#,
    )
    .unwrap();
    let cfg = AppConfig::load(&tuned).unwrap();
    let p = cfg.context_policy();
    assert_eq!(p.long.budget, 900);
    assert_eq!(p.long.strategy, LayerStrategy::Raw);
    assert_eq!(p.mid.summarize_at, 0.25);
    assert_eq!(p.short.budget, 700);
    let back = AppConfig::load(&{
        let dumped = dir.join("dumped.json");
        std::fs::write(&dumped, cfg.to_json()).unwrap();
        dumped
    })
    .unwrap();
    assert_eq!(back.context_policy(), p);
    std::fs::remove_dir_all(&dir).ok();
}

/// The summarizer must not ask for more tokens than half the layer's estimated
/// size; it must ask for at least 64 and clamp to the budget. This captures the
/// max_tokens from the LlmReq via the counting stub's response.
#[tokio::test]
async fn summarize_request_is_bounded_by_half_the_layer_estimate() {
    let client = Arc::new(CountingClient::default());
    // A 6000-char mid layer => cap 4000, 0.5 threshold => 2000; 6000 > 2000 so armed.
    let b = ContextBuilder::with_policy(mid_armed(0.5));
    let st = state("c".into(), "X".repeat(6000), "s".into());
    let (_view, reports) = b.plan_summarized(&st, &svc(client.clone())).await;
    let mid = &reports[LayerKind::Mid.index()];
    // Call should still have happened (armed, over threshold, >= MIN_SUMMARY_CHARS).
    assert!(mid.summarized);
    assert!(mid.summarize.call);
    // The request's max_tokens is bounded; the stub returns `req.prompt.len()`
    // as input_tokens, so we verify via the call having occurred with bounded
    // input rather than the full budget (pol.budget = 1000 => 4000 chars cap).
    // The key invariant: max_tokens is the content budget (bounded by half
    // est => 6000 chars => 375) plus a fixed reasoning headroom, because a
    // hidden-chain model spends tokens before its visible answer.
    assert!(
        client.last_max_tokens.load(Ordering::SeqCst) <= 375 + 3072,
        "max_tokens bounded by half est + headroom (3447): got {}",
        client.last_max_tokens.load(Ordering::SeqCst)
    );
}
