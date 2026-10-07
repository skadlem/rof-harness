//! Budgeted prompt selection: one budget, dedupe, windowing for the file
//! map and named files. Rule: select what enters, never summarize the edit
//! surface. Volatile named files are must_include and excluded from mid-layer
//! double delivery: the caller keeps them out of the mid layer, this crate
//! delivers them last.

mod assemble;
mod collapse;
mod compact;
mod files;
mod types;
mod window;

pub use assemble::ContextAssembler;
pub use collapse::{collapse_boundary, COLLAPSE_HYSTERESIS, COLLAPSE_KEEP};
pub use compact::{
    compaction_due, cut_point, estimate_tokens, summary_payload, CompactMessage, CompactionConfig,
    UsageAnchor, SUMMARY_PROMPT, SUMMARY_SYSTEM, SUMMARY_TOOL_CAP,
};
pub use files::{file_map, named_file_contents, FILE_MAP_WALK_CAP};
pub use types::{ContextItem, Fidelity, ItemKey};
pub use window::{cut_chars, window_anchored, MIN_WINDOW, WINDOW_LINES};

#[cfg(test)]
mod tests;
