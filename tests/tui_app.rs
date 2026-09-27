use rof::obs::{TraceEvent, TraceSink};
use rof::tui::app::{App, BusyMode, DeferredConfig, RunMode};
use rof::tui::cmd::Action;
use rof::tui::render::replay_filter;
use rof::tui::run::{handle_running_action, RunningActionOutcome};

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

/// Row text of a rendered screen: one `String` per terminal row.
fn rendered_rows(app: &App, width: u16, height: u16) -> Vec<String> {
    use ratatui::{backend::TestBackend, Terminal};
    use rof::tui::ui::draw;
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    buf.content()
        .chunks(width as usize)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect()
}

#[test]
fn live_activity_and_control_aware_composer_render() {
    let mut app = App::new();
    app.begin_run("watch the run");
    // More events than the activity pane has interior rows, so the newest
    // line can only be visible if the renderer honors the border.
    for (from, to) in [
        ("ready", "implementing"),
        ("implementing", "reviewing"),
        ("reviewing", "verifying"),
        ("verifying", "summarizing"),
        ("summarizing", "reporting"),
        ("reporting", "finalizing"),
    ] {
        app.on_event(&TraceEvent::StateTransition {
            from: from.into(),
            to: to.into(),
        });
    }
    let rows = rendered_rows(&app, 100, 24);
    let out: String = rows.concat();
    assert!(out.contains("run activity"), "{out}");
    // P1a is over: a live composer submits steers and queued goals, so the
    // title names the busy mode instead of claiming the input is read-only.
    assert!(out.contains("composer · steer"), "{out}");
    assert!(!out.contains("read-only"), "{out}");
    let at = |needle: &str| rows.iter().position(|row| row.contains(needle)).unwrap();
    // The pane is 6 rows on a 24-row terminal, so 4 interior rows hold the
    // tail; inside the pane the oldest line is the one that gets clipped.
    let top = at("run activity");
    let pane: String = rows[top..top + 6].concat();
    assert!(
        pane.contains("finalizing"),
        "newest line inside pane: {pane}"
    );
    assert!(
        !pane.contains("implementing"),
        "oldest line clipped: {pane}"
    );
    let bottom = at("composer · steer");
    assert!(top < bottom, "activity pane sits above the composer: {out}");
}

#[test]
fn replay_activity_region_does_not_claim_a_live_wait() {
    let mut app = App::new();
    app.set_replay_events(vec![TraceEvent::SessionStart {
        session_id: "s".into(),
        goal: "recorded".into(),
    }]);
    let out: String = rendered_rows(&app, 100, 24).concat();
    assert!(!out.contains("waiting for run"), "{out}");
    assert!(out.contains("replay: no live activity"), "{out}");
}

#[test]
fn small_terminal_keeps_transcript_status_and_composer() {
    let mut app = App::new();
    app.transcript.push("tail line".into());
    app.begin_run("small");
    let rows = rendered_rows(&app, 60, 9);
    let out: String = rows.concat();
    // 9 rows is exactly status + composer + a 3-row transcript, so the
    // activity pane yields its rows to the transcript rather than squeezing
    // it down to a bare border.
    assert_eq!(rows.len(), 9);
    assert!(out.contains("transcript"), "{out}");
    assert!(out.contains("tail line"), "transcript keeps content: {out}");
    assert!(out.contains("status"), "{out}");
    assert!(out.contains("composer"), "{out}");
    assert!(
        !out.contains("run activity"),
        "pane hidden at 9 rows: {out}"
    );
    let at = |title: &str| rows.iter().position(|row| row.contains(title)).unwrap();
    assert!(at("transcript") < at("status") && at("status") < at("composer"));
}

#[test]
fn medium_terminal_shows_all_four_panes() {
    let mut app = App::new();
    app.transcript.push("tail line".into());
    app.begin_run("medium");
    let rows = rendered_rows(&app, 60, 12);
    let out: String = rows.concat();
    assert_eq!(rows.len(), 12);
    for title in ["transcript", "run activity", "status", "composer"] {
        assert!(out.contains(title), "{title} missing: {out}");
    }
    // Transcript content survives alongside the restored activity pane.
    assert!(out.contains("tail line"), "transcript keeps content: {out}");
    let at = |title: &str| rows.iter().position(|row| row.contains(title)).unwrap();
    assert!(
        at("transcript") < at("run activity")
            && at("run activity") < at("status")
            && at("status") < at("composer"),
        "panes stack in order: {out}"
    );
}

