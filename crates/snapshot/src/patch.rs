use crate::types::{PatchText, PATCH_MAX_BYTES, PATCH_MAX_LINES, PATCH_TRUNCATED_MARKER};

/// Whole lines only: a line that does not fit is dropped, so a multi-byte
/// codepoint is never sliced.
pub(crate) fn bound_patch(text: &str) -> PatchText {
    let mut out = String::new();
    let mut truncated = false;
    for (lines, line) in text.split_inclusive('\n').enumerate() {
        if lines >= PATCH_MAX_LINES || out.len() + line.len() > PATCH_MAX_BYTES {
            truncated = true;
            break;
        }
        out.push_str(line);
    }
    if truncated {
        out.push_str(PATCH_TRUNCATED_MARKER);
        out.push('\n');
    }
    PatchText {
        text: out,
        truncated,
    }
}

/// Splits `git diff --patch` text into the fragments
/// [`crate::TreeService::restore_hunks`] accepts: one `diff --git` file header
/// (`index` / `---` / `+++` lines included) directly followed by exactly one
/// `@@` hunk. Later hunks of the same file repeat that header, because `git
/// apply` rejects a bare `@@`. Fragments carry no trailing newline.
pub fn split_patch(patch: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut header = String::new();
    let mut cur: Option<String> = None;
    for line in patch.lines() {
        if line.starts_with("diff --git") {
            if let Some(done) = cur.take() {
                out.push(done);
            }
            header = line.to_string();
        } else if line.starts_with("@@") {
            if let Some(done) = cur.take() {
                out.push(done);
            }
            cur = Some(format!("{header}\n{line}"));
        } else if let Some(hunk) = cur.as_mut() {
            hunk.push('\n');
            hunk.push_str(line);
        } else {
            header.push('\n');
            header.push_str(line);
        }
    }
    if let Some(done) = cur {
        out.push(done);
    }
    out
}
