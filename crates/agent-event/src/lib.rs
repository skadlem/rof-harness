//! Live event vocabulary (TUI/headless contract). See research/crate-agent-event.md.

mod emitter;
mod event;
mod pairing;
mod types;

pub use emitter::Emitter;
pub use event::AgentEvent;
pub use pairing::check_pairing;
pub use types::{
    AgentError, ControlAck, ControlKind, ControlStatus, DeltaKind, Message, MessageDelta,
    MessageId, Role, RunId, RunOutcome, ToolCallId, TurnEndReason, TurnId, UsageReport,
};

#[cfg(test)]
mod tests;
