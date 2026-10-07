//! The three capability bets, feature-flagged.
//! Ablation order is mechanically necessary: commit predicate = satisfied claim,
//! so the claim field (B) precedes proof gating (A); racing (C) needs both.
//! Every bet emits ablation metrics from day one or the ablation has no data.

mod claim;
mod commit;
mod metrics;
mod race;

pub use claim::{
    assess_claim, dissonance_score, Claim, ClaimAssessment, ClaimHistory, MAX_CLAIM_HISTORY,
};
pub use commit::{gate_batch_commit, gate_commit, split_savepoint, CommitVerdict, Savepoint};
pub use metrics::AblationMetrics;
pub use race::{pick_winner, BranchOutcome, RaceConfig};
