use crate::oracle::{ApplyMode, Verdict};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Strict-apply grades are not comparable with earlier fuzz-lenient
/// benchmark numbers: the same patch can grade differently, so any
/// cross-run comparison must check `InstanceReport.apply_mode` first.
pub const APPLY_COMPARABILITY_NOTE: &str =
    "Strict-apply grades are not comparable with earlier fuzz-lenient benchmark numbers.";

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
    /// Which patch applier graded this instance. Strict grades are not
    /// comparable with earlier fuzz-lenient benchmark numbers.
    #[serde(default)]
    pub apply_mode: ApplyMode,
    /// Applier stderr for the run (`None` = applier silent or no patch step).
    #[serde(default)]
    pub apply_detail: Option<String>,
}

/// Reward estimand: which grading path decided a run's verdicts. One
/// value per `rof eval` leg, never mixed in a comparison (see
/// `docs/roadmap.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Estimand {
    /// Plain `rof eval` (no `--agent`): the engine runs no solve and no
    /// verifier, so no grading path produces a reward — rows are typically
    /// `ErrorNoReport` — a scaffold, not a grade.
    Ungraded,
    /// `--agent`: the harbor CLI grades the oracle solution and the reward
    /// comes from `result.json`.
    HarborOracle,
    /// `--agent --in-container`: the verdict is the task-written reward
    /// file, with the verifier exit code as the no-artifact fallback.
    InContainerReward,
}

/// Harness specification for one run, written alongside its rows so a
/// report file identifies its own arm without driver-script convention:
/// which binary, which grading path, which budget recipe, which applier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunProvenance {
    /// Short git sha of the `rof` binary that produced the run, embedded
    /// at build time by `crates/rof/build.rs` (`git rev-parse --short
    /// HEAD`); `None` only if the value was not embedded, and "unknown"
    /// when git was unavailable — never fabricated.
    pub binary_git_sha: Option<String>,
    pub estimand: Estimand,
    /// Token/step/action caps resolved from this run's flags (the agent-leg
    /// recipe; an `Ungraded` run never applies a budget): explicit flags win
    /// over the frozen pilot recipe.
    pub budget_tokens: u64,
    pub budget_steps: u32,
    pub budget_actions: u32,
    /// Token-cap accounting shape for the agent leg (report label; the
    /// charging behavior lives in the loop). A `MissPlusOut` arm is a
    /// different estimand from a `Gross` arm — never mixed in one table.
    /// Required field: a provenance block without it fails loudly rather
    /// than silently reading as `Gross`.
    pub budget_shape: BudgetShapeLabel,
    /// Command-allowlist policy the agent leg ran under. A row produced with
    /// the allowlist skipped is a different protocol from a strict-list row
    /// (the agent could reach tools the task's tests require), so the two
    /// never pool in one table. Required field: a provenance block without
    /// it fails loudly rather than silently reading as `Allowlist`.
    pub exec_policy: ExecPolicyLabel,
    pub apply_mode: ApplyMode,
}

/// Report-side spelling of the command-allowlist policy. `Allowlist` = the
/// `--allow-cmd` prefix list; `Any` = the list is skipped (in-container eval,
/// where every exec is wrapped through the task container). Mirrors the tool
/// policy enum without a dependency edge; `rof` converts at the report site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecPolicyLabel {
    Allowlist,
    Any,
}

/// Report-side spelling of the token-cap shape (`Gross` = input+output,
/// the frozen recipe; `MissPlusOut` = (input − cache_read) + output). Mirrors
/// the charging enum in the loop crate without a dependency edge; `rof`
/// converts at the report site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BudgetShapeLabel {
    Gross,
    MissPlusOut,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub instances: Vec<InstanceReport>,
    /// `None` in a report written before provenance existed (the key is
    /// absent), so old report files still deserialize.
    #[serde(default)]
    pub provenance: Option<RunProvenance>,
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
    instance_report_with_mode(
        id,
        verdict,
        events_jsonl,
        wall_secs,
        patch,
        ApplyMode::Strict,
        None,
    )
}

/// [`instance_report`] plus the patch-apply provenance: which [`ApplyMode`]
/// graded the instance and the applier's stderr. Strict grades are not
/// comparable with earlier fuzz-lenient benchmark numbers
/// ([`APPLY_COMPARABILITY_NOTE`]).
pub fn instance_report_with_mode(
    id: &str,
    verdict: Verdict,
    events_jsonl: &Path,
    wall_secs: u64,
    patch: &str,
    apply_mode: ApplyMode,
    apply_detail: Option<&str>,
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
        apply_mode,
        apply_detail: apply_detail.map(str::to_owned),
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
