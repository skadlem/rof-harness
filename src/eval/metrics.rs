use crate::config::PricingConfig;
use crate::obs::TraceEvent;
use serde::{Deserialize, Serialize};

/// Aggregated over one suite run. Cost/latency come from the trace stream,
/// so tuning budgets/routing shows up here quantitatively (meta-harness).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub tasks: usize,
    pub passed: usize,
    #[serde(default)]
    pub verdicts: usize,
    #[serde(default)]
    pub passed_verdicts: usize,
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
    /// What agents did with the skill store (folded from `SkillOp` events).
    #[serde(default)]
    pub skills: SkillMetrics,
    /// Extra rounds the harness bought after a failed task.
    #[serde(default)]
    pub auto_pokes: u64,
    /// Goals the quality pre-check flagged.
    #[serde(default)]
    pub goal_quality_flags: u64,
}

/// Skill-store traffic for one run. Only successful ops count: a refused
/// manage attempt stays visible in the trace (`ok: false`) and in
/// tool_accuracy, and is not folded here as work done.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SkillMetrics {
    /// Index deliveries that carried at least one skill — the harness
    /// prefetches one per agent whose grant covers `skills.list`.
    pub listed: u64,
    /// Bodies a model asked for (`skill_views`).
    pub viewed: u64,
    /// Manage ops that wrote a proposal.
    pub proposed: u64,
    /// Manage ops that changed the store directly (`direct` policy).
    pub applied: u64,
    /// Bodies injected because the task named the skill: the reuse signal.
    pub reused: u64,
}

impl Default for EvalReport {
    fn default() -> Self {
        Self {
            tasks: 0,
            passed: 0,
            verdicts: 0,
            passed_verdicts: 0,
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
            skills: SkillMetrics::default(),
            auto_pokes: 0,
            goal_quality_flags: 0,
        }
    }
}

impl EvalReport {
    pub fn record_task(&mut self, passed: bool) {
        self.tasks += 1;
        if passed {
            self.passed += 1;
        }
    }

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
                self.verdicts += 1;
                if *pass {
                    self.passed_verdicts += 1;
                }
            }
            TraceEvent::BudgetExceeded { .. } => {
                self.budget_aborts += 1;
            }
            TraceEvent::ModelError { .. } => {
                self.model_errors += 1;
            }
            // Successful skill ops only; a refusal is a trace fact and a
            // tool_accuracy hit, not work done.
            TraceEvent::SkillOp { op, ok: true, .. } => match op.as_str() {
                "list" => self.skills.listed += 1,
                "view" => self.skills.viewed += 1,
                "propose" => self.skills.proposed += 1,
                "apply" => self.skills.applied += 1,
                "reuse" => self.skills.reused += 1,
                _ => {}
            },
            // What the quality pre-check flagged and how many
            // extra rounds the poke bought. Both are trace facts, folded here
            // so a report can say whether either feature did anything at all.
            TraceEvent::GoalQuality { .. } => self.goal_quality_flags += 1,
            TraceEvent::AutoPoke { .. } => self.auto_pokes += 1,
            _ => {}
        }
    }
}

/// Per-task context accounting, folded from what a run already returns.
///
/// `relevance_proxy` is named a *proxy* on purpose: it is the share of
/// retrieved chars whose file the task then touched, not a judgement that the
/// context was relevant. Nothing here needs a model call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextMetrics {
    pub retrieved_files: usize,
    pub retrieved_chars: usize,
    /// Retrieved chars whose path the task's artifact touched (`file_state[]`).
    pub referenced_chars: usize,
    pub relevance_proxy: f32,
    /// Summarizer calls made while this task ran (cheap Context LLM).
    pub summarize_calls: u64,
    /// Input+output tokens those calls cost.
    pub summarize_tokens: u64,
    /// Truncation events in the round loop: a view the builder had to cut
    /// counts once, and again if it still overflows after summarization. The
    /// planner's one-shot view is outside the loop and is not counted.
    pub truncated_views: u32,
    /// Per-layer views delivered as a model summary (stage 2), indexed by
    /// `LayerKind::index()`: long, mid, short. Counts a cached summary too —
    /// what was delivered, not what was paid for here.
    #[serde(default)]
    pub layer_summaries: [u32; 3],
    /// Files git recorded as changed by this run's attempts (§4.2), i.e. the
    /// set the recall metric is measured against. Ground truth, unlike the
    /// artifact's `file_state`, which is self-reported and counts refused
    /// patches too.
    pub changed_files: usize,
    /// Of `changed_files`, present in the retrieved set. `recall()` is the
    /// ratio; 0.0 when nothing changed (undefined, not zero).
    #[serde(default)]
    pub recalled_files: usize,
    /// Per-layer views the strategy had to cut, same indexing.
    #[serde(default)]
    pub layer_truncations: [u32; 3],
    /// Chars a duplicate carried into a prompt the §4.1 assembler refused to
    /// deliver twice. Zero is not "no duplication" — it is no duplication the
    /// assembler was in a position to see.
    #[serde(default)]
    pub eliminated_chars: usize,
}

