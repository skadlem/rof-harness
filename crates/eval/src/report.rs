use crate::oracle::{ApplyMode, Verdict};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Per-instance report: capability numbers plus the scaffold-study metrics
/// that are the real objective (tokens-per-solved, no-action turns).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceReport {
    pub id: String,
    /// SHA-256 of the frozen task inputs used by this attempt.
    #[serde(default)]
    pub task_digest: Option<String>,
    /// Immutable image ID of the running container, when inspected.
    #[serde(default)]
    pub image_id: Option<String>,
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
    /// Nonsecret resolved run configuration, absent in older reports.
    #[serde(default)]
    pub run_config: Option<serde_json::Value>,
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
    /// How the agent's workdir was seeded. This is the field whose ABSENCE
    /// voided 169 of 183 banked in-container rows: with the build-context seed
    /// the verifier tests structurally could not move, so those rows cannot
    /// answer a capability question at all. `ImageApp` copies the task image's
    /// `/app` (the workspace the verifier grades); `BuildContext` copies
    /// `<task>/environment`, which is the Docker build context and on several
    /// tasks is not the graded workspace. The two never pool. `None` = a report
    /// written before the field existed, i.e. fidelity unknown.
    #[serde(default)]
    pub workdir_seed: Option<WorkdirSeedLabel>,
    /// `reasoning_effort` level the agent leg sent on every request, if any.
    /// `None` = endpoint default (the frozen recipe); not a required field,
    /// so report files written before the knob existed read as "none sent",
    /// which is true. An effort arm never pools with frozen-recipe rows.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Initiative steering nudges the agent leg ran with (`Some(false)` =
    /// the ablation arm: no unverified-declare hold, no action directives, no
    /// wrap-up nudge; the workflow contract and rollback notice still ship).
    /// `None` = a report written before the knob existed.
    #[serde(default)]
    pub steer: Option<bool>,
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

/// Report-side spelling of the workdir seed (see `RunProvenance::workdir_seed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkdirSeedLabel {
    /// `cp -a /app/.` from the task image: the graded workspace.
    ImageApp,
    /// `<task>/environment`, the Docker build context: not the graded workspace.
    BuildContext,
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

/// Build a per-instance report from in-memory [`agent_event::AgentEvent`]s.
/// The LAST `TurnEnd.usage_totals` is the run's cumulative bill (each TurnEnd
/// folds the running totals), `MessageEnd` count is the step count, and the
/// `RunEnd` outcome becomes `halt_reason`. A stream with no TurnEnd at all
/// (missing/truncated), or carrying `wallclock-usage-unknown` /
/// `provider-usage-unknown`, leaves `tokens_in`/`tokens_out`/`dollars` `None` —
/// absence, never a zero.
pub fn instance_report_from_events(
    instance_id: String,
    verdict: Verdict,
    events: &[agent_event::AgentEvent],
    wall_ms: u64,
    apply_mode: ApplyMode,
) -> InstanceReport {
    use agent_event::{AgentEvent, RunOutcome, UsageReport};
    let mut totals: Option<UsageReport> = None;
    let mut steps = 0u32;
    let mut halt: Option<String> = None;
    let mut usage_unknown = false;
    for ev in events {
        match ev {
            AgentEvent::Error { error }
                if matches!(
                    error.code.as_str(),
                    "wallclock-usage-unknown" | "provider-usage-unknown"
                ) =>
            {
                usage_unknown = true;
            }
            AgentEvent::MessageEnd { .. } => steps += 1,
            AgentEvent::TurnEnd { usage_totals, .. } => totals = Some(usage_totals.clone()),
            AgentEvent::RunEnd { outcome, .. } => {
                halt = match outcome {
                    RunOutcome::Failed(r) => Some(r.clone()),
                    RunOutcome::Aborted => Some("aborted".into()),
                    RunOutcome::Passed => None,
                }
            }
            _ => {}
        }
    }
    InstanceReport {
        id: instance_id,
        task_digest: None,
        image_id: None,
        verdict,
        tokens_in: totals
            .as_ref()
            .filter(|_| !usage_unknown)
            .map(|t| t.input_tokens),
        tokens_out: totals
            .as_ref()
            .filter(|_| !usage_unknown)
            .map(|t| t.output_tokens),
        dollars: totals
            .as_ref()
            .filter(|_| !usage_unknown)
            .and_then(|t| t.cost_usd),
        wall_secs: wall_ms / 1000,
        steps,
        halt_reason: halt,
        patch_digest: String::new(),
        apply_mode,
        apply_detail: None,
    }
}

/// Build a per-instance report from a rof `--dump-events` JSONL dump plus
/// patch-apply provenance (`apply_mode` and applier stderr).
pub fn instance_report_with_mode(
    id: &str,
    verdict: Verdict,
    events_jsonl: &Path,
    wall_secs: u64,
    patch: &str,
    apply_mode: ApplyMode,
    apply_detail: Option<&str>,
) -> std::io::Result<InstanceReport> {
    let text = std::fs::read_to_string(events_jsonl)?;
    let events: Vec<agent_event::AgentEvent> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut r = instance_report_from_events(
        id.into(),
        verdict,
        &events,
        wall_secs.saturating_mul(1000),
        apply_mode,
    );
    r.patch_digest = patch_digest(patch);
    r.apply_detail = apply_detail.map(str::to_owned);
    Ok(r)
}

/// Identity digest for grouping identical patches across runs. Not
/// cryptographic (`DefaultHasher` is per-toolchain); grouping, not integrity.
pub fn patch_digest(patch: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    patch.hash(&mut h);
    format!("{:016x}", h.finish())
}