/// A live run plus the control state a user can act on: the busy mode in
/// the composer title, every occupied slot in the status row, and the
/// settings waiting for the next goal.
fn live_control_app() -> App {
    let mut app = App::new();
    app.begin_run("ship the feature");
    app
}

#[test]
fn live_composer_title_names_the_busy_mode() {
    let mut app = live_control_app();
    assert_eq!(app.run_mode, RunMode::Running);
    let steer: String = rendered_rows(&app, 100, 24).concat();
    assert!(steer.contains("composer · steer"), "{steer}");
    app.set_busy_mode(BusyMode::Queue);
    let queue: String = rendered_rows(&app, 100, 24).concat();
    assert!(queue.contains("composer · queue"), "{queue}");
    assert!(!queue.contains("composer · steer"), "{queue}");
    // A run that is stopping is still live: the composer still takes
    // commands, so the title still names the mode.
    app.set_stopping();
    let stopping: String = rendered_rows(&app, 100, 24).concat();
    assert!(stopping.contains("composer · queue"), "{stopping}");
}

#[test]
fn pending_control_ids_are_visible_on_the_status_row() {
    let mut app = live_control_app();
    let steer_id = app.submit_pending_steer("also cover the retry path");
    let goal_id = app.submit_pending_goal("then document the flag");
    let out: String = rendered_rows(&app, 100, 24).concat();
    assert!(
        out.contains(&format!("steer pending ({steer_id})")),
        "pending steer id shown: {out}"
    );
    assert!(
        out.contains(&format!("goal queued ({goal_id})")),
        "pending goal id shown: {out}"
    );
}

#[test]
fn deferred_settings_show_their_count_and_labels() {
    let mut app = live_control_app();
    app.defer_config(DeferredConfig::Attempts(3));
    app.defer_config(DeferredConfig::Model {
        slot: Some("provider/model".into()),
        value: "anthropic/claude-sonnet-5".into(),
    });
    // The status pane is one text row, so a wide terminal is what lets both
    // labels be seen at once rather than clipped by the frame.
    let out: String = rendered_rows(&app, 160, 24).concat();
    assert!(out.contains("2 deferred"), "count shown: {out}");
    assert!(out.contains("attempts 3"), "label shown: {out}");
    assert!(
        out.contains("provider/model anthropic/claude-sonnet-5"),
        "label shown: {out}"
    );
}

#[test]
fn refused_login_renders_without_the_key() {
    let mut app = live_control_app();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<rof::engine::control::RunCommand>();
    let trace = TraceSink::new();
    let mut awaiting_key: Option<String> = None;
    let outcome = handle_running_action(
        &mut app,
        Action::Login(None),
        &tx,
        "/login openrouter",
        &trace,
        &mut awaiting_key,
    );
    let reason = match outcome {
        RunningActionOutcome::Rejected(reason) => reason,
        other => panic!("a live run must refuse /login, got {other:?}"),
    };
    // The pump is what writes the refusal into the transcript.
    app.transcript.push(reason);
    let out: String = rendered_rows(&app, 100, 24).concat();
    assert!(
        out.contains("/login is available between goals"),
        "refusal shown: {out}"
    );
    assert!(!out.contains("sk-"), "no key text: {out}");
    // The refusal itself carries no credential material.
    assert!(
        !app.transcript
            .iter()
            .any(|line| line.contains("openrouter")),
        "{:?}",
        app.transcript
    );
}

#[test]
fn idle_and_replay_status_lines_are_unchanged() {
    // Pinned byte-for-byte: the live control summary is appended only while
    // a run is live, so these two postures must not shift by a character.
    assert_eq!(App::new().status_line(), "calls=0 in=0 out=0 pass=0 fail=0");
    let mut app = App::new();
    app.set_replay_events(replay_events());
    assert_eq!(
        app.status_line(),
        "calls=0 in=0 out=0 pass=1 fail=0 · replay 3/3 [] · help: j/k move · g/G ends · / filter · q quit"
    );
    // A finished run is not live either: the control summary is not
    // appended to a terminal status line.
    let mut settled = live_control_app();
    settled.defer_config(DeferredConfig::Rounds(2));
    settled.run_mode = RunMode::Finished;
    assert_eq!(
        settled.status_line(),
        "finished · calls=0 in=0 out=0 pass=0 fail=0"
    );
}

