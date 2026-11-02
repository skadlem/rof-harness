//! Retain known provider spend when an in-flight completion is cancelled.
use super::LlmUsage;
use crate::obs::{TraceEvent, TraceSink};
use std::{future::Future, sync::Arc, time::Instant};

tokio::task_local! {
    static TRACE: Arc<TraceSink>;
}

/// Scope a task's model calls to its trace. Normal returns remain recorded by
/// the caller; only interrupted calls flush usage through their drop guard.
pub async fn with_usage_trace<F: Future>(trace: Arc<TraceSink>, future: F) -> F::Output {
    TRACE.scope(trace, future).await
}

pub(super) struct InterruptedUsage {
    trace: Option<Arc<TraceSink>>,
    model: String,
    usage: LlmUsage,
    started: Instant,
}

impl InterruptedUsage {
    pub(super) fn new(model: &str) -> Self {
        Self {
            trace: TRACE.try_with(Arc::clone).ok(),
            model: model.to_string(),
            usage: LlmUsage::default(),
            started: Instant::now(),
        }
    }
    pub(super) fn observe(&mut self, usage: &LlmUsage) {
        self.usage.add(usage);
    }
    pub(super) fn disarm(&mut self) {
        self.trace = None;
    }
}

impl Drop for InterruptedUsage {
    fn drop(&mut self) {
        if self.usage.attempts == 0 {
            return;
        }
        if let Some(trace) = self.trace.take() {
            trace.emit(TraceEvent::ModelCall {
                agent: "interrupted".into(),
                model: self.model.clone(),
                input_tokens: self.usage.input_tokens,
                output_tokens: self.usage.output_tokens,
                cached_input_tokens: self.usage.cached_input_tokens,
                cost_usd: self.usage.cost_usd,
                attempts: self.usage.attempts,
                latency_ms: self
                    .usage
                    .latency_ms
                    .saturating_add(self.started.elapsed().as_millis() as u64),
            });
        }
    }
}
