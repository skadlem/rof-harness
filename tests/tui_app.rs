use rof::obs::TraceEvent;
use rof::tui::app::App;
use rof::tui::render::replay_filter;

fn replay_events() -> Vec<TraceEvent> {
    vec![
        TraceEvent::SessionStart {
            session_id: "s".into(),
            goal: "first".into(),
        },
        TraceEvent::ReviewVerdict {
            pass: true,
            feedback: "looks good".into(),
        },
        TraceEvent::SessionStart {
            session_id: "s2".into(),
            goal: "second".into(),
        },
    ]
}

#[test]
fn replay_filter_selects_matching_lines() {
    assert_eq!(replay_filter(&replay_events(), "verdict"), vec![1]);
}

#[test]
fn replay_cursor_clamps() {
    let mut app = App::new();
    app.set_replay_events(replay_events());
    assert_eq!(app.replay_idx, 2);
    app.replay_step(10);
    assert_eq!(app.replay_idx, 2);
    app.replay_step(-10);
    assert_eq!(app.replay_idx, 0);
    app.set_replay_filter("verdict");
    assert_eq!(app.replay_idx, 0);
    assert!(app.transcript.is_empty());
    app.replay_step(1);
    assert_eq!(app.replay_idx, 1);
    assert_eq!(app.transcript.len(), 1);
    assert!(app.transcript[0].contains("looks good"));
}

#[test]
fn events_append_transcript_and_bump_counters() {
    let mut app = App::new();
    app.on_event(&TraceEvent::SessionStart {
        session_id: "s".into(),
        goal: "fix it".into(),
    });
    app.on_event(&TraceEvent::ReviewVerdict {
        pass: true,
        feedback: "ok".into(),
    });
    assert_eq!(app.transcript.len(), 2);
    assert!(app.transcript[0].contains("fix it"));
    assert_eq!(app.counters.pass, 1);
    assert!(app.status_line().contains("pass=1"));
}

#[test]
fn scroll_offset_moves_the_transcript_window() {
    use ratatui::{backend::TestBackend, Terminal};
    use rof::tui::{app::App, ui::draw};
    fn screen(app: &App) -> String {
        // 24 rows: the titled panes (transcript/status/composer) take 6
        // chrome rows, leaving room for the scrolled window below.
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol().to_string())
            .collect()
    }
    let mut app = App::new();
    for i in 0..20 {
        app.transcript.push(format!("line {i:02}"));
    }
    // Tailed by default: last line visible, first lines scrolled off.
    let bottom = screen(&app);
    assert!(bottom.contains("line 19"), "tail visible by default");
    assert!(!bottom.contains("line 00"), "head scrolled off by default");
    // Scroll up: head comes into view, tail leaves.
    app.scroll_lines(15);
    let up = screen(&app);
    assert!(up.contains("line 00"), "head visible after scroll-up");
    // Scroll clamps at both ends, never panics on empty.
    app.scroll_lines(10_000);
    assert!(
        !screen(&app).contains("line 19"),
        "tail left after scroll-up"
    );
    app.scroll_lines(-10_000);
    assert_eq!(app.scroll, 0);
    let mut empty = App::new();
    empty.scroll_lines(5);
    assert_eq!(empty.scroll, 0);
}

#[test]
fn replay_draw_shows_cursor_filter_and_help() {
    use ratatui::{backend::TestBackend, Terminal};
    use rof::tui::ui::draw;
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::new();
    app.set_replay_events(replay_events());
    terminal.draw(|f| draw(f, &app)).unwrap();
    let out: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol().to_string())
        .collect();
    assert!(out.contains("replay 3/3"), "cursor shown: {out}");
    assert!(out.contains("j/k move"), "help shown: {out}");
    app.set_replay_filter("verdict");
    terminal.draw(|f| draw(f, &app)).unwrap();
    let filtered: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol().to_string())
        .collect();
    assert!(filtered.contains("reviewer"), "matching line shown");
    assert!(!filtered.contains("first"), "non-matching line hidden");
}

#[test]
fn composer_title_comes_from_app_not_the_environment() {
    use ratatui::{backend::TestBackend, Terminal};
    use rof::tui::{app::App, ui::draw};
    fn screen(app: &App) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol().to_string())
            .collect()
    }
    // The accent follows App state, never the process environment.
    let mut app = App::new();
    app.thinking = "low".to_string();
    assert!(
        screen(&app).contains("composer · low"),
        "composer title carries App thinking"
    );
    let plain = App::new();
    assert!(
        !screen(&plain).contains("·"),
        "no accent without App thinking"
    );
}

#[test]
fn masked_composer_hides_key_entry() {
    use ratatui::{backend::TestBackend, Terminal};
    use rof::tui::{app::App, ui::draw};
    fn screen(app: &App) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol().to_string())
            .collect()
    }
    let mut app = App::new();
    app.input = "sk-secret".to_string();
    app.mask_input = true;
    let out = screen(&app);
    assert!(!out.contains("sk-secret"), "key must not render");
    assert!(out.contains("••••••"), "bullets stand in: {out}");
    app.mask_input = false;
    assert!(screen(&app).contains("sk-secret"), "unmasked renders");
}

#[test]
fn layout_shows_transcript_status_and_composer() {
    use ratatui::{backend::TestBackend, Terminal};
    use rof::tui::{app::App, ui::draw};
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::new();
    app.input = "hello".to_string();
    terminal.draw(|f| draw(f, &app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let text: String = buf
        .content()
        .iter()
        .map(|c| c.symbol().to_string())
        .collect();
    assert!(text.contains("hello"), "composer visible");
}
