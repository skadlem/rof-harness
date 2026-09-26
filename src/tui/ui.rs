use ratatui::{
    layout::{Constraint, Direction, Layout},
    widgets::Paragraph,
    Frame,
};

use super::{
    app::{App, RunMode},
    theme,
};

/// Transcript (top) · run activity · status · composer (bottom, 3 lines).
/// Logic-free: every string comes from `App`. No clock, git, environment,
/// or channel access happens here, so the same `&App` always draws the
/// same screen.
pub fn draw(f: &mut Frame, app: &App) {
    // The activity pane is the only elastic region: it is capped at 6 rows
    // and yields all of them back to the transcript on a short terminal, so
    // a small window degrades to the pre-P1a three-pane layout. Subtracting
    // 9 (status + composer + a 3-row transcript with its border) means a
    // terminal shorter than 10 rows drops the pane instead of squeezing the
    // transcript down to its bare border.
    let activity_height = f.area().height.saturating_sub(9).min(6);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(activity_height),
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
    // `theme::pane` draws `Borders::ALL`, so the text area is two rows
    // shorter than the pane; asking for the full height would push the
    // newest line under the bottom border.
    let activity_inner = rows[1].height.saturating_sub(2) as usize;
    let activity = app.activity_tail(activity_inner);
    // The empty state names the posture it is in, so a replay never claims
    // to be waiting on a live run and a settled run never implies one.
    let empty_state = if app.replay_mode {
        "replay: no live activity"
    } else {
        match app.run_mode {
            RunMode::Running | RunMode::Stopping => "waiting for run",
            RunMode::Idle | RunMode::Finished | RunMode::Failed => "no run activity",
        }
    };
    let activity: Vec<String> = if activity.is_empty() && activity_inner > 0 {
        vec![empty_state.to_string()]
    } else {
        activity
    };
    f.render_widget(
        Paragraph::new(activity.join("\n")).block(theme::pane("run activity")),
        rows[1],
    );
    f.render_widget(
        Paragraph::new(app.status_line()).block(theme::pane("status")),
        rows[2],
    );
    let shown = if app.mask_input {
        "•".repeat(app.input.chars().count())
    } else {
        app.input.clone()
    };
    // A live run owns the terminal, so the composer title says read-only
    // instead of showing a thinking state the user cannot change yet.
    let read_only = matches!(app.run_mode, RunMode::Running | RunMode::Stopping);
    f.render_widget(
        Paragraph::new(shown.as_str())
            .block(theme::composer_block_with_state(&app.thinking, read_only)),
        rows[3],
    );
}
