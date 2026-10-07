use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;
use std::time::Duration;

/// Static caps. Every cap is finite: a cap that defaults to infinity is not a cap.
/// Single-trial design: the driver has no trial/refine boundaries (`run` is
/// one trial; `actions_per_trial` never resets), so there are no trials or
/// refines caps — the step cap and the per-run action cap are the live bounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    pub max_steps: NonZeroU32,
    pub warn_steps: NonZeroU32,
    pub max_tokens: u64,
    pub max_wallclock: Duration,
    pub max_spend_cents: Option<u64>,
    pub same_action_cycles: u32,
    /// Effectively per-RUN today: the counter never resets, so this cap fires
    /// once per run despite the per-trial name.
    pub actions_per_trial: u32,
    pub max_refunds: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Interactive,
    UnattendedBatch,
    LongTask,
    Subagent,
    Eval,
}

/// Spend on-value: $4.00. `None` in config means spend gate off by default.
pub const SPEND_ON_CENTS: u64 = 400;

pub(crate) fn nz(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).expect("budget cap is non-zero")
}

/// Capability presets. UnattendedBatch 20/12 is measured; LongTask 40/25 is
/// chosen-to-be-validated.
pub fn config_for(cap: Capability) -> BudgetConfig {
    let (steps, warn, wall_secs, spend) = match cap {
        Capability::Interactive => (100, 80, 3600, None),
        Capability::UnattendedBatch => (20, 12, 900, Some(SPEND_ON_CENTS)),
        // chosen-to-be-validated: 40/25 has no measured anchor yet.
        Capability::LongTask => (40, 25, 3600, Some(SPEND_ON_CENTS)),
        Capability::Subagent => (50, 40, 300, None),
        Capability::Eval => (20, 12, 3600, Some(SPEND_ON_CENTS)),
    };
    BudgetConfig {
        max_steps: nz(steps),
        warn_steps: nz(warn),
        max_tokens: 50_000,
        max_wallclock: Duration::from_secs(wall_secs),
        max_spend_cents: spend,
        same_action_cycles: 3,
        actions_per_trial: 30,
        max_refunds: steps / 4,
    }
}

/// Step-cap override: an explicit step budget wins over the capability
/// preset. Warn clamps to `min(warn, max-1)` headroom and refunds re-derive
/// as 25% of the step cap. Single implementation: callers (rof `budget_for`)
/// apply this instead of re-implementing the clamp math.
pub fn with_steps(base: BudgetConfig, steps: NonZeroU32) -> BudgetConfig {
    BudgetConfig {
        warn_steps: base.warn_steps.min(
            NonZeroU32::new(steps.get().saturating_sub(1).max(1)).expect("max(1) is non-zero"),
        ),
        max_refunds: steps.get() / 4,
        max_steps: steps,
        ..base
    }
}
