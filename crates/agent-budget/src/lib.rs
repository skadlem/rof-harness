//! AND-gate budget owner. See research/crate-agent-budget.md.
//! Step unit: one model call. Check [`BudgetGuard::may_step`] once at the
//! step head, after tool results are folded in and before the model call.
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

/// Static caps. Every cap is finite: a cap that defaults to infinity is not a cap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    pub max_steps: NonZeroU32,
    pub warn_steps: NonZeroU32,
    pub max_trials: NonZeroU32,
    pub max_refines: NonZeroU32,
    pub max_tokens: u64,
    pub max_wallclock: Duration,
    pub max_spend_cents: Option<u64>,
    pub same_action_cycles: u32,
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

#[derive(Debug, Clone, Default)]
pub struct BudgetCounters {
    pub steps: u32,
    pub trials: u32,
    pub refines: u32,
    pub tokens: u64,
    pub spent_cents: u64,
    pub refunds: u32,
    pub same_action_streak: u32,
    pub actions_this_trial: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetHalt {
    Steps,
    Trials,
    Refines,
    Tokens,
    Wallclock,
    Spend,
    SameAction,
    TrialActions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Nudge {
    WrapUp(String),
}

/// Terminal halt detail: which counter tripped and at what spent/limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetExceeded {
    pub halt: BudgetHalt,
    pub spent: u64,
    pub limit: u64,
}

impl BudgetExceeded {
    /// Terminal verdict text. An aborted task never appeals.
    pub fn verdict(&self) -> BudgetVerdict {
        BudgetVerdict {
            pass: false,
            feedback: format!(
                "aborted: {} budget exceeded ({} > {})",
                halt_name(&self.halt),
                self.spent,
                self.limit
            ),
        }
    }
    /// Aborted tasks never appeal: the budget, not the judge, decided.
    pub fn may_appeal(&self) -> bool {
        false
    }
}

/// Terminal verdict for a budget halt. `pass` is always false.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetVerdict {
    pub pass: bool,
    pub feedback: String,
}

impl BudgetVerdict {
    /// Aborted tasks never appeal.
    pub fn may_appeal(&self) -> bool {
        false
    }
}

fn halt_name(h: &BudgetHalt) -> &'static str {
    match h {
        BudgetHalt::Steps => "steps",
        BudgetHalt::Trials => "trials",
        BudgetHalt::Refines => "refines",
        BudgetHalt::Tokens => "tokens",
        BudgetHalt::Wallclock => "wallclock",
        BudgetHalt::Spend => "spend",
        BudgetHalt::SameAction => "same-action",
        BudgetHalt::TrialActions => "trial-actions",
    }
}

fn nz(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).expect("budget cap is non-zero")
}

pub struct BudgetGuard {
    config: BudgetConfig,
    counters: BudgetCounters,
    started: Instant,
    grace_used: Cell<bool>,
    nudge_fired: Cell<bool>,
}

impl BudgetGuard {
    pub fn new(config: BudgetConfig, started: Instant) -> Self {
        Self {
            config,
            counters: BudgetCounters::default(),
            started,
            grace_used: Cell::new(false),
            nudge_fired: Cell::new(false),
        }
    }

    pub fn config(&self) -> &BudgetConfig {
        &self.config
    }

    pub fn counters(&self) -> &BudgetCounters {
        &self.counters
    }

