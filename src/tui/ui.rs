use ratatui::{
    layout::{Constraint, Direction, Layout},
    widgets::Paragraph,
    Frame,
};

use super::{app::App, theme};

/// Transcript (top) · status (middle) · composer (bottom, 3 lines).
/// Logic-free: every string comes from `App` (plus `ROF_THINKING` for the
/// composer's title accent).
pub fn draw(f: &mut Frame, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(f.area());
    let height = rows[0].height.saturating_sub(2) as usize;
    let skip = app.scroll.min(app.transcript.len());
    let tail: Vec<String> = app
        .transcript
        .iter()
        .take(app.transcript.len() - skip)
        .cloned()
        .rev()
        .take(height)
        .collect();
    let shown: Vec<String> = tail.into_iter().rev().collect();
    f.render_widget(
        Paragraph::new(shown.join("\n")).block(theme::pane("transcript")),
        rows[0],
    );
    f.render_widget(
        Paragraph::new(app.status_line()).block(theme::pane("status")),
        rows[1],
    );
    let thinking = std::env::var("ROF_THINKING").unwrap_or_default();
    f.render_widget(
        Paragraph::new(app.input.as_str()).block(theme::composer_block(&thinking)),
        rows[2],
    );
}
