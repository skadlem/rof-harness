//! Bet B: frozen claim field. See crate docs for ablation order.
use serde::{Deserialize, Serialize};

/// Bet B: frozen claim field. The claim predicts the VERIFIER's verdict, not
/// the raw observation; the loop refuses to execute a step without one.
///
/// PreAct countermeasure format (stolen): state the predicted feedback type
/// AND the handling measure up front, so the mismatch branch is cheaper than
/// re-planning. E.g. predicted_verdict "cargo check passes",
/// on_mismatch "on E0308: localize to last hunk, revert it, retry once".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    /// e.g. "cargo check passes", "test auth_login flips green".
    pub predicted_verdict: String,
    /// Pre-stated remedy if the prediction misses (PreAct countermeasure format).
    pub on_mismatch: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimAssessment {
    Match,
    Mismatch,
}

/// History of predictions capped at Reflexion discipline (Ω=1-3 trials; code
/// loops run the low end). PreAct's Permanent-history mode helped 3/4 datasets
/// but raised refusals on the fourth — so watch [`ClaimHistory::refusal_rate`]:
/// if it climbs, shrink the cap instead of feeding more history.
pub const MAX_CLAIM_HISTORY: usize = 3;

/// Capped prediction history with an abstention/refusal watch.
#[derive(Debug, Clone, Default)]
pub struct ClaimHistory {
    recent: Vec<ClaimAssessment>,
    filed: u64,
    abstained: u64,
}

impl ClaimHistory {
    pub fn push(&mut self, claim: &Claim, assessment: ClaimAssessment) {
        if claim.predicted_verdict.trim().is_empty() {
            self.abstained += 1;
        } else {
            self.filed += 1;
        }
        if self.recent.len() >= MAX_CLAIM_HISTORY {
            self.recent.remove(0);
        }
        self.recent.push(assessment);
    }

    /// Share of claims filed with no prediction. Never vetoes acting;
    /// a climbing rate means the claim field is becoming a refusal vector.
    pub fn refusal_rate(&self) -> f64 {
        let total = self.filed + self.abstained;
        if total == 0 {
            0.0
        } else {
            self.abstained as f64 / total as f64
        }
    }
}

/// Deterministic tripwire: normalized case-insensitive containment either way,
/// so "cargo check passes" matches "cargo check passes with warnings".
/// Empty prediction = abstention: Match, so the field can say "no prediction"
/// without halting the loop (Look-Before-You-Leap abstention operating point).
pub fn assess_claim(claim: &Claim, observation: &str) -> ClaimAssessment {
    let predicted = claim.predicted_verdict.trim().to_lowercase();
    let observed = observation.trim().to_lowercase();
    if predicted.is_empty()
        || observed.contains(&predicted)
        || (!observed.is_empty() && predicted.contains(&observed))
    {
        ClaimAssessment::Match
    } else {
        ClaimAssessment::Mismatch
    }
}

/// Predictive-Intent dissonance score, binary form: 0.0 on match, 1.0 on
/// mismatch. The calibrator loop (foresight) consumes this, not prose.
pub fn dissonance_score(claim: &Claim, observation: &str) -> f64 {
    match assess_claim(claim, observation) {
        ClaimAssessment::Match => 0.0,
        ClaimAssessment::Mismatch => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(predicted: &str) -> Claim {
        Claim {
            predicted_verdict: predicted.to_string(),
            on_mismatch: "revert last hunk, retry once".to_string(),
        }
    }

    #[test]
    fn assess_matches_verdict_verbatim() {
        assert_eq!(
            assess_claim(&claim("cargo check passes"), "cargo check passes"),
            ClaimAssessment::Match
        );
    }

    #[test]
    fn assess_matches_case_insensitive_containment() {
        assert_eq!(
            assess_claim(
                &claim("cargo check passes"),
                "cargo check passes with warnings"
            ),
            ClaimAssessment::Match
        );
    }

    #[test]
    fn assess_mismatches_failed_proof() {
        assert_eq!(
            assess_claim(
                &claim("cargo check passes"),
                "error[E0308]: mismatched types"
            ),
            ClaimAssessment::Mismatch
        );
    }

    #[test]
    fn assess_empty_prediction_abstains_safe() {
        // Abstention must never veto acting: empty claim always matches.
        assert_eq!(
            assess_claim(&claim(""), "error[E0308]: mismatched types"),
            ClaimAssessment::Match
        );
        assert_eq!(assess_claim(&claim("   "), ""), ClaimAssessment::Match);
    }

    #[test]
    fn assess_empty_observation_mismatches_filed_claim() {
        assert_eq!(
            assess_claim(&claim("test auth_login flips green"), ""),
            ClaimAssessment::Mismatch
        );
    }

    #[test]
    fn dissonance_is_binary_match_signal() {
        assert_eq!(dissonance_score(&claim("ok"), "ok"), 0.0);
        assert_eq!(dissonance_score(&claim("ok"), "boom"), 1.0);
    }

    #[test]
    fn history_caps_at_reflexion_discipline() {
        let mut h = ClaimHistory::default();
        let c = claim("ok");
        for _ in 0..MAX_CLAIM_HISTORY + 2 {
            h.push(&c, ClaimAssessment::Match);
        }
        assert_eq!(h.recent.len(), MAX_CLAIM_HISTORY);
        assert_eq!(h.refusal_rate(), 0.0);
    }

    #[test]
    fn history_tracks_refusal_rate() {
        let mut h = ClaimHistory::default();
        h.push(&claim("ok"), ClaimAssessment::Match);
        h.push(&claim(""), ClaimAssessment::Match);
        assert_eq!(h.refusal_rate(), 0.5);
    }
}
