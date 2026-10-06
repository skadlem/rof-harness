use crate::oracle::Verdict;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Per-instance report: capability numbers plus the scaffold-study metrics
/// that are the real objective (tokens-per-solved, no-action turns).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceReport {
    pub id: String,
    pub verdict: Verdict,
    /// Cumulative `TurnEnd.usage_totals`: `None` = the dump carried no TurnEnd
    /// at all (missing/truncated), so the counts are unknown — `null`, never 0.
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    /// Carried from `TurnEnd.usage_totals.cost_usd`: `None` = no billed turn
    /// was priced (serialises to `null`), never flattened to 0.0.
    pub dollars: Option<f64>,
    pub wall_secs: u64,
    pub steps: u32,
    pub halt_reason: Option<String>,
    pub patch_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub instances: Vec<InstanceReport>,
}

impl RunReport {
    /// Guardrail metric (≤1.3× budget): `None` when nothing solved, or when
    /// any instance's token counts are unknown (a partial sum would hide the
    /// missing dump behind a smaller number).
    pub fn tokens_per_solved(&self) -> Option<f64> {
        let solved = self
            .instances
            .iter()
            .filter(|r| r.verdict == Verdict::Resolved)
            .count();
        if solved == 0 {
            return None;
        }
        let total = self
            .instances
            .iter()
            .try_fold(0u64, |sum, r| Some(sum + r.tokens_in? + r.tokens_out?))?;
        Some(total as f64 / solved as f64)
    }
}

/// Build a per-instance report from a rof `--dump-events` JSONL dump. The
/// LAST `TurnEnd.usage_totals` is the run's cumulative bill (each TurnEnd
/// folds the running totals), `MessageEnd` count is the step count, and the
/// `RunEnd` outcome becomes `halt_reason`. A dump with no TurnEnd at all
/// (missing/truncated) leaves `tokens_in`/`tokens_out`/`dollars` `None` —
/// absence, never a zero. `wall_secs` and `patch` are
/// caller-measured (the dump carries neither). Typed event parsing: a schema
/// drift is a compile error here, never silently zero.
pub fn instance_report(
    id: &str,
    verdict: Verdict,
    events_jsonl: &Path,
    wall_secs: u64,
    patch: &str,
) -> std::io::Result<InstanceReport> {
    use agent_event::{AgentEvent, RunOutcome, UsageReport};
    let text = std::fs::read_to_string(events_jsonl)?;
    let mut totals: Option<UsageReport> = None;
    let mut steps = 0u32;
    let mut halt: Option<String> = None;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let ev: AgentEvent = serde_json::from_str(line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        match ev {
            AgentEvent::MessageEnd { .. } => steps += 1,
            AgentEvent::TurnEnd { usage_totals, .. } => totals = Some(usage_totals),
            AgentEvent::RunEnd { outcome, .. } => {
                halt = match outcome {
                    RunOutcome::Failed(r) => Some(r),
                    RunOutcome::Aborted => Some("aborted".into()),
                    RunOutcome::Passed => None,
                }
            }
            _ => {}
        }
    }
    Ok(InstanceReport {
        id: id.into(),
        verdict,
        tokens_in: totals.as_ref().map(|t| t.input_tokens),
        tokens_out: totals.as_ref().map(|t| t.output_tokens),
        dollars: totals.as_ref().and_then(|t| t.cost_usd),
        wall_secs,
        steps,
        halt_reason: halt,
        patch_digest: patch_digest(patch),
    })
}

/// Identity digest for grouping identical patches across runs. Not
/// cryptographic (`DefaultHasher` is per-toolchain); grouping, not integrity.
fn patch_digest(patch: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    patch.hash(&mut h);
    format!("{:016x}", h.finish())
}
