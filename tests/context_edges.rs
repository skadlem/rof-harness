//! Char-boundary regressions. Two sites sliced `str` by raw byte index, and a
//! multi-byte character at the cut panicked the run ("start byte index 1 is
//! not a char boundary"): the retriever's hint extraction (fed a model-authored
//! patch anchor by the refused-patch re-read) and the context builder's head/
//! tail truncation. Both are exercised through their public entry points.

use rof::config::TokenBudgets;
use rof::context::CtxState;
use rof::context::{window_on, ContextBuilder};

#[test]
fn window_on_survives_non_ascii_anchor() {
    let content = "fn filler() {}\n".repeat(200) + "fn target() {}\n";
    // A word starting with a multi-byte character: the old `w[1..]` panicked.
    let win = window_on(&content, "let λ_ToolRegistry = build();");
    assert!(!win.is_empty());
}

#[test]
fn window_centres_on_the_anchor_symbol() {
    let head = "fn filler() {}\n".repeat(400);
    let tail = "fn filler2() {}\n".repeat(400);
    let content = format!("{head}pub struct ProcRunTool {{\n    root: PathBuf,\n}}\n{tail}");
    let win = window_on(&content, "replace the doc comment on `struct ProcRunTool`");
    assert!(win.contains("pub struct ProcRunTool"));
    assert!(win.len() < content.len());
}

#[test]
fn truncation_cuts_on_char_boundaries() {
    // Multi-byte content in every layer, budgets far below it: the head/tail
    // cut used to land inside a character and panic.
    let filler = "λ".repeat(200);
    let b = ContextBuilder::new(TokenBudgets {
        long_term: 3,
        mid_term: 3,
        short_term: 3,
    });
    let view = b.build(&CtxState {
        long_term: format!("{filler}цель"),
        mid_term: format!("{filler}目標"),
        short_term: format!("{filler}🎯"),
    });
    assert!(view.truncated);
    assert!(view.prompt.contains("[truncated]"));
}
