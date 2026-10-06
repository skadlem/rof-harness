use serde::{Deserialize, Serialize};

/// Per-rep outcomes for one cell: one task inside one arm of a comparison.
/// `reps` are the raw per-rep scores in [0, 1] the storage already carries
/// (matrix-v1: `tests_passed / (tests_passed + tests_failed)`); an empty
/// `reps` means the cell was never measured and can only withhold numbers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellReps {
    pub task: String,
    pub reps: Vec<f64>,
}

/// The same task measured in both arms — the pairing unit is the task, same
/// convention as [`crate::gate::paired_bootstrap`] (reps average inside a task first).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchedPair {
    pub control: CellReps,
    pub treatment: CellReps,
}

/// Report-honesty flags (pi `evals/report.ts:336-357`); no new statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReportFlag {
    /// Reps inside one cell disagree, so the cell is not a stable estimate.
    /// Raw per-rep scores are compared: 5/7 vs 6/7 flags, not just solved.
    Flaky,
    /// Control arm passes 100%: no headroom for a positive delta.
    ControlSaturatedPass,
    /// Control arm fails 100%: no headroom for a negative delta.
    ControlSaturatedFail,
    /// Treatment arm passes 100%.
    TreatmentSaturatedPass,
    /// Treatment arm fails 100%.
    TreatmentSaturatedFail,
}

/// One paired comparison (e.g. the swd cell: rof against a rival). Both pass
/// rates and `lift` are `None` (WITHHELD) when the pair set is empty, any
/// expected pair is blocked, or any cell is unmeasured — a number computed
/// from nothing is never emitted. Saturation flags therefore only fire on
/// published rates; a withheld rate makes no headroom claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparisonReport {
    pub control_pass_rate: Option<f64>,
    pub treatment_pass_rate: Option<f64>,
    pub lift: Option<f64>,
    pub flags: Vec<ReportFlag>,
    pub eligible_pairs: usize,
    pub blocked_pairs: usize,
}

impl ComparisonReport {
    /// Summarize `pairs` (tasks measured in both arms) with `blocked_pairs`
    /// expected pairs that could not be resolved (e.g. a rival cell with zero
    /// logs). Flaky compares the stored per-rep scores for exact equality —
    /// no tolerance, no new interval math is invented.
    pub fn from_pairs(pairs: &[MatchedPair], blocked_pairs: usize) -> Self {
        let eligible_pairs = pairs
            .iter()
            .filter(|p| !p.control.reps.is_empty() && !p.treatment.reps.is_empty())
            .count();
        let publish = blocked_pairs == 0 && eligible_pairs == pairs.len() && eligible_pairs > 0;
        let arm_rate = |side: fn(&MatchedPair) -> &CellReps| {
            pairs
                .iter()
                .map(|p| {
                    let reps = &side(p).reps;
                    reps.iter().sum::<f64>() / reps.len() as f64
                })
                .sum::<f64>()
                / pairs.len() as f64
        };
        let (control_pass_rate, treatment_pass_rate) = if publish {
            (
                Some(arm_rate(|p| &p.control)),
                Some(arm_rate(|p| &p.treatment)),
            )
        } else {
            (None, None)
        };
        let lift = match (control_pass_rate, treatment_pass_rate) {
            (Some(c), Some(t)) => Some(t - c),
            _ => None,
        };
        let unanimous = |cell: &CellReps| {
            cell.reps
                .first()
                .is_none_or(|first| cell.reps.iter().all(|r| r == first))
        };
        let mut flags = Vec::new();
        if control_pass_rate == Some(1.0) {
            flags.push(ReportFlag::ControlSaturatedPass);
        }
        if control_pass_rate == Some(0.0) {
            flags.push(ReportFlag::ControlSaturatedFail);
        }
        if treatment_pass_rate == Some(1.0) {
            flags.push(ReportFlag::TreatmentSaturatedPass);
        }
        if treatment_pass_rate == Some(0.0) {
            flags.push(ReportFlag::TreatmentSaturatedFail);
        }
        if pairs
            .iter()
            .any(|p| !unanimous(&p.control) || !unanimous(&p.treatment))
        {
            flags.push(ReportFlag::Flaky);
        }
        Self {
            control_pass_rate,
            treatment_pass_rate,
            lift,
            flags,
            eligible_pairs,
            blocked_pairs,
        }
    }
}
