use super::*;
use crate::config::nz;
use std::time::{Duration, Instant};

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
    assert!(g.may_step().is_ok()); // the one goodbye step
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
        assert_eq!(c.max_tokens, 50_000);
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
fn step_override_clamps_warn_and_rederives_refunds() {
    // UnattendedBatch preset is 20 steps / warn 12.
    let small = with_steps(config_for(Capability::UnattendedBatch), nz(7));
    assert_eq!(small.max_steps.get(), 7);
    assert_eq!(small.warn_steps.get(), 6); // min(12, 7-1): headroom
    assert_eq!(small.max_refunds, 1); // 7 / 4
                                      // A raise keeps the preset warn and scales refunds.
    let big = with_steps(config_for(Capability::UnattendedBatch), nz(30));
    assert_eq!(big.max_steps.get(), 30);
    assert_eq!(big.warn_steps.get(), 12);
    assert_eq!(big.max_refunds, 7);
    // Degenerate floor: warn keeps headroom at 1, refunds hit 0.
    let one = with_steps(config_for(Capability::UnattendedBatch), nz(1));
    assert_eq!((one.max_steps.get(), one.warn_steps.get()), (1, 1));
    assert_eq!(one.max_refunds, 0);
    // Everything else rides the preset untouched.
    assert_eq!(small.max_tokens, 50_000);
    assert_eq!(small.actions_per_trial, 30);
    assert_eq!(small.max_spend_cents, Some(SPEND_ON_CENTS));
}
