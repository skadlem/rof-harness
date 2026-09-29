//! The three capability bets, feature-flagged. See research/bets-prior-art.md.
//! Ablation order is mechanically necessary: commit predicate = satisfied claim,
//! so the claim field (B) precedes proof gating (A); racing (C) needs both.
//! Every bet emits ablation metrics from day one or the ablation has no data.
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

/// Bet A: proof-gated batch commit with proven-hunk savepoints.
/// Commit succeeds only on the verifier cascade; failure keeps the verified
/// prefix and rolls back only the failing suffix (savepoints, not all-or-nothing).
///
/// Aider `/undo` guard checklist (stolen — enforce before any partial rollback):
/// refuse if the commit was pushed, refuse dirty overlaps between agent and
/// user edits, refuse multi-parent merges, verify each file existed in the
/// prior tree before restoring. Plus codex-rewind's rule: never infer absence
/// from a missing path — guessing wrong destroys work never ours to remove.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Savepoint {
    pub kept_hunks: Vec<String>,
    pub reverted_hunks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommitVerdict {
    Committed,
    Partial { savepoint: Savepoint },
    Aborted { reason: String },
}

/// Proof = the predicted verifier verdict, satisfied. Single-proof fast path.
pub fn gate_commit(claim: &Claim, proof_passed: bool) -> CommitVerdict {
    if proof_passed {
        CommitVerdict::Committed
    } else {
        CommitVerdict::Aborted {
            reason: format!("claim unsatisfied: {}", claim.predicted_verdict),
        }
    }
}

/// Keep the verified leading prefix, revert from the first failure on.
/// A later hunk that passed in isolation still reverts: it was verified
/// against uncommitted state, so it is not proven. Never all-or-nothing.
pub fn split_savepoint(hunks: &[(String, bool)]) -> Savepoint {
    let first_fail = hunks.iter().position(|(_, ok)| !ok).unwrap_or(hunks.len());
    Savepoint {
        kept_hunks: hunks[..first_fail].iter().map(|(h, _)| h.clone()).collect(),
        reverted_hunks: hunks[first_fail..].iter().map(|(h, _)| h.clone()).collect(),
    }
}

/// Batch commit: all proven → Committed; verified prefix exists → Partial with
/// the savepoint; nothing verified (empty batch or first hunk failed) → Aborted.
pub fn gate_batch_commit(claim: &Claim, hunks: &[(String, bool)]) -> CommitVerdict {
    if hunks.is_empty() {
        return CommitVerdict::Aborted {
            reason: "no hunks proven".to_string(),
        };
    }
    let savepoint = split_savepoint(hunks);
    if savepoint.reverted_hunks.is_empty() {
        CommitVerdict::Committed
    } else if savepoint.kept_hunks.is_empty() {
        CommitVerdict::Aborted {
            reason: format!(
                "claim unsatisfied at first hunk: {}",
                claim.predicted_verdict
            ),
        }
    } else {
        CommitVerdict::Partial { savepoint }
    }
}

/// Bet C (renamed: sandboxed candidate racing): divergent candidate actions in
/// isolated views, winner selected by verifier score — a capability mechanism,
/// not latency prefetch. Shadow-view substrate is future work (Cordon's
/// shadow-state trick: agent reads its own uncommitted work from a
/// transaction-scoped view); v1 races on snapshot overlays.
///
/// Cost guardrails: at most `max_branches` branches race; a branch spending
/// over `max_extra_spend_cents` is disqualified. Copy-on-write init and early
/// termination of under-performing branches (ACID-Agent) keep the race inside
/// the ≤1.3× tokens-per-solved-task guardrail; cumulative spend accounting is
/// the loop's job via [`AblationMetrics::branch_spend_cents`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RaceConfig {
    pub max_branches: u32,
    pub max_extra_spend_cents: u64,
}