    pub fn counters_mut(&mut self) -> &mut BudgetCounters {
        &mut self.counters
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn grace_used(&self) -> bool {
        self.grace_used.get()
    }

    /// Nested scope with child isolation: fresh counters and latches, so
    /// child totals may exceed the parent cap by design.
    pub fn child_scope(&self) -> Self {
        Self {
            config: self.config.clone(),
            counters: BudgetCounters::default(),
            started: Instant::now(),
            grace_used: Cell::new(false),
            nudge_fired: Cell::new(false),
        }
    }

    /// Peek the first tripped counter without touching the grace latch.
    fn peek(&self) -> Option<BudgetHalt> {
        let c = &self.counters;
        let cfg = &self.config;
        if c.steps >= cfg.max_steps.get() {
            return Some(BudgetHalt::Steps);
        }
        if c.trials >= cfg.max_trials.get() {
            return Some(BudgetHalt::Trials);
        }
        if c.refines >= cfg.max_refines.get() {
            return Some(BudgetHalt::Refines);
        }
        if cfg.max_tokens > 0 && c.tokens >= cfg.max_tokens {
            return Some(BudgetHalt::Tokens);
        }
        if !cfg.max_wallclock.is_zero() && self.elapsed() >= cfg.max_wallclock {
            return Some(BudgetHalt::Wallclock);
        }
        if let Some(limit) = cfg.max_spend_cents {
            if c.spent_cents >= limit {
                return Some(BudgetHalt::Spend);
            }
        }
        if cfg.same_action_cycles > 0 && c.same_action_streak >= cfg.same_action_cycles {
            return Some(BudgetHalt::SameAction);
        }
        if cfg.actions_per_trial > 0 && c.actions_this_trial >= cfg.actions_per_trial {
            return Some(BudgetHalt::TrialActions);
        }
        None
    }

    /// AND-gate over all counters plus one latched grace step: the first
    /// exhaustion still returns `Ok` (the goodbye step) and latches; the
    /// next call returns the halt.
    pub fn may_step(&self) -> Result<(), BudgetHalt> {
        match self.peek() {
            None => Ok(()),
            Some(halt) => {
                if !self.grace_used.get() {
                    self.grace_used.set(true);
                    Ok(())
                } else {
                    Err(halt)
                }
            }
        }
    }

    /// Current halt with spent/limit, without consuming grace.
    pub fn exceeded(&self) -> Option<BudgetExceeded> {
        self.peek().map(|halt| {
            let (spent, limit) = self.spent_limit(&halt);
            BudgetExceeded { halt, spent, limit }
        })
    }

    fn spent_limit(&self, halt: &BudgetHalt) -> (u64, u64) {
        let c = &self.counters;
        let cfg = &self.config;
        match halt {
            BudgetHalt::Steps => (c.steps as u64, cfg.max_steps.get() as u64),
            BudgetHalt::Trials => (c.trials as u64, cfg.max_trials.get() as u64),
            BudgetHalt::Refines => (c.refines as u64, cfg.max_refines.get() as u64),
            BudgetHalt::Tokens => (c.tokens, cfg.max_tokens),
            BudgetHalt::Wallclock => (self.elapsed().as_secs(), cfg.max_wallclock.as_secs()),
            BudgetHalt::Spend => (c.spent_cents, cfg.max_spend_cents.unwrap_or(0)),
            BudgetHalt::SameAction => (c.same_action_streak as u64, cfg.same_action_cycles as u64),
            BudgetHalt::TrialActions => (c.actions_this_trial as u64, cfg.actions_per_trial as u64),
        }
    }

    pub fn record_step(&mut self) {
        self.counters.steps = self.counters.steps.saturating_add(1);
    }
    /// Refund iff the round produced no agentic progress. Steps-only
    /// (tokens/spend never refunded), saturates at zero, bounded by
    /// `max_refunds` (25% of the step cap).
    pub fn refund_step(&mut self) {
        if self.counters.refunds >= self.config.max_refunds {
            return;
        }
        if self.counters.steps > 0 {
            self.counters.steps -= 1;
            self.counters.refunds = self.counters.refunds.saturating_add(1);
        }
    }
    pub fn record_tokens(&mut self, n: u64) {
        self.counters.tokens = self.counters.tokens.saturating_add(n);
    }
    pub fn record_spend_cents(&mut self, n: u64) {
        self.counters.spent_cents = self.counters.spent_cents.saturating_add(n);
    }

    /// One-shot wrap-up notice with forced headroom (`min(warn, max-1)`).
    /// Contract: the caller appends the text to the mutable newest tool-result
    /// tail, never as a synthetic user/system row, and skips the append when
    /// that row is provider-cached (prefix cache stays intact). `None` after
    /// firing once.
    pub fn nudge_due(&self) -> Option<Nudge> {
        if self.nudge_fired.get() {
            return None;
        }
        let max = self.config.max_steps.get();
        let threshold = self
            .config
            .warn_steps
            .get()
            .min(max.saturating_sub(1).max(1));
        let steps_due = self.counters.steps >= threshold;
        let wall_due = !self.config.max_wallclock.is_zero()
            && self.elapsed() >= self.config.max_wallclock / 5 * 4;
        if steps_due || wall_due {
            self.nudge_fired.set(true);
            Some(Nudge::WrapUp(format!(
                "wrap up: step {}/{}; close out now",
                self.counters.steps, max
            )))
        } else {
            None
        }
    }
}

/// Capability presets. UnattendedBatch 20/12 is measured; LongTask 40/25 is
/// chosen-to-be-validated. See research/crate-agent-budget.md §2.3/§2.5.
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
        max_trials: nz(6),
        max_refines: nz(3),
        max_tokens: 50_000,
        max_wallclock: Duration::from_secs(wall_secs),
        max_spend_cents: spend,
        same_action_cycles: 3,
        actions_per_trial: 30,
        max_refunds: steps / 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unattended() -> BudgetGuard {
        BudgetGuard::new(config_for(Capability::UnattendedBatch), Instant::now())
    }

