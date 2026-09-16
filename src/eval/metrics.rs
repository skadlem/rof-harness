use crate::config::PricingConfig;
use crate::obs::TraceEvent;
use serde::{Deserialize, Serialize};

/// Aggregated over one suite run. Cost/latency come from the trace stream,
/// so tuning budgets/routing shows up here quantitatively (meta-harness).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub tasks: usize,
    pub passed: usize,
    pub tool_calls: usize,
    pub tool_ok: usize,
    pub total_latency_ms: u64,
    pub est_input_tokens: u64,
    pub est_output_tokens: u64,
    /// Input tokens the provider served from its prefix cache.
    pub cached_input_tokens: u64,
    /// Sum of provider-reported cost; None when no call reported one.
    pub cost_usd: Option<f64>,
    /// Number of calls that reported cost (denominator honesty).
    pub cost_samples: u64,
    /// Tasks stopped by the token ceiling (reliability signal).
    pub budget_aborts: usize,
    /// Model calls that needed more than one HTTP attempt (reliability).
    pub retried_calls: u64,
    /// Highest attempt count seen on any single call.
    pub max_attempts: u64,
    /// Model calls that ultimately failed (reliability).
    pub model_errors: u64,
    /// Per-agent input tokens, for attribution (agent -> tokens).
    pub tokens_by_agent: std::collections::BTreeMap<String, u64>,
    /// USD estimated from the local price table (always available).
    pub est_cost_usd: f64,
    /// Price table used for `est_cost_usd`.
    pub pricing: PricingConfig,
    /// Weight on cost in `utility` (0 = report-only).
    pub cost_lambda: f64,
}

impl Default for EvalReport {
    fn default() -> Self {
        Self {
            tasks: 0,
            passed: 0,
            tool_calls: 0,
            tool_ok: 0,
            total_latency_ms: 0,
            est_input_tokens: 0,
            est_output_tokens: 0,
            cached_input_tokens: 0,
            cost_usd: None,
            cost_samples: 0,
            budget_aborts: 0,
            retried_calls: 0,
            max_attempts: 0,
            model_errors: 0,
            tokens_by_agent: std::collections::BTreeMap::new(),
            est_cost_usd: 0.0,
            pricing: PricingConfig::default(),
            cost_lambda: 0.0,
        }
    }
}

impl EvalReport {
    pub fn success_rate(&self) -> f64 {
        if self.tasks == 0 {
            0.0
        } else {
            self.passed as f64 / self.tasks as f64
        }
    }

    pub fn tool_accuracy(&self) -> f64 {
        if self.tool_calls == 0 {
            0.0
        } else {
            self.tool_ok as f64 / self.tool_calls as f64
        }
    }

    /// Share of billed input tokens that were cache hits.
    pub fn cache_hit_rate(&self) -> f64 {
        if self.est_input_tokens == 0 {
            0.0
        } else {
            self.cached_input_tokens as f64 / self.est_input_tokens as f64
        }
    }

    /// success − λ·estimated_cost. With λ=0 this is pure success rate and the
    /// cost columns decide ties (lexicographic), which is the default stance.
    pub fn utility(&self) -> f64 {
        self.success_rate() - self.cost_lambda * self.est_cost_usd
    }

    fn add_cost(&mut self, input: u64, cached: u64, output: u64) {
        let m = 1_000_000.0;
        let uncached = input.saturating_sub(cached);
        self.est_cost_usd += uncached as f64 / m * self.pricing.input_per_mtok
            + cached as f64 / m * self.pricing.cached_input_per_mtok
            + output as f64 / m * self.pricing.output_per_mtok;
    }

    pub fn fold(&mut self, ev: &TraceEvent) {
        match ev {
            TraceEvent::ModelCall {
                agent,
                input_tokens,
                output_tokens,
                latency_ms,
                cost_usd,
                cached_input_tokens,
                attempts,
                ..
            } => {
                let attempts = (*attempts).max(1);
                if attempts > 1 {
                    self.retried_calls += 1;
                }
                self.max_attempts = self.max_attempts.max(attempts);
                self.est_input_tokens += input_tokens;
                self.est_output_tokens += output_tokens;
                self.total_latency_ms += latency_ms;
                self.cached_input_tokens += cached_input_tokens;
                self.add_cost(*input_tokens, *cached_input_tokens, *output_tokens);
                *self.tokens_by_agent.entry(agent.clone()).or_insert(0) += input_tokens;
                if let Some(c) = cost_usd {
                    self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + c);
                    self.cost_samples += 1;
                }
            }
            TraceEvent::ToolCall { ok, latency_ms, .. } => {
                self.tool_calls += 1;
                if *ok {
                    self.tool_ok += 1;
                }
                self.total_latency_ms += latency_ms;
            }
            TraceEvent::ReviewVerdict { pass, .. } => {
                self.tasks += 1;
                if *pass {
                    self.passed += 1;
                }
            }
            TraceEvent::BudgetExceeded { .. } => {
                self.budget_aborts += 1;
            }
            TraceEvent::ModelError { .. } => {
                self.model_errors += 1;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::TraceEvent;

    fn call(input: u64, cached: u64, output: u64) -> TraceEvent {
        TraceEvent::ModelCall {
            agent: "implementer".into(),
            model: "deepseek-chat".into(),
            input_tokens: input,
            output_tokens: output,
            latency_ms: 100,
            cost_usd: None,
            cached_input_tokens: cached,
            attempts: 1,
        }
    }

    #[test]
    fn cache_hits_are_billed_at_the_cached_rate() {
        let mut r = EvalReport::default();
        r.fold(&call(1000, 0, 0));
        let full = r.est_cost_usd;
        let mut r2 = EvalReport::default();
        r2.fold(&call(1000, 1000, 0));
        let all_cached = r2.est_cost_usd;
        // DeepSeek off-peak: 0.15 vs 0.003 per Mtok => 50x cheaper
        assert!(
            (full / all_cached - 50.0).abs() < 1.0,
            "{full} vs {all_cached}"
        );
    }

    #[test]
    fn attribution_and_rates() {
        let mut r = EvalReport::default();
        r.fold(&call(800, 400, 200));
        r.fold(&TraceEvent::ReviewVerdict {
            pass: true,
            feedback: String::new(),
        });
        assert_eq!(r.est_input_tokens, 800);
        assert_eq!(r.cached_input_tokens, 400);
        assert!((r.cache_hit_rate() - 0.5).abs() < 1e-9);
        assert_eq!(r.success_rate(), 1.0);
        assert_eq!(r.utility(), 1.0); // λ=0 by default
        assert_eq!(r.tokens_by_agent.get("implementer"), Some(&800));
    }
}
