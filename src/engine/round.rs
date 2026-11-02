//! Lifecycle transitions shared by planned and direct execution. This module
//! owns evidence and failure handling; each mode retains its verdict policy.
use super::{Budget, TreeDiff, TreeService};
use crate::obs::{TraceEvent, TraceSink};

#[derive(Debug, thiserror::Error)]
pub(crate) enum RoundStartError {
    #[error("aborted: token budget exceeded ({spent} > {limit})")]
    Budget { spent: u64, limit: u64 },
    #[error("harness: git baseline failed: {0}")]
    Baseline(String),
}

pub(crate) fn begin(
    trace: &TraceSink,
    task: &str,
    limit: u64,
    budget: &Budget,
    tree: &TreeService,
) -> Result<(), RoundStartError> {
    if let Some(spent) = budget.exceeded() {
        trace.emit(TraceEvent::BudgetExceeded {
            task: task.to_string(),
            tokens: spent,
            limit,
        });
        return Err(RoundStartError::Budget { spent, limit });
    }
    tree.baseline().map_err(|error| {
        trace.emit(TraceEvent::ModelError {
            agent: "tree".into(),
            error: error.to_string(),
        });
        RoundStartError::Baseline(error.to_string())
    })
}

pub(crate) fn diff(trace: &TraceSink, tree: &TreeService) -> anyhow::Result<TreeDiff> {
    tree.diff().inspect_err(|error| {
        trace.emit(TraceEvent::ModelError {
            agent: "tree".into(),
            error: error.to_string(),
        })
    })
}

pub(crate) fn rollback(trace: &TraceSink, tree: &TreeService) -> anyhow::Result<()> {
    tree.rollback().inspect_err(|error| {
        trace.emit(TraceEvent::ModelError {
            agent: "tree".into(),
            error: format!("rollback failed; execution stopped: {error}"),
        })
    })?;
    trace.emit(TraceEvent::DiffSnapshot {
        names: Vec::new(),
        stat: String::new(),
        patch: String::new(),
        truncated: false,
    });
    Ok(())
}