    /// Consume the one grace step, then return the terminal halt.
    fn terminal(g: &BudgetGuard) -> BudgetHalt {
        let _ = g.may_step();
        g.may_step().expect_err("must halt after grace")
    }

    #[test]
    fn and_gate_each_cap_halts_alone() {
        let g = unattended();
        assert!(g.may_step().is_ok());

        let mut g = unattended();
        g.counters_mut().steps = g.config().max_steps.get();
        assert_eq!(terminal(&g), BudgetHalt::Steps);

        let mut g = unattended();
        g.counters_mut().trials = g.config().max_trials.get();
        assert_eq!(terminal(&g), BudgetHalt::Trials);

        let mut g = unattended();
        g.counters_mut().refines = g.config().max_refines.get();
        assert_eq!(terminal(&g), BudgetHalt::Refines);

        let mut g = unattended();
        g.counters_mut().tokens = g.config().max_tokens;
        assert_eq!(terminal(&g), BudgetHalt::Tokens);

        let g = BudgetGuard::new(
            config_for(Capability::UnattendedBatch),
            Instant::now() - Duration::from_secs(10_000),
        );
        assert_eq!(terminal(&g), BudgetHalt::Wallclock);

        let mut g = unattended();
        g.counters_mut().spent_cents = SPEND_ON_CENTS;
        assert_eq!(terminal(&g), BudgetHalt::Spend);

        let mut g = unattended();
        g.counters_mut().same_action_streak = g.config().same_action_cycles;
        assert_eq!(terminal(&g), BudgetHalt::SameAction);

        let mut g = unattended();
        g.counters_mut().actions_this_trial = g.config().actions_per_trial;
        assert_eq!(terminal(&g), BudgetHalt::TrialActions);
    }

    #[test]
    fn refund_saturates_and_bounds() {
        let mut g = unattended();
        g.refund_step();
        assert_eq!((g.counters().steps, g.counters().refunds), (0, 0));

        for _ in 0..3 {
            g.record_step();
        }
        let bound = g.config().max_refunds;
        assert_eq!(bound, 5); // 25% of 20
        for _ in 0..10 {
            g.refund_step();
        }
        assert_eq!(g.counters().steps, 0);
        assert_eq!(g.counters().refunds, 3); // only 3 steps existed to refund
        assert!(g.counters().refunds <= bound);

        let mut g = unattended();
        for _ in 0..20 {
            g.record_step();
        }
        for _ in 0..20 {
            g.refund_step();
        }
        assert_eq!(g.counters().refunds, bound);
        assert_eq!(g.counters().steps, 20 - bound);
        let tokens_before = g.counters().tokens;
        g.record_tokens(100);
        g.refund_step();
        assert_eq!(g.counters().tokens, tokens_before + 100); // tokens never refunded
    }

