//! Prompt helpers: file map, named-file reads, collapse boundary, char
//! truncation, and compaction checkpoint estimation/cut-point/payload.

mod collapse;
mod compact;
mod files;
mod window;

pub use collapse::{collapse_boundary, COLLAPSE_HYSTERESIS, COLLAPSE_KEEP};
pub use compact::{
    compaction_due, cut_point, estimate_tokens, parse_checkpoint, render_checkpoint,
    summary_payload, summary_payload_with_sources, CheckpointEntry, CheckpointKind, CompactMessage,
    CompactionConfig, UsageAnchor, CHECKPOINT_PROMPT, SUMMARY_PROMPT, SUMMARY_SYSTEM,
    SUMMARY_TOOL_CAP,
};
pub use files::{file_map, named_file_contents, FILE_MAP_WALK_CAP};
pub use window::cut_chars;

#[cfg(test)]
mod tests;
