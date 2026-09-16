//! Stage 2: explicit per-layer context policy.
//!
//! One place decides, for each of the three layers, three things:
//!
//! - its char budget (the numbers `TokenBudgets` always held, `budget * 4`),
//! - what happens to the text that does not fit (`LayerStrategy`),
//! - whether the cheap Context LLM compresses that layer *before* the overflow
//!   is cut (`summarize_at`).
//!
//! The order matters and it is the inverse of v1's: v1 summarized the whole
//! prompt *because* it had already overflowed, then cut it anyway if the
//! compression was not enough. Here a layer that crosses its threshold is
//! summarized alone, the result is cached by content, and truncation is what a
//! *failed* summarization falls back to.
//!
//! Defaults are deliberately asymmetric. The mid layer (retrieval) is armed at
//! `DEFAULT_MID_SUMMARIZE_AT`: it is the only layer whose size is a function of
//! the repo rather than of the conversation. The long layer is the stable head
//! (conventions + skill index) and the short layer is the volatile evidence a
//! reviewer judges (artifact, checks, refusals) — compressing either one drops
//! exactly the detail the agents act on, so both default to `0.0` (never).

use crate::config::TokenBudgets;
use serde::{Deserialize, Serialize};

/// Below this many chars a summary call costs more than the text it removes.
pub const MIN_SUMMARY_CHARS: usize = 1200;

/// The mid layer (retrieval) is armed by default at 80% of its budget.
pub const DEFAULT_MID_SUMMARIZE_AT: f32 = 0.8;

/// Which layer of the prompt. Order is the order they are joined in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    #[default]
    Long,
    Mid,
    Short,
}

impl LayerKind {
    pub const ALL: [LayerKind; 3] = [LayerKind::Long, LayerKind::Mid, LayerKind::Short];

    pub fn index(self) -> usize {
        match self {
            LayerKind::Long => 0,
            LayerKind::Mid => 1,
            LayerKind::Short => 2,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            LayerKind::Long => "long",
            LayerKind::Mid => "mid",
            LayerKind::Short => "short",
        }
    }
}

/// What to do with text that overflows the budget and was not summarized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerStrategy {
    /// Head + tail cut at `budget * 4` chars (v1 behaviour, char-boundary safe).
    #[default]
    HeadTail,
    /// Pass through whatever the layer holds. Only sane for a layer whose
    /// content is known to be small; it is how a layer can opt out of cutting.
    Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayerPolicy {
    /// Token budget; the char cap is `budget * 4`.
    pub budget: usize,
    pub strategy: LayerStrategy,
    /// Summarize this layer with the cheap model when it holds more than this
    /// share of its budget. `0.0` = never (truncation is the only overflow
    /// path for this layer).
    pub summarize_at: f32,
}

impl Default for LayerPolicy {
    fn default() -> Self {
        Self {
            budget: 4000,
            strategy: LayerStrategy::HeadTail,
            summarize_at: 0.0,
        }
    }
}

/// Per-layer policy for the three layers. Loaded from the config file's
/// `context` block when present; otherwise derived from `budgets`
/// ([`AppConfig::context_policy`](crate::config::AppConfig::context_policy)),
/// so an old config file keeps meaning exactly what it said.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextPolicy {
    pub long: LayerPolicy,
    pub mid: LayerPolicy,
    pub short: LayerPolicy,
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self::from(&TokenBudgets::default())
    }
}

impl From<&TokenBudgets> for ContextPolicy {
    fn from(b: &TokenBudgets) -> Self {
        Self {
            long: LayerPolicy {
                budget: b.long_term,
                strategy: LayerStrategy::HeadTail,
                summarize_at: 0.0,
            },
            mid: LayerPolicy {
                budget: b.mid_term,
                strategy: LayerStrategy::HeadTail,
                summarize_at: DEFAULT_MID_SUMMARIZE_AT,
            },
            short: LayerPolicy {
                budget: b.short_term,
                strategy: LayerStrategy::HeadTail,
                summarize_at: 0.0,
            },
        }
    }
}