    #[test]
    fn grace_fires_exactly_once() {
        let mut g = unattended();
        g.counters_mut().steps = g.config().max_steps.get();
        assert!(!g.grace_used());
        assert!(g.may_step().is_ok()); // the one goodbye step
        assert!(g.grace_used());
        assert_eq!(g.may_step(), Err(BudgetHalt::Steps));
        assert_eq!(g.may_step(), Err(BudgetHalt::Steps)); // stays halted
    }

    #[test]
    fn nudge_one_shot_with_headroom() {
        let mut g = unattended();
        assert!(g.config().warn_steps.get() < g.config().max_steps.get());
        assert!(g.nudge_due().is_none());
        g.counters_mut().steps = g.config().warn_steps.get() - 1;
        assert!(g.nudge_due().is_none());
        g.counters_mut().steps = g.config().warn_steps.get();
        assert!(g.nudge_due().is_some());
        assert!(g.nudge_due().is_none()); // latched

        let g = BudgetGuard::new(
            config_for(Capability::UnattendedBatch),
            Instant::now() - Duration::from_secs(800), // past 80% of 900s
        );
        assert!(g.nudge_due().is_some());
        assert!(g.nudge_due().is_none());
    }

    #[test]
    fn finite_defaults() {
        for cap in [
            Capability::Interactive,
            Capability::UnattendedBatch,
            Capability::LongTask,
            Capability::Subagent,
            Capability::Eval,
        ] {
            let c = config_for(cap);
            assert!(c.max_steps.get() > 0);
            assert!(c.warn_steps.get() > 0);
            assert!(c.warn_steps.get() < c.max_steps.get());
            assert!(c.max_trials.get() > 0);
            assert!(c.max_refines.get() > 0);
            assert!(c.max_tokens > 0);
            assert!(!c.max_wallclock.is_zero());
            assert!(c.same_action_cycles > 0);
            assert!(c.actions_per_trial > 0);
            if let Some(s) = c.max_spend_cents {
                assert!(s > 0);
            }
        }
    }

    #[test]
    fn capability_table() {
        let u = config_for(Capability::UnattendedBatch);
        assert_eq!((u.max_steps.get(), u.warn_steps.get()), (20, 12));
        let l = config_for(Capability::LongTask);
        assert_eq!((l.max_steps.get(), l.warn_steps.get()), (40, 25));
        for cap in [
            Capability::Interactive,
            Capability::UnattendedBatch,
            Capability::LongTask,
            Capability::Subagent,
            Capability::Eval,
        ] {
            let c = config_for(cap);
            assert_eq!(
                (c.max_trials.get(), c.max_refines.get(), c.max_tokens),
                (6, 3, 50_000)
            );
            assert_eq!((c.same_action_cycles, c.actions_per_trial), (3, 30));
            assert_eq!(c.max_refunds, c.max_steps.get() / 4);
        }
        assert_eq!(
            config_for(Capability::UnattendedBatch).max_spend_cents,
            Some(400)
        );
        assert_eq!(config_for(Capability::Interactive).max_spend_cents, None);
        assert_eq!(config_for(Capability::Subagent).max_spend_cents, None);
        assert_eq!(config_for(Capability::Subagent).max_steps.get(), 50);
    }

    #[test]
    fn verdict_is_terminal_and_never_appeals() {
        let mut g = unattended();
        g.counters_mut().steps = g.config().max_steps.get();
        let ex = g.exceeded().expect("halt detail");
        assert_eq!(ex.halt, BudgetHalt::Steps);
        let v = ex.verdict();
        assert!(!v.pass);
        assert!(v.feedback.contains("aborted: steps budget exceeded"));
        assert!(!v.may_appeal());
        assert!(!ex.may_appeal());
    }

    #[test]
    fn child_scope_is_isolated() {
        let mut parent = unattended();
        for _ in 0..20 {
            parent.record_step();
        }
        let child = parent.child_scope();
        assert_eq!(child.counters().steps, 0);
        assert!(child.may_step().is_ok()); // totals may exceed parent
    }
}
