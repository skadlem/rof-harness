use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{
    AgentError, ControlAck, Message, MessageDelta, MessageId, Role, RunId, RunOutcome, ToolCallId,
    TurnEndReason, TurnId, UsageReport,
};

/// The live vocabulary. Flat, tagged, serializable: one enum, one emission seam.
/// Partials are never authoritative; the terminal frame is mandatory.
///
/// `Eq` is absent because `UsageReport.cost_usd` is an `f64`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    RunStart {
        run_id: RunId,
        goal: String,
    },
    RunEnd {
        outcome: RunOutcome,
        messages: Vec<Message>,
    },
    TurnStart {
        turn: TurnId,
    },
    TurnEnd {
        turn: TurnId,
        reason: TurnEndReason,
        /// Cumulative over the run so far, not just this turn.
        #[serde(default)]
        usage_totals: UsageReport,
    },
    MessageStart {
        id: MessageId,
        role: Role,
        partial: Message,
    },
    MessageUpdate {
        id: MessageId,
        delta: MessageDelta,
        partial: Message,
    },
    MessageEnd {
        id: MessageId,
        message: Message,
        interrupted: bool,
        /// This settle's provider usage; `None` = the provider reported none.
        usage: Option<UsageReport>,
    },
    ToolStart {
        id: ToolCallId,
        name: String,
        args: Value,
    },
    ToolEnd {
        id: ToolCallId,
        result: Value,
        is_error: bool,
    },
    Control(ControlAck),
    Error {
        error: AgentError,
    },
}