impl ContextMetrics {
    /// Fold one orchestrator return value: `retrieved[]` is what retrieval put
    /// in front of the agents, `tasks[].artifact.file_state[].path` is what the
    /// run actually touched.
    pub fn from_run(out: &serde_json::Value) -> Self {
        let retrieved: Vec<(&str, u64)> = out
            .get("retrieved")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|e| Some((e.get("path")?.as_str()?, e.get("chars")?.as_u64()?)))
                    .collect()
            })
            .unwrap_or_default();
        let mut touched: Vec<&str> = Vec::new();
        let entries = out
            .get("tasks")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten();
        for t in entries {
            let states = t
                .pointer("/artifact/file_state")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten();
            for f in states {
                if let Some(p) = f.get("path").and_then(|v| v.as_str()) {
                    if !touched.contains(&p) {
                        touched.push(p);
                    }
                }
            }
        }
        let total: u64 = retrieved.iter().map(|(_, c)| c).sum();
        let referenced: u64 = retrieved
            .iter()
            .filter(|(p, _)| touched.contains(p))
            .map(|(_, c)| c)
            .sum();
        // Recall (§4.4): git's change set vs what retrieval put in front of
        // the agents. `retrieved` is run-wide, so a multi-task plan measures
        // each task against the one retrieval — the signal this run supports.
        let mut changed: Vec<&str> = Vec::new();
        for t in out
            .get("tasks")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            for f in t
                .get("changed_files")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                if let Some(p) = f.as_str() {
                    if !changed.contains(&p) {
                        changed.push(p);
                    }
                }
            }
        }
        let recalled = changed
            .iter()
            .filter(|p| retrieved.iter().any(|(r, _)| r == *p))
            .count();
        Self {
            retrieved_files: retrieved.len(),
            retrieved_chars: total as usize,
            referenced_chars: referenced as usize,
            relevance_proxy: if total == 0 {
                0.0
            } else {
                referenced as f32 / total as f32
            },
            changed_files: changed.len(),
            recalled_files: recalled,
            summarize_calls: out
                .get("summarize_calls")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            summarize_tokens: out
                .get("summarize_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            truncated_views: out
                .get("truncated_views")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            layer_summaries: layer_counts(out, "layer_summaries"),
            layer_truncations: layer_counts(out, "layer_truncations"),
            eliminated_chars: out
                .get("eliminated_chars")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize,
        }
    }

    /// Sums for a suite-level aggregate (the report keeps these per task).
    pub fn sum(tasks: impl IntoIterator<Item = Self>) -> Self {
        let mut acc = Self::default();
        for t in tasks {
            acc.retrieved_files += t.retrieved_files;
            acc.retrieved_chars += t.retrieved_chars;
            acc.referenced_chars += t.referenced_chars;
            acc.summarize_calls += t.summarize_calls;
            acc.summarize_tokens += t.summarize_tokens;
            acc.truncated_views += t.truncated_views;
            acc.changed_files += t.changed_files;
            acc.recalled_files += t.recalled_files;
            acc.eliminated_chars += t.eliminated_chars;
            for i in 0..3 {
                acc.layer_summaries[i] += t.layer_summaries[i];
                acc.layer_truncations[i] += t.layer_truncations[i];
            }
        }
        acc.relevance_proxy = if acc.retrieved_chars == 0 {
            0.0
        } else {
            acc.referenced_chars as f32 / acc.retrieved_chars as f32
        };
        acc
    }

    /// Recall (§4.4): the share of git-recorded changes whose file retrieval
    /// had already put in front of the agents. 0.0 when nothing changed —
    /// undefined, not zero. Reads the counts so a suite aggregate is a true
    /// ratio, not a mean of per-task ratios.
    pub fn recall(&self) -> f32 {
        if self.changed_files == 0 {
            0.0
        } else {
            self.recalled_files as f32 / self.changed_files as f32
        }
    }
}

