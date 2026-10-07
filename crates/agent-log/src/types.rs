use serde::{Deserialize, Serialize};
use std::time::SystemTime;

pub type Seq = u64;
pub type ItemId = String;
pub type InputId = String;
pub type TurnId = String;
pub type CallId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputSource {
    External,
    Control,
    Crash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnEndReason {
    Completed,
    Error(String),
    Interrupted,
    Budget,
    MaxTokens,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryCode {
    ToolNotStarted,
    ToolOutcomeUnknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ItemKind {
    Header {
        version: u32,
        session_id: String,
        cwd: String,
        model: String,
    },
    Input {
        input_id: InputId,
        text: String,
        source: InputSource,
    },
    TurnStart {
        turn_id: TurnId,
        prev_turn_id: Option<TurnId>,
    },
    Assistant {
        message: serde_json::Value,
        stop_reason: String,
        interrupted: bool,
    },
    Attempt {
        error: String,
        will_retry: bool,
    },
    ToolCall {
        call_id: CallId,
        tool: String,
        args: serde_json::Value,
    },
    ToolResult {
        call_id: CallId,
        content: String,
        is_error: bool,
        recovery: Option<RecoveryCode>,
    },
    TurnEnd {
        turn_id: TurnId,
        reason: TurnEndReason,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub seq: Seq,
    pub id: ItemId,
    pub parent_id: Option<ItemId>,
    pub recorded_at: SystemTime,
    #[serde(flatten)]
    pub kind: ItemKind,
}

/// Current log format version. Unknown versions are rejected loudly on read;
/// there are no migrations.
pub const LOG_VERSION: u32 = 1;

/// Model-visible guidance carried by synthetic results for calls whose outcome
/// was never durably recorded. It tells the model to verify before retrying.
pub const UNKNOWN_OUTCOME_TEXT: &str = "The tool call was interrupted after it was recorded, but no result was durably recorded. Its outcome is unknown. Retry only if the operation is read-only or idempotent; if it may have side effects, first verify external state or ask the user. Do not retry blindly.";
