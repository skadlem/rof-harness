#[derive(Debug, Clone, Default)]
pub struct BudgetCounters {
    pub steps: u32,
    pub tokens: u64,
    pub spent_cents: u64,
    pub refunds: u32,
    pub same_action_streak: u32,
    pub actions_this_trial: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetHalt {
    Steps,
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

/// Name of the tripped counter, verbatim in halt labels.
pub fn halt_name(h: &BudgetHalt) -> &'static str {
    match h {
        BudgetHalt::Steps => "steps",
        BudgetHalt::Tokens => "tokens",
        BudgetHalt::Wallclock => "wallclock",
        BudgetHalt::Spend => "spend",
        BudgetHalt::SameAction => "same-action",
        BudgetHalt::TrialActions => "trial-actions",
    }
}
