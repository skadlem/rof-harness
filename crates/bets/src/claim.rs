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
}
