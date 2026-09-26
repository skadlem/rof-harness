//! Shared TUI palette + pane frames.
//!
//! Named ANSI colors only, so the UI respects the user's terminal theme.
//! Single-accent scheme: amber (yellow) leads (titles, composer), dim gray
//! frames the panes, cyan is reserved for highlights, green/red only for
//! pass/fail.

use ratatui::{
    style::{Color, Style},
    widgets::{Block, Borders},
};

pub const AMBER: Color = Color::Yellow;
pub const CYAN: Color = Color::Cyan;
pub const DIM: Color = Color::DarkGray;
pub const PASS: Color = Color::Green;
pub const FAIL: Color = Color::Red;

/// Titled bordered pane with a dim border.
pub fn pane(title: &str) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DIM))
}

/// Composer frame: amber border. The title carries the thinking state so the
/// state-color intent stays a single accent for now.
pub fn composer_block(thinking: &str) -> Block<'static> {
    composer_block_with_state(thinking, false)
}

/// Composer frame for a live run. P1a is a read-only monitor, so a run in
/// progress overrides the thinking state in the title rather than adding a
/// second accent; every other posture is the plain [`composer_block`].
pub fn composer_block_with_state(thinking: &str, read_only: bool) -> Block<'static> {
    let title = if read_only {
        "composer · read-only (P1a)".to_string()
    } else if thinking.trim().is_empty() {
        "composer".to_string()
    } else {
        format!("composer · {}", thinking.trim())
    };
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(AMBER))
}
