//! Append-only durable Item log (JSONL). See `research/crate-agent-log.md`.
//!
//! Locked rules: pre-effect items are appended and flushed before the effect;
//! a failed pre-effect append is a hard turn failure (propagate it and end the
//! turn, never run the effect from process-only state). One file has one
//! `LogWriter`. Framing is LF-only over raw bytes: `U+2028`/`U+2029` inside
//! strings are data, never record boundaries.

mod reader;
mod types;
mod writer;

pub use reader::read_log;
pub use types::{
    CallId, InputId, InputSource, Item, ItemId, ItemKind, RecoveryCode, Seq, TurnEndReason, TurnId,
    LOG_VERSION, UNKNOWN_OUTCOME_TEXT,
};
pub use writer::{append_item, LogWriter};

#[cfg(test)]
mod tests;