impl ContextPolicy {
    pub fn layer(&self, k: LayerKind) -> &LayerPolicy {
        match k {
            LayerKind::Long => &self.long,
            LayerKind::Mid => &self.mid,
            LayerKind::Short => &self.short,
        }
    }

    pub fn layer_mut(&mut self, k: LayerKind) -> &mut LayerPolicy {
        match k {
            LayerKind::Long => &mut self.long,
            LayerKind::Mid => &mut self.mid,
            LayerKind::Short => &mut self.short,
        }
    }

    /// Chars a layer may hold before the strategy acts (`budget * 4`).
    pub fn chars_cap(&self, k: LayerKind) -> usize {
        self.layer(k).budget * 4
    }

    /// Is this layer over its summarize threshold, and big enough that a call
    /// is worth making?
    pub fn wants_summary(&self, k: LayerKind, chars: usize) -> bool {
        let p = self.layer(k);
        p.summarize_at > 0.0
            && chars >= MIN_SUMMARY_CHARS
            && chars as f32 > p.summarize_at * self.chars_cap(k) as f32
    }
}

/// What one layer's summarization cost. All-zero (via `Default`) when the raw
/// text went through, and `cached: true` when an earlier view already paid for
/// the same summary. Carried on the report so the orchestrator can trace the
/// model call without the builder owning a trace sink.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SummaryStat {
    /// A cheap-model call happened for THIS view.
    pub call: bool,
    /// The delivered text is a summary paid for by an earlier view.
    pub cached: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub latency_ms: u64,
    pub cost_usd: Option<f64>,
    pub attempts: u64,
}

/// What one layer looked like on the way into a prompt. Every field has an
/// emitter in `ContextBuilder` and a consumer (the orchestrator's counters and
/// then `ContextMetrics`, or the acceptance tests).
#[derive(Debug, Clone, PartialEq)]
pub struct LayerReport {
    pub layer: LayerKind,
    pub strategy: LayerStrategy,
    /// Chars delivered (the summary's length when it was summarized).
    pub chars: usize,
    /// chars / 4, the same estimate the budgets are expressed in.
    pub est_tokens: usize,
    pub truncated: bool,
    /// The delivered text is a model summary (fresh or cached).
    pub summarized: bool,
    pub summarize: SummaryStat,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_budgets_derive_a_policy_with_only_mid_armed() {
        let b = TokenBudgets {
            long_term: 1000,
            mid_term: 2000,
            short_term: 3000,
        };
        let p = ContextPolicy::from(&b);
        assert_eq!(p.chars_cap(LayerKind::Long), 4000);
        assert_eq!(p.chars_cap(LayerKind::Mid), 8000);
        assert_eq!(p.chars_cap(LayerKind::Short), 12000);
        assert_eq!(p.layer(LayerKind::Long).summarize_at, 0.0);
        assert!(p.layer(LayerKind::Mid).summarize_at > 0.0);
        assert_eq!(p.layer(LayerKind::Short).summarize_at, 0.0);
    }

    #[test]
    fn threshold_is_share_of_budget_and_needs_a_minimum_size() {
        let p = ContextPolicy::from(&TokenBudgets {
            long_term: 2000,
            mid_term: 1000,
            short_term: 6000,
        });
        // mid cap = 4000 chars, armed at 0.8 -> 3200 chars
        assert!(!p.wants_summary(LayerKind::Mid, 3199));
        assert!(p.wants_summary(LayerKind::Mid, 3201));
        // never for an unarmed layer, however big
        assert!(!p.wants_summary(LayerKind::Long, 999_999));
        // and never for a layer that is armed but tiny
        assert!(!p.wants_summary(LayerKind::Mid, MIN_SUMMARY_CHARS - 1));
    }
}