impl Default for RaceConfig {
    fn default() -> Self {
        // chosen: 2 branches = spex's 2-slot shadow-queue discipline (measured
        // lossless at 13.9% wall saving on SWE-bench Verified); $2 cap is
        // chosen-to-validate via branch_spend_cents per solved task.
        Self {
            max_branches: 2,
            max_extra_spend_cents: 200,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BranchOutcome {
    pub branch_id: u32,
    pub verifier_score: f64,
    pub spend_cents: u64,
}

/// Verifier-score winner among the first `max_branches` branches that fit the
/// spend cap. Ties keep the earliest branch. None if nothing qualifies.
pub fn pick_winner(outcomes: &[BranchOutcome], config: &RaceConfig) -> Option<u32> {
    outcomes
        .iter()
        .take(config.max_branches as usize)
        .filter(|o| o.spend_cents <= config.max_extra_spend_cents)
        .max_by(|a, b| {
            a.verifier_score
                .partial_cmp(&b.verifier_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|o| o.branch_id)
}

/// Ablation metrics. The loop reads these; the B→+A→+C ablation decides on them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AblationMetrics {
    pub rollbacks: u64,
    pub proven_hunks: u64,
    pub claim_mismatches: u64,
    pub branch_wins: u64,
    pub branch_spend_cents: u64,
}

impl AblationMetrics {
    pub fn note_assessment(&mut self, assessment: ClaimAssessment) {
        if assessment == ClaimAssessment::Mismatch {
            self.claim_mismatches += 1;
        }
    }

    pub fn note_commit(&mut self, verdict: &CommitVerdict) {
        match verdict {
            CommitVerdict::Committed => self.proven_hunks += 1,
            CommitVerdict::Partial { savepoint } => {
                self.proven_hunks += savepoint.kept_hunks.len() as u64;
                self.rollbacks += 1;
            }
            CommitVerdict::Aborted { .. } => self.rollbacks += 1,
        }
    }

    pub fn note_race(&mut self, winner: Option<u32>, raced: &[BranchOutcome]) {
        if winner.is_some() {
            self.branch_wins += 1;
        }
        self.branch_spend_cents += raced.iter().map(|b| b.spend_cents).sum::<u64>();
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

    #[test]
    fn gate_single_proof_commits_or_aborts() {
        let c = claim("cargo check passes");
        assert_eq!(gate_commit(&c, true), CommitVerdict::Committed);
        assert!(matches!(
            gate_commit(&c, false),
            CommitVerdict::Aborted { .. }
        ));
    }

    #[test]
    fn gate_batch_all_proven_commits() {
        let c = claim("all green");
        let hunks = vec![("h1".to_string(), true), ("h2".to_string(), true)];
        assert_eq!(gate_batch_commit(&c, &hunks), CommitVerdict::Committed);
    }

    #[test]
    fn gate_batch_partial_keeps_verified_prefix() {
        let c = claim("all green");
        // h4 passed in isolation but sits past the first failure: not proven.
        let hunks = vec![
            ("h1".to_string(), true),
            ("h2".to_string(), true),
            ("h3".to_string(), false),
            ("h4".to_string(), true),
        ];
        match gate_batch_commit(&c, &hunks) {
            CommitVerdict::Partial { savepoint } => {
                assert_eq!(savepoint.kept_hunks, vec!["h1", "h2"]);
                assert_eq!(savepoint.reverted_hunks, vec!["h3", "h4"]);
            }
            v => panic!("expected Partial, got {v:?}"),
        }
    }

    #[test]
    fn gate_batch_first_failure_aborts() {
        let c = claim("all green");
        let hunks = vec![("h1".to_string(), false), ("h2".to_string(), true)];
        assert!(matches!(
            gate_batch_commit(&c, &hunks),
            CommitVerdict::Aborted { .. }
        ));
        assert!(matches!(
            gate_batch_commit(&c, &[]),
            CommitVerdict::Aborted { .. }
        ));
    }

    #[test]
    fn race_default_caps_two_branches() {
        assert_eq!(RaceConfig::default().max_branches, 2);
    }

    #[test]
    fn pick_winner_takes_highest_verifier_score() {
        let cfg = RaceConfig {
            max_branches: 4,
            max_extra_spend_cents: 1000,
        };
        let outcomes = vec![
            BranchOutcome {
                branch_id: 0,
                verifier_score: 0.6,
                spend_cents: 10,
            },
            BranchOutcome {
                branch_id: 1,
                verifier_score: 0.9,
                spend_cents: 20,
            },
        ];
        assert_eq!(pick_winner(&outcomes, &cfg), Some(1));
    }

    #[test]
    fn pick_winner_enforces_spend_cap() {
        let cfg = RaceConfig {
            max_branches: 4,
            max_extra_spend_cents: 100,
        };
        let outcomes = vec![
            BranchOutcome {
                branch_id: 0,
                verifier_score: 0.95,
                spend_cents: 500,
            },
            BranchOutcome {
                branch_id: 1,
                verifier_score: 0.7,
                spend_cents: 50,
            },
        ];
        assert_eq!(pick_winner(&outcomes, &cfg), Some(1));
        let all_over = vec![BranchOutcome {
            branch_id: 0,
            verifier_score: 0.95,
            spend_cents: 500,
        }];
        assert_eq!(pick_winner(&all_over, &cfg), None);
    }

    #[test]
    fn pick_winner_enforces_branch_cap_and_empty() {
        let cfg = RaceConfig::default(); // max_branches = 2
        let outcomes = vec![
            BranchOutcome {
                branch_id: 0,
                verifier_score: 0.5,
                spend_cents: 10,
            },
            BranchOutcome {
                branch_id: 1,
                verifier_score: 0.6,
                spend_cents: 10,
            },
            BranchOutcome {
                branch_id: 2,
                verifier_score: 0.99,
                spend_cents: 10,
            },
        ];
        assert_eq!(pick_winner(&outcomes, &cfg), Some(1));
        assert_eq!(pick_winner(&[], &cfg), None);
    }

    #[test]
    fn metrics_increment_on_every_path() {
        let mut m = AblationMetrics::default();
        m.note_assessment(ClaimAssessment::Match);
        assert_eq!(m.claim_mismatches, 0);
        m.note_assessment(ClaimAssessment::Mismatch);
        assert_eq!(m.claim_mismatches, 1);

        m.note_commit(&CommitVerdict::Committed);
        assert_eq!(m.proven_hunks, 1);
        m.note_commit(&CommitVerdict::Partial {
            savepoint: Savepoint {
                kept_hunks: vec!["h1".to_string(), "h2".to_string()],
                reverted_hunks: vec!["h3".to_string()],
            },
        });
        assert_eq!(m.proven_hunks, 3);
        assert_eq!(m.rollbacks, 1);
        m.note_commit(&CommitVerdict::Aborted {
            reason: "x".to_string(),
        });
        assert_eq!(m.rollbacks, 2);

        let raced = vec![
            BranchOutcome {
                branch_id: 0,
                verifier_score: 0.9,
                spend_cents: 30,
            },
            BranchOutcome {
                branch_id: 1,
                verifier_score: 0.4,
                spend_cents: 20,
            },
        ];
        m.note_race(Some(0), &raced);
        assert_eq!(m.branch_wins, 1);
        assert_eq!(m.branch_spend_cents, 50);
        m.note_race(None, &[]);
        assert_eq!(m.branch_wins, 1);
        assert_eq!(m.branch_spend_cents, 50);
    }
}
