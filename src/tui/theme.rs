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
/// state-color intent stays a single accent for now. A live run uses
/// [`composer_block_live`] instead, which names the busy mode and whatever
/// is pending.
///
/// The P1a `read_only` variant of this title is GONE, not merely unused: a
/// live composer submits steers and queued goals, so nothing can render that
/// claim truthfully any more.
pub fn composer_block(thinking: &str) -> Block<'static> {
    let title = if thinking.trim().is_empty() {
        "composer".to_string()
    } else {
        format!("composer · {}", thinking.trim())
    };
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(AMBER))
}

/// Diff frame. A snapshot the harness cut at its evidence bound gets a
/// `partial` title, so a short patch can never read as the whole change;
/// the body carries the same claim in words. Every other posture uses
/// [`pane`], which this defers to.
pub fn diff_block(truncated: bool) -> Block<'static> {
    let title = if truncated { "diff (partial)" } else { "diff" };
    pane(title)
}

/// Longest control summary a live composer title may carry. The title
/// rides the top border of a 3-row pane, so a longer summary is cut with
/// an ellipsis: the title is one line by construction and can neither wrap
/// onto a second row nor widen the frame.
const LIVE_TITLE_SUMMARY_MAX_CHARS: usize = 40;

/// Longest thinking label a live composer title may carry. The thinking
/// label comes from `ROF_THINKING` and is unbounded, so it is the segment
/// that gets cut: the control state is what the user needs to read, and a
/// long thinking label must not push it off the top border.
const LIVE_TITLE_THINKING_MAX_CHARS: usize = 20;

/// Truncate `text` to `max` characters, ellipsis included when anything was
/// dropped. Char-based, so a multi-byte label is never split mid-codepoint.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max - 1).collect();
    format!("{kept}…")
}

/// Composer frame for a live run. P1a is over: a live composer submits
/// steers and queued goals, so the title names what the composer is doing —
/// the thinking posture plus the busy mode, occupied slots, and deferred
/// settings from [`App::control_summary`](crate::tui::app::App::control_summary).
/// Each segment is bounded so the whole title stays one line, and the
/// control segment is what survives a long thinking label.
///
/// Every other posture uses [`composer_block`].
pub fn composer_block_live(thinking: &str, control: &str) -> Block<'static> {
    let mut title = String::from("composer");
    let thinking = thinking.trim();
    if !thinking.is_empty() {
        title.push_str(" · ");
        title.push_str(&clip(thinking, LIVE_TITLE_THINKING_MAX_CHARS));
    }
    let control = control.trim();
    if !control.is_empty() {
        title.push_str(" · ");
        title.push_str(&clip(control, LIVE_TITLE_SUMMARY_MAX_CHARS));
    }
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(AMBER))
}
