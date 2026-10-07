//! Git overlay transactions over a task copy: baseline / diff / rollback.
//! Fixed identity (`rof@local`), copied-in hooks off, rename detection off
//! (`--no-renames`), patch text cut to 8KiB / 200 lines. Git is never on
//! any command allowlist; rollback runs only when a retry follows, so the
//! final tree stays readable for post-mortem.

mod git;
mod patch;
mod types;

pub use git::{workdir_state, TreeService};
pub use types::{
    DiffSummary, PatchText, SnapshotError, WorkdirState, PATCH_MAX_BYTES, PATCH_MAX_LINES,
    PATCH_TRUNCATED_MARKER,
};
