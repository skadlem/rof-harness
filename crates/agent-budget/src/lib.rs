//! AND-gate budget owner. See research/crate-agent-budget.md.
//! Step unit: one model call. Check [`BudgetGuard::may_step`] once at the
//! step head, after tool results are folded in and before the model call.

mod config;
mod guard;
mod types;

pub use config::{config_for, with_steps, BudgetConfig, Capability, SPEND_ON_CENTS};
pub use guard::BudgetGuard;
pub use types::{halt_name, BudgetCounters, BudgetExceeded, BudgetHalt, Nudge};

#[cfg(test)]
mod tests;
