/// Below this many chars a window is too narrow to judge, so a `must_include`
/// windowed item that still does not fit after halvings is emitted at this
/// floor rather than silently cut.
pub const MIN_WINDOW: usize = 2_000;

/// Active file window in lines (SWE-agent: 30 lines −3.7pp, full file −5.3pp).
pub const WINDOW_LINES: usize = 100;

/// Window text on an anchor line with head+tail around it: `cap` chars centred
/// on the anchor substring, head+tail with an elision marker when absent.
pub fn window_anchored(_text: &str, _anchor: &str, _cap_chars: usize) -> String {
    let total = _text.chars().count();
    if total <= _cap_chars {
        return _text.to_string();
    }
    if !_anchor.is_empty() {
        if let Some(pos) = _text.find(_anchor) {
            let at = _text[..pos].chars().count();
            let start = at.saturating_sub(_cap_chars / 2);
            let win: String = _text.chars().skip(start).take(_cap_chars).collect();
            let end = start + _cap_chars.min(total - start);
            return format!("...[chars {start}..{end} of {total}]...\n{win}");
        }
    }
    let half = _cap_chars / 2;
    let head: String = _text.chars().take(half).collect();
    let tail: String = _text.chars().skip(total - half).collect();
    format!(
        "{head}\n...[{} chars elided]...\n{tail}",
        total - _cap_chars
    )
}

/// Char-boundary cut. Never splits a UTF-8 sequence (v1 context_edges lesson).
pub fn cut_chars(_text: &str, _max_chars: usize) -> &str {
    match _text.char_indices().nth(_max_chars) {
        Some((i, _)) => &_text[..i],
        None => _text,
    }
}
