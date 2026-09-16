use super::policy::{ContextPolicy, LayerKind, LayerReport, LayerStrategy, SummaryStat};
use super::state::{CtxState, CtxView};
use crate::config::TokenBudgets;
use crate::llm::ContextService;
use std::collections::HashMap;
use std::sync::Mutex;

/// Builds the three-layer prompt from [`CtxState`] under a [`ContextPolicy`].
///
/// Two entry points:
///
/// - [`plan`](Self::plan) is pure and synchronous: strategy + cut, no model
///   call. This is what a prompt-construction path that must not spend tokens
///   (the planner's view, tests) uses.
/// - [`plan_summarized`](Self::plan_summarized) is the first-class path: a
///   layer over its `summarize_at` threshold is compressed by the cheap Context
///   LLM, the compression is cached by content, and only a *failed* call falls
///   back to the strategy.
///
/// The summary cache lives here rather than in `CtxState` because `CtxState` is
/// rebuilt every round by the orchestrator: a cache inside a per-round value
/// caches nothing. One builder per run means a retry sees the summary the first
/// round paid for, keyed on the text it was made from.
pub struct ContextBuilder {
    policy: ContextPolicy,
    summaries: Mutex<HashMap<String, String>>,
}

/// Nearest char boundary at or below `i` (0 when there is none).
fn floor_boundary(s: &str, i: usize) -> usize {
    let i = i.min(s.len());
    (0..=i).rev().find(|&j| s.is_char_boundary(j)).unwrap_or(0)
}

/// Nearest char boundary at or above `i` (`s.len()` when there is none).
fn ceil_boundary(s: &str, i: usize) -> usize {
    let i = i.min(s.len());
    (i..=s.len())
        .find(|&j| s.is_char_boundary(j))
        .unwrap_or(s.len())
}

/// Cache key for one layer's text: the layer plus the text itself, so a layer
/// that changed is summarized again and one that did not is free.
fn summary_key(kind: LayerKind, text: &str) -> String {
    format!(
        "{}:{}",
        kind.index(),
        crate::eval::runner::fnv1a_hex(text.as_bytes())
    )
}

impl ContextBuilder {
    /// Policy derived from the old `TokenBudgets` (see [`ContextPolicy::from`]).
    pub fn new(budgets: TokenBudgets) -> Self {
        Self::with_policy(ContextPolicy::from(&budgets))
    }

    pub fn with_policy(policy: ContextPolicy) -> Self {
        Self {
            policy,
            summaries: Mutex::new(HashMap::new()),
        }
    }

    pub fn policy(&self) -> &ContextPolicy {
        &self.policy
    }

    /// v1 signature, kept for callers that only want the prompt.
    pub fn build(&self, state: &CtxState) -> CtxView {
        self.plan(state).0
    }

    /// Pure, synchronous: apply each layer's strategy and cut what overflows.
    pub fn plan(&self, state: &CtxState) -> (CtxView, Vec<LayerReport>) {
        let texts = self.texts(state);
        let mut fitted = Vec::with_capacity(3);
        let mut reports = Vec::with_capacity(3);
        for (i, kind) in LayerKind::ALL.iter().enumerate() {
            let delivered = self.shape(*kind, texts[i]);
            reports.push(self.report(*kind, &delivered.0, delivered.1, SummaryStat::default()));
            fitted.push(delivered.0);
        }
        (join(fitted, &reports), reports)
    }

    /// The first-class path: summarize-before-truncate, one cached call per
    /// changed layer. A summarization that fails (transport, budget) leaves the
    /// layer as it was, so the strategy still gets its chance — a failed
    /// summary must never fail the round.
    pub async fn plan_summarized(
        &self,
        state: &CtxState,
        ctx: &ContextService,
    ) -> (CtxView, Vec<LayerReport>) {
        let texts = self.texts(state);
        let mut fitted = Vec::with_capacity(3);
        let mut reports = Vec::with_capacity(3);
        for (i, kind) in LayerKind::ALL.iter().enumerate() {
            let pol = *self.policy.layer(*kind);
            let raw = texts[i];
            let mut delivered = raw.to_string();
            let mut stat = SummaryStat::default();
            if self.policy.wants_summary(*kind, raw.chars().count()) {
                let key = summary_key(*kind, raw);
                let hit = self.summaries.lock().unwrap().get(&key).cloned();
                match hit {
                    Some(cached) => {
                        delivered = cached;
                        stat.cached = true;
                    }
                    None => {
                        let est_tokens = (raw.chars().count() / 16).max(64).min(pol.budget);
                        if let Ok(r) = ctx.summarize(raw, est_tokens).await {
                            self.summaries.lock().unwrap().insert(key, r.text.clone());
                            delivered = r.text;
                            stat = SummaryStat {
                                call: true,
                                cached: false,
                                input_tokens: r.input_tokens,
                                output_tokens: r.output_tokens,
                                cached_input_tokens: r.cached_input_tokens,
                                latency_ms: r.latency_ms,
                                cost_usd: r.cost_usd,
                                attempts: r.attempts,
                            };
                        }
                    }
                }
            }
            let (text, truncated) = self.shape(*kind, &delivered);
            reports.push(self.report(*kind, &text, truncated, stat));
            fitted.push(text);
        }
        (join(fitted, &reports), reports)
    }

    fn texts<'a>(&self, state: &'a CtxState) -> [&'a str; 3] {
        [&state.long_term, &state.mid_term, &state.short_term]
    }

    /// The strategy applied to one layer's text: the delivered string and
    /// whether it had to be cut.
    fn shape(&self, kind: LayerKind, text: &str) -> (String, bool) {
        let pol = self.policy.layer(kind);
        match pol.strategy {
            LayerStrategy::Raw => (text.to_string(), false),
            LayerStrategy::HeadTail => fit(text, pol.budget),
        }
    }

    fn report(
        &self,
        kind: LayerKind,
        delivered: &str,
        truncated: bool,
        summarize: SummaryStat,
    ) -> LayerReport {
        LayerReport {
            layer: kind,
            strategy: self.policy.layer(kind).strategy,
            chars: delivered.chars().count(),
            est_tokens: delivered.len() / 4,
            truncated,
            summarized: summarize.call || summarize.cached,
            summarize,
        }
    }
}

fn join(fitted: Vec<String>, reports: &[LayerReport]) -> CtxView {
    let prompt = format!(
        "[LONG-TERM]\n{}\n\n[MID-TERM]\n{}\n\n[SHORT-TERM]\n{}",
        fitted[0], fitted[1], fitted[2]
    );
    CtxView {
        used_tokens: prompt.len() / 4,
        truncated: reports.iter().any(|r| r.truncated),
        prompt,
    }
}

/// Head + tail cut at `budget * 4` chars, on char boundaries (a cut landing
/// inside a multi-byte character panicked a live run: "start byte index 1 is
/// not a char boundary").
fn fit(s: &str, budget: usize) -> (String, bool) {
    let max_chars = budget * 4;
    if s.len() <= max_chars {
        return (s.to_string(), false);
    }
    let head = max_chars * 2 / 3;
    let tail = max_chars - head;
    let hs = floor_boundary(s, head);
    let ts = ceil_boundary(s, s.len() - tail);
    let (h, t) = (&s[..hs], &s[ts..]);
    (format!("{h}\n...[truncated]...\n{t}"), true)
}
