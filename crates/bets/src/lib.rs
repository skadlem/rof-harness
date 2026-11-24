//! The capability bets, feature-flagged. Racing (C) is not wired yet.
//! Ablation order is mechanically necessary: commit predicate = satisfied claim,
//! so the claim field (B) precedes proof gating (A); racing (C) needs both.
//! Every bet emits ablation metrics from day one or the ablation has no data.

mod claim;
mod commit;
mod metrics;

pub use claim::{assess_claim, Claim, ClaimAssessment};
pub use commit::{gate_batch_commit, gate_commit, split_savepoint, CommitVerdict, Savepoint};
pub use metrics::AblationMetrics;
