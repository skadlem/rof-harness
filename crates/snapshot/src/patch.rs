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
