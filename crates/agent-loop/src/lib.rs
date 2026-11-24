//! Turn/step driver: the headless multi-tick [`run`].
//!
//! [`run`] is the shipped assembly: sequential `complete` + inline tool
//! execute over one step head and termination order, with the durable log
//! synced before every live frame.

mod budget_nudge;
mod proof;
mod request;
mod run;
mod state;
mod verify;

#[cfg(test)]
mod tests;

pub use budget_nudge::IncentivesLevel;
pub use proof::{BetsHook, NoBets};
pub use run::{run, BudgetShape, Run, RunConfig};
pub use state::{
    turn_end_reason_to_event, Checkpoint, ClaimOutcome, Experiment, FailureKind, Input, LoopState,
    Outcome, Phase, PhaseVerdict, QueuedInput, ToolCallState,
};
pub use verify::VerifyState;

pub(crate) use proof::{
    batch_hunks, incremental_hunks, note_tool_execution, outcome_to_result, refund_batch,
    settle_tool_tail, snapshot_batch, ROLLBACK_NOTICE,
};
pub(crate) use request::{build_request, checkpoint, freeze_context_head};
pub(crate) use state::{append_to, outcome_log_reason, turn_id};
pub(crate) use verify::is_edit_tool;
