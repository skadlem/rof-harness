use ratatui::{
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::app::App;

/// Transcript (top) · status (thin middle) · composer (bottom, 3 lines).
/// Logic-free: every string comes from `App`.
pub fn draw(f: &mut Frame, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(f.area());
    let height = rows[0].height as usize;
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
    f.render_widget(Paragraph::new(shown.join("\n")), rows[0]);
    f.render_widget(Paragraph::new(app.status_line()), rows[1]);
    f.render_widget(
        Paragraph::new(app.input.as_str()).block(Block::default().borders(Borders::ALL)),
        rows[2],
    );
}
