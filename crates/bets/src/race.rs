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
/// the loop's job via [`crate::AblationMetrics::branch_spend_cents`].
///
/// Unwired in `agent-loop` (with [`crate::AblationMetrics`]) until the later B→+A→+C ablation.
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
        // Strict win only: ties keep the earliest branch (max_by would
        // return the last of several maxima, contradicting the doc).
        .reduce(|best, o| {
            if o.verifier_score > best.verifier_score {
                o
            } else {
                best
            }
        })
        .map(|o| o.branch_id)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn pick_winner_tie_keeps_earliest_branch() {
        let cfg = RaceConfig {
            max_branches: 4,
            max_extra_spend_cents: 100,
        };
        let outcomes = vec![
            BranchOutcome {
                branch_id: 0,
                verifier_score: 0.9,
                spend_cents: 10,
            },
            BranchOutcome {
                branch_id: 1,
                verifier_score: 0.9,
                spend_cents: 20,
            },
        ];
        assert_eq!(pick_winner(&outcomes, &cfg), Some(0));
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
}
