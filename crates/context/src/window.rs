/// Char-boundary cut. Never splits a UTF-8 sequence (v1 context_edges lesson).
pub fn cut_chars(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}
