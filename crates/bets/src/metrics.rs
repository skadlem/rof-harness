use crate::{BranchOutcome, ClaimAssessment, CommitVerdict};
use serde::{Deserialize, Serialize};

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
    use crate::Savepoint;

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
