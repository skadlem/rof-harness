use crate::tool::ToolResult;

/// Cap applied to every string handed to the model at the loop boundary.
pub const MAX_MODEL_CHARS: usize = 40_000;

/// Head+tail bound with omission marker; total never exceeds `max` (chars).
pub fn bound_text(s: &str, max: usize) -> String {
    const MARKER: &str = "\n...[omitted]...\n";
    let n = s.chars().count();
    if n <= max {
        return s.to_owned();
    }
    let m = MARKER.chars().count();
    if max <= m {
        return s.chars().take(max).collect();
    }
    let rest = max - m;
    let head = rest / 2 + rest % 2;
    let tail = rest / 2;
    let h: String = s.chars().take(head).collect();
    let t: String = s.chars().skip(n - tail).collect();
    format!("{h}{MARKER}{t}")
}

pub(crate) fn error_result(content: String) -> ToolResult {
    ToolResult {
        content: bound_text(&content, MAX_MODEL_CHARS),
        is_error: true,
    }
}
