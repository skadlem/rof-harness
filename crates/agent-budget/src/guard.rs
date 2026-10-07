use crate::config::BudgetConfig;
use crate::types::{BudgetCounters, BudgetExceeded, BudgetHalt, Nudge};
use std::cell::Cell;
use std::time::{Duration, Instant};

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

    /// Peek the first tripped counter without touching the grace latch.
    /// No trials/refines arms: the driver is single-trial (no trial/refine
    /// boundaries exist in `run`, so those counters could never increment
    /// and the arms could never fire). The per-run action cap
    /// (`TrialActions`, which never resets) is the live trial boundary.
    fn peek(&self) -> Option<BudgetHalt> {
        let c = &self.counters;
        let cfg = &self.config;
        if c.steps >= cfg.max_steps.get() {
            return Some(BudgetHalt::Steps);
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
