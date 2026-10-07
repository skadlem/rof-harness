use serde::{Deserialize, Serialize};
use std::io::Error;

/// Same change set as [`crate::TreeService::diff`]: changed paths, stat evidence,
/// plus the untracked subset (status alone would miss created files).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffSummary {
    pub changed: Vec<String>,
    pub stat: String,
    pub untracked: Vec<String>,
}

impl DiffSummary {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
    }
}

/// Error type for [`crate::TreeService::patch_since_start_full`]: the same I/O
/// error the git-overlay helpers already return, named so the
/// deliverable-patch contract spells its failure mode.
pub type SnapshotError = Error;

/// Patch-text bounds: either cap alone leaves a hole (many tiny lines fit
/// in few bytes; one minified line is one line however long).
pub const PATCH_MAX_BYTES: usize = 8 * 1024;
pub const PATCH_MAX_LINES: usize = 200;
/// Appended on a cut so the text is honest on its own, in a pane and in JSONL.
pub const PATCH_TRUNCATED_MARKER: &str = "… [diff truncated: harness evidence bound reached]";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PatchText {
    pub text: String,
    pub truncated: bool,
}

/// Workdir cleanliness for a path, for pre-flight guards. Ignored files
/// never count (plain porcelain omits them): only tracked edits and
/// non-ignored untracked files read as [`WorkdirState::Dirty`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkdirState {
    NotARepo,
    Clean,
    Dirty,
}
