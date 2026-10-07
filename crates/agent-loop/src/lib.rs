//! Turn/step driver: headless multi-tick [`run`] plus the channel test
//! harness [`drive_tick`].
//!
//! [`run`] is the headless multi-tick assembly: sequential `complete` +
//! inline tool execute over the same step-head and termination order as
//! [`drive_tick`]. Channel-driven traffic still enters as scripted
//! [`ProviderMsg`] / [`ToolMsg`] fakes into [`drive_tick`] for select!-shape
//! tests.

mod budget_nudge;
mod gate;
mod proof;
mod request;
mod run;
mod state;
mod verify;

#[cfg(test)]
mod tests;

pub use budget_nudge::IncentivesLevel;
pub use gate::{EffectGate, GateError, GateStatus};
pub use proof::{BetsHook, NoBets};
pub use run::{drive_tick, run, Run, RunConfig};
pub use state::{
    turn_end_reason_to_event, Checkpoint, ClaimOutcome, FailureKind, InFlight, Input, LoopState,
    Outcome, Phase, PhaseVerdict, ProviderMsg, QueuedInput, ToolCallState, ToolMsg, TurnGuard,
};
pub use verify::VerifyState;

pub(crate) use proof::{
    batch_hunks, incremental_hunks, note_tool_execution, outcome_to_result, refund_batch,
    settle_tool_msg, settle_tool_tail, snapshot_batch, ROLLBACK_NOTICE,
};
pub(crate) use request::{build_request, checkpoint, pin_snapshot};
pub(crate) use state::{append_to, outcome_log_reason, turn_id};
