//! Terminal runner for `rof chat`: alternate-screen event pump plus a
//! `--replay <trace.jsonl>` mode that renders a recorded trace with no live
//! loop. The live loop lands in Plan C; this file never touches the
//! orchestrator.

use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{backend::CrosstermBackend, Terminal};

use super::app::App;
use super::render::parse_lenient_line;
use super::ui::draw;
use crate::obs::TraceEvent;

/// Render a recorded trace file in the fullscreen console. `q`/Esc/Ctrl-C
/// quits; anything else edits the (replay-inert) composer.
pub fn replay(path: &std::path::Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut app = App::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<TraceEvent>(line) {
            Ok(ev) => app.on_event(&ev),
            Err(_) => app.transcript.push(parse_lenient_line(line)),
        }
    }
    run_repl(app)
}

/// Alternate-screen event pump: 30fps render cap, `q`/Esc/Ctrl-C quits,
/// chars/backspace/enter drive the composer buffer.
fn run_repl(mut app: App) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = pump(&mut terminal, &mut app);

    disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;
    result
}

fn pump(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;
        if !event::poll(Duration::from_millis(33))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
            {
                break;
            }
            match key.code {
                KeyCode::Esc => break,
                KeyCode::Char('q') if key.modifiers.is_empty() => break,
                KeyCode::Char(c)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    app.input.push(c);
                }
                KeyCode::Backspace => {
                    app.input.pop();
                }
                KeyCode::Enter => {
                    app.input.clear();
                }
                _ => {}
            }
        }
    }
    Ok(())
}