#[test]
fn long_pending_goal_text_truncates_the_title_without_resizing() {
    let short_goal = {
        let mut app = live_control_app();
        app.submit_pending_goal("document the flag");
        app
    };
    let long_goal = {
        let mut app = live_control_app();
        // A runaway goal text, plus a deferred value long enough to blow
        // past any title budget: neither may change the frame.
        app.submit_pending_goal(&"x".repeat(500));
        app.defer_config(DeferredConfig::Thinking("high".into()));
        app.defer_config(DeferredConfig::Effort("maximum".into()));
        app
    };
    let short = rendered_rows(&short_goal, 60, 9);
    let long = rendered_rows(&long_goal, 60, 9);
    assert_eq!(short.len(), 9);
    assert_eq!(long.len(), 9);
    // One title row, cut with an ellipsis rather than wrapped onto a second.
    let title_rows: Vec<&String> = long
        .iter()
        .filter(|row| row.contains("composer ·"))
        .collect();
    assert_eq!(title_rows.len(), 1, "one title row: {long:?}");
    assert!(
        title_rows[0].contains('…'),
        "title truncated: {title_rows:?}"
    );
    // A NARROW terminal is the real proof that the title is cut rather than
    // wrapped: a 60-column backend renders every row 60 wide whatever the
    // content, so a fixed-width assertion there proves nothing. At 24
    // columns the title must still be one row and the frame must still be 9
    // rows, because the border clips it and nothing else may move.
    let narrow = rendered_rows(&long_goal, 24, 9);
    assert_eq!(narrow.len(), 9, "a long title changed the frame height");
    let narrow_titles: Vec<&String> = narrow
        .iter()
        .filter(|row| row.contains("composer"))
        .collect();
    assert_eq!(narrow_titles.len(), 1, "title wrapped at 24 columns");
}

/// A long thinking label is the one unbounded input to the live title, and
/// it comes from the environment. It gets clipped so the control state — the
/// part the user is reading the title for — always survives.
#[test]
fn a_long_thinking_label_cannot_push_the_control_state_off_the_title() {
    let mut app = live_control_app();
    app.thinking = "reasoning-effort-max-plus-some-extra".to_string();
    app.submit_pending_goal("document the flag");
    app.set_busy_mode(BusyMode::Queue);

    let rows = rendered_rows(&app, 72, 9);
    let titles: Vec<&String> = rows.iter().filter(|row| row.contains("composer")).collect();
    assert_eq!(titles.len(), 1, "one title row: {rows:?}");
    assert!(
        titles[0].contains('…'),
        "the thinking label was not clipped: {titles:?}"
    );
    assert!(
        titles[0].contains("queue"),
        "the busy mode was pushed off the title: {titles:?}"
    );
    assert!(
        titles[0].contains("goal queued"),
        "the pending slot was pushed off the title: {titles:?}"
    );
}

/// A resolved run is not live, so its composer is the plain one even when
/// control state is still on the App: a finished goal must not keep claiming
/// a mode in its title.
#[test]
fn a_resolved_run_shows_the_plain_composer_even_with_control_state_present() {
    for mode in [RunMode::Finished, RunMode::Failed] {
        let mut app = live_control_app();
        app.submit_pending_steer("focus on the parser");
        app.defer_config(DeferredConfig::Attempts(2));
        app.set_busy_mode(BusyMode::Queue);
        app.on_live_finished(&rof::obs::GoalFinished {
            passed: mode == RunMode::Finished,
            error: None,
        });

        let rows = rendered_rows(&app, 60, 9);
        let titles: Vec<&String> = rows.iter().filter(|row| row.contains("composer")).collect();
        assert_eq!(titles.len(), 1, "one title row: {rows:?}");
        assert!(
            !titles[0].contains("queue") && !titles[0].contains("goal queued"),
            "{mode:?} run still claims a live mode: {titles:?}"
        );
        assert!(
            titles[0].contains("┌composer"),
            "{mode:?} run did not use the plain title: {titles:?}"
        );
        assert!(
            !titles[0].contains('·'),
            "{mode:?} run padded the plain title with a mode or summary: {titles:?}"
        );
    }
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