/// One per-layer counter triple (`[long, mid, short]`) off the orchestrator's
/// return value. Missing or short arrays read as zeros — a pre-stage-2 return
/// value has no such keys at all.
fn layer_counts(out: &serde_json::Value, key: &str) -> [u32; 3] {
    let mut v = [0u32; 3];
    if let Some(a) = out.get(key).and_then(|x| x.as_array()) {
        for (i, n) in a.iter().take(3).enumerate() {
            v[i] = n.as_u64().unwrap_or(0) as u32;
        }
    }
    v
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
        r.record_task(true);
        assert_eq!(r.est_input_tokens, 800);
        assert_eq!(r.cached_input_tokens, 400);
        assert!((r.cache_hit_rate() - 0.5).abs() < 1e-9);
        assert_eq!(r.success_rate(), 1.0);
        assert_eq!(r.utility(), 1.0); // λ=0 by default
        assert_eq!(r.tokens_by_agent.get("implementer"), Some(&800));
    }

    #[test]
    fn recall_counts_git_changed_files_against_retrieval() {
        let out = serde_json::json!({
            "retrieved": [
                {"path": "src/a.rs", "chars": 100},
                {"path": "src/b.rs", "chars": 50},
            ],
            "tasks": [
                {"changed_files": ["src/a.rs", "src/c.rs"]},
                // A file changed twice across the plan's tasks counts once.
                {"changed_files": ["src/c.rs", "README.md"]},
            ],
        });
        let m = ContextMetrics::from_run(&out);
        // git saw 3 distinct files; retrieval had handed over 1 of them.
        assert_eq!((m.changed_files, m.recalled_files), (3, 1));
        assert!((m.recall() - 1.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn recall_is_zero_when_nothing_changed() {
        // A no-write run has an empty change set: the ratio is undefined, and
        // reporting 0.0 (not a mean over zero tasks) is what the gate sees.
        let out = serde_json::json!({
            "retrieved": [{"path": "src/a.rs", "chars": 100}],
            "tasks": [{"changed_files": []}],
        });
        let m = ContextMetrics::from_run(&out);
        assert_eq!((m.changed_files, m.recalled_files), (0, 0));
        assert_eq!(m.recall(), 0.0);
    }

    #[test]
    fn recall_aggregates_as_a_true_ratio_not_a_mean() {
        // ponytail: recall() reads the counts, so a suite aggregate is
        // weighted by files changed — one task touching 1/1 must not average
        // away a task touching 0/100.
        let a = ContextMetrics {
            changed_files: 1,
            recalled_files: 1,
            ..Default::default()
        };
        let b = ContextMetrics {
            changed_files: 100,
            recalled_files: 0,
            ..Default::default()
        };
        let s = ContextMetrics::sum([a, b]);
        assert!((s.recall() - 1.0 / 101.0).abs() < 1e-6);
    }

    #[test]
    fn verdicts_do_not_define_task_success() {
        let mut r = EvalReport::default();
        r.fold(&TraceEvent::ReviewVerdict {
            pass: true,
            feedback: String::new(),
        });
        r.fold(&TraceEvent::ReviewVerdict {
            pass: false,
            feedback: String::new(),
        });
        assert_eq!((r.tasks, r.passed), (0, 0));
        assert_eq!((r.verdicts, r.passed_verdicts), (2, 1));
        r.record_task(true);
        r.record_task(false);
        assert_eq!((r.tasks, r.passed), (2, 1));
        assert_eq!(r.success_rate(), 0.5);
    }
}
