use ratatui::layout::Rect;
use rof::obs::{TraceEvent, TraceSink};
use rof::tui::app::{App, BusyMode, DeferredConfig, Focus, RunMode};
use rof::tui::cmd::Action;
use rof::tui::render::{replay_filter, Counters};
use rof::tui::run::{handle_running_action, RunningActionOutcome};
use rof::tui::ui::visible_panes;

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
    // Scoped to the composer's own pane: the check is that the title carries
    // no accent, and the status row has separators of its own.
    let plain_screen = screen(&plain);
    let composer = plain_screen
        .split('┌')
        .find(|pane| pane.starts_with("composer"))
        .expect("a composer pane is rendered at 80x24");
    assert!(!composer.contains('·'), "no accent without App thinking");
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

/// The splash frame, rendered the way the pump renders it: the overlay while
/// `App::fresh` is set, using the App's own chosen sprite.
fn rendered_rows_splash(app: &App, width: u16, height: u16) -> Vec<String> {
    use ratatui::{backend::TestBackend, Terminal};
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| rof::tui::splash::draw(f, app.mascot))
        .unwrap();
    let buf = terminal.backend().buffer().clone();
    buf.content()
        .chunks(width as usize)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect()
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
fn idle_replay_and_settled_status_lines_keep_their_pinned_prefix() {
    // Pinned byte-for-byte as a PREFIX, and the name says what the assertion
    // actually is: the live control summary is appended only while a run is
    // live, and the metrics the status row gained (cost, model identity) are
    // appended AFTER the counters, so these postures must not shift the
    // pinned text by a character. What follows the prefix is the metrics
    // suffix, pinned separately by the metrics tests.
    assert!(
        App::new()
            .status_line()
            .starts_with("calls=0 in=0 out=0 pass=0 fail=0"),
        "{}",
        App::new().status_line()
    );
    let mut app = App::new();
    app.set_replay_events(replay_events());
    assert!(
        app.status_line().starts_with(
            "calls=0 in=0 out=0 pass=1 fail=0 · replay 3/3 [] · help: j/k move · g/G ends · / filter · q quit"
        ),
        "{}",
        app.status_line()
    );
    // A finished run is not live either: the control summary is not
    // appended to a terminal status line.
    let mut settled = live_control_app();
    settled.defer_config(DeferredConfig::Rounds(2));
    settled.run_mode = RunMode::Finished;
    assert!(
        settled
            .status_line()
            .starts_with("finished · calls=0 in=0 out=0 pass=0 fail=0"),
        "{}",
        settled.status_line()
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

// ---- diff pane (P3 C2): rendered only from `App::diff_snapshot()` ----

/// A diff snapshot exactly as the engine emits it. This is the pane's only
/// input: the renderer never runs `git` and never reads the tree itself.
fn diff_event(names: &[&str], stat: &str, patch: &str, truncated: bool) -> TraceEvent {
    TraceEvent::DiffSnapshot {
        names: names.iter().map(|n| (*n).to_string()).collect(),
        stat: stat.to_string(),
        patch: patch.to_string(),
        truncated,
    }
}

const SAMPLE_PATCH: &str = "+ use ratatui::Frame;\n- use std::io;";

/// The rows of one titled pane: from its frame's top border up to the next
/// pane's, so a wide layout that shares rows is compared pane by pane.
/// The title is matched on the border, never on pane text.
fn pane_slice(rows: &[String], title: &str, next: &str) -> String {
    let border = format!("┌{title}");
    let stop = format!("┌{next}");
    let start = rows
        .iter()
        .position(|row| row.contains(&border))
        .unwrap_or_else(|| panic!("no {title} pane: {rows:?}"));
    let end = rows[start + 1..]
        .iter()
        .position(|row| row.contains(&stop))
        .map(|offset| start + 1 + offset)
        .unwrap_or(rows.len());
    rows[start..end].concat()
}

#[test]
fn diff_pane_renders_the_snapshot_names_and_patch_in_both_layouts() {
    let mut app = App::new();
    app.on_event(&diff_event(
        &["src/tui/ui.rs", "src/tui/theme.rs"],
        "2 files changed",
        SAMPLE_PATCH,
        false,
    ));
    // 160 columns is the wide split (transcript beside the run pane, diff
    // in the lower detail area); 60 is the narrow full-width stack. The
    // pane renders from the same snapshot either way.
    for (width, layout) in [(160u16, "wide"), (60, "narrow")] {
        let rows = rendered_rows(&app, width, 40);
        let out: String = rows.concat();
        assert!(out.contains("src/tui/ui.rs"), "{layout}: {out}");
        assert!(out.contains("src/tui/theme.rs"), "{layout}: {out}");
        assert!(out.contains("+ use ratatui::Frame;"), "{layout}: {out}");
        assert!(
            rows.iter().any(|row| row.contains("┌diff")),
            "{layout}: {rows:?}"
        );
    }
}

#[test]
fn a_missing_snapshot_says_the_evidence_is_missing() {
    let out: String = rendered_rows(&App::new(), 100, 24).concat();
    assert!(out.contains("no diff evidence"), "{out}");
    // No evidence is not the same claim as a clean tree: the pane must not
    // read like `git` found nothing to change.
    assert!(!out.contains("tree is clean"), "{out}");
}

#[test]
fn a_snapshot_with_no_changed_names_says_the_tree_is_clean() {
    let mut app = App::new();
    app.on_event(&diff_event(&[], "", "", false));
    let out: String = rendered_rows(&app, 100, 24).concat();
    assert!(out.contains("tree is clean"), "{out}");
    assert!(!out.contains("no diff evidence"), "{out}");
}

#[test]
fn a_truncated_snapshot_is_labelled_as_part_of_the_change() {
    let mut app = App::new();
    app.on_event(&diff_event(
        &["src/tui/ui.rs"],
        "1 file changed",
        "+ use ratatui::Frame;",
        true,
    ));
    let partial: String = rendered_rows(&app, 100, 24).concat();
    assert!(
        partial.contains("partial"),
        "pane is not marked partial: {partial}"
    );
    // The wording is the pane's own, and it differs from the whole-change
    // render of the same evidence.
    let mut whole = App::new();
    whole.on_event(&diff_event(
        &["src/tui/ui.rs"],
        "1 file changed",
        "+ use ratatui::Frame;",
        false,
    ));
    let full = rendered_rows(&whole, 100, 24);
    assert!(!full.concat().contains("partial"), "{full:?}");
    assert!(full.iter().any(|row| row.contains("┌diff")));
}

#[test]
fn the_truncated_flag_is_the_contract_not_the_patch_text() {
    // The marker string can appear in a whole patch and be absent from a
    // cut one; only the boolean decides how the pane labels itself.
    let mut marked = App::new();
    marked.on_event(&diff_event(
        &["src/tui/ui.rs"],
        "",
        "+ a line\n… [diff truncated: harness evidence bound reached]",
        false,
    ));
    assert!(
        !rendered_rows(&marked, 100, 24)
            .iter()
            .any(|row| row.contains("diff (partial)")),
        "marker text alone must not mark the pane partial"
    );
    let mut cut = App::new();
    cut.on_event(&diff_event(&["src/tui/ui.rs"], "", "+ a line", true));
    assert!(
        rendered_rows(&cut, 100, 24)
            .iter()
            .any(|row| row.contains("diff (partial)")),
        "the flag alone must mark the pane partial"
    );
}

#[test]
fn a_long_patch_clips_without_resizing_the_frame_or_losing_the_composer() {
    let patch: String = (0..200)
        .map(|i| format!("+ line {i:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = App::new();
    for i in 0..40 {
        app.transcript.push(format!("line {i:02}"));
    }
    app.on_event(&diff_event(&["src/tui/ui.rs"], "", &patch, false));
    // The patch is bounded by the harness, not by the pane, so it is far
    // taller than any frame here.
    let rows = rendered_rows(&app, 100, 24);
    assert_eq!(rows.len(), 24, "the patch changed the frame height");
    let out: String = rows.concat();
    assert!(
        out.contains("+ line 199"),
        "tail of the patch visible: {out}"
    );
    assert!(!out.contains("+ line 000"), "head clipped: {out}");
    for title in ["transcript", "run activity", "diff", "status", "composer"] {
        assert!(out.contains(title), "{title} lost: {out}");
    }
    // Scrolling the DIFF moves the patch window and nothing else: same
    // frame, same panes, composer still docked. The diff owns its own
    // offset now, so the patch is scrolled through the focused pane and
    // the transcript's offset is untouched by it.
    app.focus = Focus::Diff;
    app.scroll_focused(10_000);
    assert_eq!(app.scroll, 0, "scrolling the diff moved the transcript");
    let up = rendered_rows(&app, 100, 24);
    assert_eq!(up.len(), 24, "scrolling changed the frame height");
    let up: String = up.concat();
    // The offset is clamped so the first body line can never scroll out of
    // the window: a pane with less content than its window is not
    // scrollable into blank space.
    assert!(
        up.contains("+ line 000"),
        "the patch window did not move: {up}"
    );
    assert!(
        up.contains("composer"),
        "composer lost while scrolling: {up}"
    );
}

#[test]
fn the_diff_pane_gives_way_before_the_four_pinned_panes() {
    for height in [9u16, 12] {
        let mut app = App::new();
        app.transcript.push("tail line".into());
        app.begin_run("small");
        app.on_event(&diff_event(
            &["src/tui/ui.rs"],
            "1 file changed",
            "+ use ratatui::Frame;",
            false,
        ));
        let rows = rendered_rows(&app, 60, height);
        let out: String = rows.concat();
        assert_eq!(rows.len(), height as usize);
        assert!(
            !out.contains("diff"),
            "the diff pane took rows from the pinned panes at {height}: {out}"
        );
        for title in ["transcript", "status", "composer"] {
            assert!(out.contains(title), "{title} missing at {height}: {out}");
        }
        assert!(out.contains("tail line"), "transcript lost content: {out}");
    }
}

#[test]
fn a_replayed_snapshot_renders_the_same_diff_pane_as_the_live_one() {
    let mut live = App::new();
    live.on_event(&diff_event(
        &["src/tui/ui.rs"],
        "1 file changed",
        SAMPLE_PATCH,
        true,
    ));
    let live_rows = rendered_rows(&live, 100, 40);
    // The same evidence, reached through a recorded trace instead of the
    // live event stream.
    let mut replayed = App::new();
    replayed.set_replay_events(vec![
        TraceEvent::SessionStart {
            session_id: "s".into(),
            goal: "add the diff pane".into(),
        },
        diff_event(&["src/tui/ui.rs"], "1 file changed", SAMPLE_PATCH, true),
        TraceEvent::ReviewVerdict {
            pass: true,
            feedback: "pane reads well".into(),
        },
    ]);
    let replay_rows = rendered_rows(&replayed, 100, 40);
    let live_pane = pane_slice(&live_rows, "diff", "status");
    let replay_pane = pane_slice(&replay_rows, "diff", "status");
    assert_eq!(replay_pane, live_pane, "replay rendered a different pane");
    assert!(replay_pane.contains("src/tui/ui.rs"), "{replay_pane}");
    assert!(
        replay_pane.contains("+ use ratatui::Frame;"),
        "{replay_pane}"
    );
}

#[test]
fn the_diff_pane_is_wired_to_the_app_snapshot() {
    let bare = rendered_rows(&App::new(), 160, 40);
    let mut with_snapshot = App::new();
    with_snapshot.on_event(&diff_event(
        &["src/tui/ui.rs"],
        "1 file changed",
        "+ use ratatui::Frame;",
        false,
    ));
    let with = rendered_rows(&with_snapshot, 160, 40);
    assert_ne!(bare, with, "a snapshot changed nothing on screen");
    assert!(with.iter().any(|row| row.contains("┌diff")), "{with:?}");
    let out: String = with.concat();
    assert!(out.contains("src/tui/ui.rs"), "{out}");
    assert!(out.contains("+ use ratatui::Frame;"), "{out}");
}

// ---- pane focus and per-pane scroll (P3 D) ----

/// The panes the given terminal size actually renders, as the focus cycle
/// sees them. The renderer computes this from the geometry alone.
fn visible(width: u16, height: u16) -> Vec<Focus> {
    visible_panes(Rect::new(0, 0, width, height))
}

/// An app with a long transcript and a patch far taller than any window,
/// so both panes have something to scroll.
fn scrolled_app() -> App {
    let patch: String = (0..200)
        .map(|i| format!("+ line {i:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = App::new();
    for i in 0..40 {
        app.transcript.push(format!("line {i:02}"));
    }
    app.on_event(&diff_event(&["src/tui/ui.rs"], "", &patch, false));
    app
}

#[test]
fn tab_cycles_focus_through_every_pane_and_shift_tab_reverses_it() {
    // Every pane the wide layout renders takes a turn, and the cycle wraps.
    let panes = visible(160, 40);
    assert_eq!(
        panes,
        vec![Focus::Transcript, Focus::Run, Focus::Diff, Focus::Composer],
        "the wide layout renders all four focusable panes"
    );
    let mut app = App::new();
    // Typing must keep working with no focus key pressed, so the composer
    // is where the focus starts.
    assert_eq!(
        app.focus,
        Focus::Composer,
        "the composer is the default focus"
    );
    for expected in [Focus::Transcript, Focus::Run, Focus::Diff, Focus::Composer] {
        app.focus_next(&panes);
        assert_eq!(app.focus, expected);
    }
    // The same cycle in reverse, from the default.
    app.focus = Focus::Composer;
    for expected in [Focus::Diff, Focus::Run, Focus::Transcript, Focus::Composer] {
        app.focus_prev(&panes);
        assert_eq!(app.focus, expected);
    }
}

#[test]
fn a_cycle_skips_a_pane_that_is_not_rendered() {
    // 60x9 is the pinned short layout: the diff pane is dropped whole and
    // the run pane yields its rows, so neither may be landed on.
    let panes = visible(60, 9);
    assert!(
        !panes.contains(&Focus::Diff) && !panes.contains(&Focus::Run),
        "a dropped pane must not be in the cycle: {panes:?}"
    );
    let mut app = App::new();
    let mut seen = vec![app.focus];
    for _ in 0..4 {
        app.focus_next(&panes);
        assert!(
            panes.contains(&app.focus),
            "focus landed on an unrendered pane: {:?}",
            app.focus
        );
        seen.push(app.focus);
    }
    // Two panes, so the cycle is exactly those two and it wraps.
    assert_eq!(
        seen,
        vec![
            Focus::Composer,
            Focus::Transcript,
            Focus::Composer,
            Focus::Transcript,
            Focus::Composer
        ]
    );
    // The medium layout keeps the run pane and still drops the diff.
    let medium = visible(60, 12);
    assert!(!medium.contains(&Focus::Diff), "{medium:?}");
    let mut app = App::new();
    let mut landed: Vec<Focus> = Vec::new();
    for _ in 0..3 {
        app.focus_next(&medium);
        landed.push(app.focus);
    }
    assert_eq!(
        landed,
        vec![Focus::Transcript, Focus::Run, Focus::Composer],
        "the diff was never landed on"
    );
}

#[test]
fn the_transcript_and_the_diff_scroll_independently() {
    let mut app = scrolled_app();
    let panes = visible(100, 24);
    // The composer is the default focus, so the scroll keys keep their
    // transcript meaning: the transcript moves and the diff does not.
    app.scroll_focused(35);
    assert_eq!(app.scroll, 35, "the transcript did not scroll");
    assert_eq!(app.diff_scroll, 0, "the diff moved with the transcript");
    let out: String = rendered_rows(&app, 100, 24).concat();
    assert!(
        out.contains("line 00"),
        "the transcript head is shown: {out}"
    );
    assert!(out.contains("+ line 199"), "the diff tail is shown: {out}");

    // Focus the diff through the cycle, the way Tab does it.
    for _ in 0..3 {
        app.focus_next(&panes);
    }
    assert_eq!(app.focus, Focus::Diff);
    // Now the same keys move the diff, and the transcript's offset is left
    // exactly where it was.
    app.scroll_focused(isize::MAX);
    assert_eq!(app.diff_scroll, app.diff_body_len() - 1);
    assert_eq!(app.scroll, 35, "scrolling the diff moved the transcript");
    let out: String = rendered_rows(&app, 100, 24).concat();
    assert!(out.contains("+ line 000"), "the diff head is shown: {out}");
    assert!(out.contains("line 00"), "the transcript stayed put: {out}");

    // And back down: the diff returns to its tail, the transcript unmoved.
    app.scroll_focused(isize::MIN);
    assert_eq!(app.diff_scroll, 0);
    assert_eq!(app.scroll, 35);
    assert!(rendered_rows(&app, 100, 24).concat().contains("+ line 199"));
}

#[test]
fn a_pane_cannot_be_scrolled_into_blank_space() {
    let mut app = scrolled_app();
    app.focus = Focus::Diff;
    app.scroll_focused(isize::MAX);
    let rows = rendered_rows(&app, 100, 24);
    let top = rows
        .iter()
        .position(|row| row.contains("┌diff"))
        .expect("a diff pane is rendered at 100x24");
    let bottom = rows
        .iter()
        .position(|row| row.contains("┌status"))
        .expect("a status pane is rendered at 100x24");
    // The window is as tall as the pane allows and the patch is far taller,
    // so every interior row carries a line: scrolling never scrolls a
    // pane's content out from under itself.
    assert!(bottom > top + 2, "the diff pane has no interior: {rows:?}");
    for row in &rows[top + 1..bottom - 1] {
        assert!(
            !row.trim().is_empty(),
            "an interior row of the diff went blank: {row:?}"
        );
    }
    // A pane whose content is shorter than its window is not scrollable at
    // all: the offset stays at 0 and the line is still there.
    let mut short = App::new();
    short.on_event(&diff_event(&["src/tui/ui.rs"], "", "+ one line", false));
    short.focus = Focus::Diff;
    short.scroll_focused(isize::MAX);
    assert_eq!(short.diff_scroll, 0, "a one-line diff was scrolled");
    let out: String = rendered_rows(&short, 100, 24).concat();
    assert!(out.contains("+ one line"), "{out}");
}

#[test]
fn the_focused_pane_is_marked_and_the_composer_stays_available() {
    let mut app = scrolled_app();
    // The default focus is the composer, and the mark is on the composer.
    let rows = rendered_rows(&app, 100, 24);
    let out: String = rows.concat();
    assert!(
        out.contains("composer ▸"),
        "the focused pane is marked: {out}"
    );
    assert!(
        !out.contains("transcript ▸") && !out.contains("diff ▸"),
        "an unfocused pane is marked: {out}"
    );
    // The mark says where the keys go. It must never read as a pane the
    // user cannot type into, which is what the P1a read-only claim did.
    for blocked in ["read-only", "readonly", "locked", "disabled"] {
        assert!(!out.contains(blocked), "composer reads {blocked}: {out}");
    }
    // The mark follows the focus: one pane is marked at a time.
    app.focus = Focus::Diff;
    let out: String = rendered_rows(&app, 100, 24).concat();
    assert!(out.contains("diff ▸"), "the focus mark did not move: {out}");
    assert!(!out.contains("composer ▸"), "two panes are marked: {out}");
}

/// The size matrix the layout has to survive: a short terminal down to a
/// single row, at narrow, medium, and wide widths. The focus cycle is
/// walked at each size, because a cycle that lands on a pane the frame
/// dropped is the one way a Tab press could be lost.
#[test]
fn every_short_terminal_size_renders_and_cycles_within_its_panes() {
    let mut app = scrolled_app();
    app.begin_run("goal");
    for height in 1..=20u16 {
        for width in [20u16, 60, 160] {
            let rows = rendered_rows(&app, width, height);
            assert_eq!(rows.len(), height as usize, "{width}x{height}");
            let panes = visible(width, height);
            if panes.is_empty() {
                continue;
            }
            for _ in 0..5 {
                app.focus_next(&panes);
                assert!(
                    panes.contains(&app.focus),
                    "focus {:?} is not rendered at {width}x{height}",
                    app.focus
                );
            }
            for _ in 0..5 {
                app.focus_prev(&panes);
                assert!(
                    panes.contains(&app.focus),
                    "reverse focus {:?} is not rendered at {width}x{height}",
                    app.focus
                );
            }
            // The P1a guarantees, checked on the vertical stack (the wide
            // split at 120 columns and up is a different layout): the
            // transcript, the status row, and the composer are all there
            // from 6 rows up, and the run pane yields its rows rather than
            // squeezing the transcript to a bare border. Below 6 rows a
            // 1-row frame cannot carry a title at all, and the point there
            // is only that the renderer neither panics nor loses the frame.
            if height >= 6 && width < 120 {
                let out: String = rows.concat();
                for title in ["transcript", "status", "composer"] {
                    assert!(
                        out.contains(title),
                        "{title} missing at {width}x{height}: {out}"
                    );
                }
            }
        }
    }
}

/// A short WIDE terminal is the one geometry the diff pane could take the
/// primary pane with: sizing the upper area as `detail - diff_rows` gave the
/// transcript zero rows at 6 rows tall and 160 columns. The transcript keeps
/// its 3-row floor at every size, exactly as the narrow stack does.
#[test]
fn the_transcript_survives_a_short_wide_terminal() {
    let app = scrolled_app();
    for (width, height) in [(160u16, 6u16), (160, 8), (200, 9), (120, 6)] {
        let rows: String = rendered_rows(&app, width, height).concat();
        assert_eq!(
            rows.matches("transcript").count(),
            1,
            "no transcript at {width}x{height}"
        );
        assert!(
            rows.contains("composer"),
            "composer lost at {width}x{height}"
        );
        // A wide terminal may still show the run pane beside the transcript's
        // 3-row floor; what it may not do is spend the transcript's rows on
        // it, which is exactly what the `Min(3)` floor prevents.
        assert_eq!(
            rows.matches("transcript").count(),
            1,
            "the transcript was displaced at {width}x{height}"
        );
    }
}

/// The status pane's own text row, borders trimmed: the metrics row a user
/// reads, not a string the reducer happens to build.
fn status_row(app: &App, width: u16, height: u16) -> String {
    let rows = rendered_rows(app, width, height);
    let top = rows
        .iter()
        .position(|row| row.contains("┌status"))
        .unwrap_or_else(|| panic!("no status pane at {width}x{height}: {rows:?}"));
    // The pane's interior row, with the frame's own border characters and
    // padding stripped so the text a user reads is what is asserted on.
    rows[top + 1]
        .trim_matches(|c: char| c == '│' || c.is_whitespace())
        .to_string()
}

/// One segment of the status row, e.g. its `cost=…` or `model=…` part. Read
/// segment-wise because a prefix overlaps: `$0.0042` starts with `$0.00`.
fn metric(row: &str, name: &str) -> String {
    row.split(" · ")
        .find(|part| part.starts_with(&format!("{name}=")))
        .unwrap_or_else(|| panic!("no {name} segment in: {row}"))
        .to_string()
}

/// One recorded model call, so a test states the identity and the cost it
/// is about and nothing else.
fn model_call(agent: &str, model: &str, input: u64, output: u64, cost: Option<f64>) -> TraceEvent {
    TraceEvent::ModelCall {
        agent: agent.into(),
        model: model.into(),
        input_tokens: input,
        output_tokens: output,
        latency_ms: 10,
        cost_usd: cost,
        cached_input_tokens: 0,
        attempts: 1,
    }
}

#[test]
fn status_row_names_the_executor_and_reviewer_models() {
    let mut app = App::new();
    app.begin_run("ship the feature");
    app.on_event(&model_call("executor", "exec-model", 100, 40, Some(0.25)));
    app.on_event(&model_call("reviewer", "rev-model", 20, 10, Some(0.25)));
    // Both identities on one compact row is a wide-terminal read: a narrow
    // row clips its trailing metrics rather than splitting them.
    let row = status_row(&app, 140, 24);
    // Both identities come from what the trace recorded, one per path, so a
    // run that reviewed with a different model cannot read as if it used one.
    assert!(
        row.contains("model=exec=exec-model rev=rev-model"),
        "model identity missing: {row}"
    );
}

#[test]
fn a_run_with_no_model_calls_says_none_yet() {
    let row = status_row(&App::new(), 100, 24);
    assert!(
        row.contains("model=none yet"),
        "no honest empty state: {row}"
    );
    assert!(
        !row.contains("model=exec=") && !row.contains("model=rev="),
        "a model was invented: {row}"
    );
    // A recorded but blank model id is no identity at all, so it reads the
    // same way: an empty string must never print as if it named a model.
    let mut blank = App::new();
    blank.on_event(&model_call("executor", "   ", 10, 5, None));
    let row = status_row(&blank, 100, 24);
    assert!(row.contains("model=none yet"), "blank model printed: {row}");
}

#[test]
fn a_cost_bearing_run_shows_the_recorded_cost() {
    let mut app = App::new();
    app.on_event(&model_call("executor", "exec-model", 100, 40, Some(0.25)));
    app.on_event(&model_call("reviewer", "rev-model", 20, 10, Some(0.25)));
    let row = status_row(&app, 100, 24);
    assert_eq!(metric(&row, "cost"), "cost=$0.50", "recorded cost: {row}");
    // Sub-cent money is real money: rounding it to a `$0.00` would read as a
    // free run, so the amount keeps the digits it actually has.
    let mut sub = App::new();
    sub.on_event(&model_call("executor", "exec-model", 100, 40, Some(0.0042)));
    let row = status_row(&sub, 100, 24);
    assert_eq!(
        metric(&row, "cost"),
        "cost=$0.0042",
        "sub-cent cost lost: {row}"
    );
}

#[test]
fn an_unrecorded_cost_is_not_a_free_run() {
    // `cost_usd: None` means the provider reported no cost at all, which is
    // not the same claim as "this run cost nothing".
    let mut none = App::new();
    none.on_event(&model_call("executor", "exec-model", 10, 5, None));
    let none_row = status_row(&none, 100, 24);
    assert_eq!(metric(&none_row, "cost"), "cost=unrecorded", "{none_row}");
    assert!(
        !none_row.contains('$'),
        "an amount was invented: {none_row}"
    );

    // A recorded zero is the opposite claim, and must read differently.
    let mut free = App::new();
    free.on_event(&model_call("executor", "exec-model", 10, 5, Some(0.0)));
    let free_row = status_row(&free, 100, 24);
    assert_eq!(metric(&free_row, "cost"), "cost=$0.00", "{free_row}");
    assert_ne!(
        metric(&none_row, "cost"),
        metric(&free_row, "cost"),
        "unknown and free read the same"
    );
}

#[test]
fn the_pinned_status_strings_are_still_byte_identical() {
    // The P1a tokens and the run prefix are pinned: the metrics the row
    // gained are appended after them, never folded into them.
    let idle = App::new().status_line();
    assert!(
        idle.starts_with("calls=0 in=0 out=0 pass=0 fail=0"),
        "the idle counters shifted: {idle}"
    );
    let row = status_row(&App::new(), 100, 24);
    assert!(
        row.starts_with("calls=0 in=0 out=0 pass=0 fail=0"),
        "the rendered idle row shifted: {row}"
    );
    let mut settled = App::new();
    settled.run_mode = RunMode::Finished;
    settled.on_event(&TraceEvent::ReviewVerdict {
        pass: true,
        feedback: "ok".into(),
    });
    let line = settled.status_line();
    assert!(
        line.starts_with("finished · calls=0 in=0 out=0 pass=1 fail=0"),
        "the run prefix or the tokens shifted: {line}"
    );
}

#[test]
fn the_status_metrics_come_from_the_reduced_trace() {
    let events = vec![
        model_call("executor", "exec-model", 100, 40, Some(0.25)),
        TraceEvent::ReviewVerdict {
            pass: true,
            feedback: "ok".into(),
        },
        model_call("executor", "exec-model", 250, 60, Some(0.25)),
        TraceEvent::ReviewVerdict {
            pass: false,
            feedback: "not yet".into(),
        },
        model_call("reviewer", "rev-model", 30, 5, None),
    ];
    let mut app = App::new();
    for event in &events {
        app.on_event(event);
    }
    // The live reducer and the replay fold are the only two writers of these
    // numbers, so feeding the same events through both must agree: a row
    // built from anywhere else would be a number no trace reducer produced.
    assert_eq!(
        app.counters,
        Counters::fold(&events),
        "the live reducer drifted from the fold"
    );
    // The expected row is derived from those same events, here in the test.
    let mut expected = String::new();
    let mut in_tokens = 0;
    let mut out_tokens = 0;
    let mut cost = 0.0;
    let mut pass = 0;
    let mut fail = 0;
    for event in &events {
        match event {
            TraceEvent::ModelCall {
                input_tokens,
                output_tokens,
                cost_usd,
                ..
            } => {
                in_tokens += input_tokens;
                out_tokens += output_tokens;
                cost += cost_usd.unwrap_or(0.0);
            }
            TraceEvent::ReviewVerdict { pass: ok, .. } => {
                if *ok {
                    pass += 1;
                } else {
                    fail += 1;
                }
            }
            _ => {}
        }
    }
    expected.push_str(&format!(
        "calls={} in={in_tokens} out={out_tokens} pass={pass} fail={fail}",
        events
            .iter()
            .filter(|event| matches!(event, TraceEvent::ModelCall { .. }))
            .count()
    ));
    let row = status_row(&app, 100, 24);
    assert!(
        row.contains(&expected),
        "row disagrees with the events: {row}"
    );
    // 0.25 + 0.25 is the whole recorded spend; the call that carried no cost
    // adds nothing and does not make the sum look unknown.
    assert_eq!(metric(&row, "cost"), "cost=$0.50", "cost: {row}");
    assert!(
        row.contains("model=exec=exec-model rev=rev-model"),
        "identity is not the reduced last call per path: {row}"
    );
    assert_eq!(cost, 0.5, "the test's own sum drifted");
}

#[test]
fn the_status_row_fits_the_short_terminals() {
    let mut app = App::new();
    app.transcript.push("tail line".into());
    app.begin_run("short");
    app.on_event(&model_call("executor", "exec-model", 100, 40, Some(0.25)));
    app.on_event(&model_call("reviewer", "rev-model", 20, 10, Some(0.25)));
    for (width, height) in [(60u16, 9u16), (60, 12)] {
        let rows = rendered_rows(&app, width, height);
        assert_eq!(rows.len(), height as usize, "{width}x{height}");
        let out: String = rows.concat();
        for title in ["transcript", "status", "composer"] {
            assert!(
                out.contains(title),
                "{title} lost at {width}x{height}: {out}"
            );
        }
        let at = |title: &str| rows.iter().position(|row| row.contains(title)).unwrap();
        // The transcript keeps its 3-row floor, so it still has an interior
        // row to read: the metrics row never took the space instead.
        let transcript_top = at("transcript");
        assert!(
            !rows[transcript_top + 1]
                .trim_matches(|c: char| c == '│' || c.is_whitespace())
                .is_empty(),
            "the transcript lost its interior row at {width}x{height}: {out}"
        );
        // The metrics row is still ONE row, so it cannot push the transcript
        // or the composer out of a 9-row terminal.
        assert_eq!(
            at("composer") - at("status"),
            3,
            "the status row changed height at {width}x{height}: {out}"
        );
        // The pinned counters stay inside a 60-column row: the metrics are
        // appended, never spliced into the tokens, so what the narrow row
        // clips is the trailing metrics, not the run's own accounting.
        let row = status_row(&app, width, height);
        assert!(
            row.contains("calls=2 in=120 out=50 pass=0 fail=0"),
            "the pinned counters are not readable at {width}x{height}: {row}"
        );
    }
}

// ---- P3 F1: built-in themes chosen through `/theme` ----
//
// A theme is visible only through the rendered cells, so these tests read
// the TestBackend buffer's colors, not just its text. Every assertion here
// is a claim about what a user would SEE change.

use ratatui::style::Color;

/// The foreground color of every rendered cell, row-major. Two frames with
/// the same colors here are the same picture as far as a terminal is
/// concerned, so this is what the round-trip compatibility claim compares.
fn rendered_colors(app: &App, width: u16, height: u16) -> Vec<Color> {
    use ratatui::{backend::TestBackend, Terminal};
    use rof::tui::ui::draw;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.fg)
        .collect()
}

/// The full frame: every cell's symbol AND color. The compatibility round
/// trip compares this, because a theme that moved a color but also shifted
/// a glyph would still be a visible change.
fn rendered_frame(app: &App, width: u16, height: u16) -> Vec<(String, Color)> {
    use ratatui::{backend::TestBackend, Terminal};
    use rof::tui::ui::draw;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| (cell.symbol().to_string(), cell.fg))
        .collect()
}

/// The color of one titled pane's top border: the `┌` cell that opens the
/// frame, found by locating the pane's title on its own border. Asserting
/// per-pane is what stops "one pane changed" from passing as "the theme
/// applied".
fn pane_border_color(app: &App, title: &str, width: u16, height: u16) -> Color {
    use ratatui::{backend::TestBackend, Terminal};
    use rof::tui::ui::draw;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let cols = width as usize;
    for row in buf.content().chunks(cols) {
        let text: String = row.iter().map(|c| c.symbol()).collect();
        // EVERY `┌` on the row, not just the first: the wide layout puts
        // the transcript and the run pane side by side, so two pane frames
        // open on the same row and checking only the first would report a
        // present pane as missing.
        for (offset, _) in text.match_indices('┌') {
            if text[offset..].starts_with(&format!("┌{title}")) {
                return row[text[..offset].chars().count()].fg;
            }
        }
    }
    panic!("no {title} pane at {width}x{height}");
}

/// The five panes a theme has to reach, and the frame each one owns.
const ALL_PANES: [(&str, &str); 5] = [
    ("transcript", "transcript"),
    ("run activity", "run activity"),
    ("diff", "diff"),
    ("status", "status"),
    ("composer", "composer"),
];

/// A console with the same content in it every time, under one theme. The
/// tests compare frames across themes, so the CONTENT has to be held fixed
/// and only the palette varies; `App` is deliberately not `Clone` (its
/// reducer owns the coupling between fields), so the content is rebuilt
/// rather than copied.
fn themed(theme: rof::tui::theme::Theme, goal: &str) -> App {
    let mut app = App::new();
    app.theme = theme;
    if !goal.is_empty() {
        app.begin_run(goal);
        app.on_event(&TraceEvent::SessionStart {
            session_id: "s".into(),
            goal: goal.to_string(),
        });
    }
    app.on_event(&diff_event(
        &["src/tui/theme.rs"],
        "1 file changed",
        SAMPLE_PATCH,
        false,
    ));
    app
}

/// Apply one slash action the way the between-goals path does.
fn apply(app: &mut App, raw: &str) {
    let trace = TraceSink::new();
    let mut awaiting_key: Option<String> = None;
    let action = rof::tui::cmd::parse(raw).expect("test raw is a command");
    assert!(
        !rof::tui::run::apply_action(app, &trace, action, raw, &mut awaiting_key),
        "{raw} must not end the console"
    );
}

/// `/theme` with no name LISTS: every available theme is named and the
/// current one is marked, so a user can see both the choices and where they
/// are without reading anything else.
#[test]
fn theme_listing_names_every_choice_and_reports_the_current_one() {
    let mut app = App::new();
    apply(&mut app, "/theme");
    let listing = app
        .transcript
        .last()
        .cloned()
        .expect("/theme wrote no line");
    for name in ["default", "dark", "light"] {
        assert!(
            listing.contains(name),
            "the listing omits {name}: {listing}"
        );
    }
    assert!(
        listing.contains("default (current)") || listing.contains("current: default"),
        "the listing does not report the current theme: {listing}"
    );
    // Listing is a read: it must not have moved the selection.
    assert_eq!(app.theme, rof::tui::theme::Theme::default());
}

/// Every named theme repaints the frame a user can see, and returning to
/// `default` restores the pre-change frame cell for cell. That round trip IS
/// the terminal-compatibility guarantee: a user who never picks a theme
/// sees exactly the frame they saw before this feature existed.
#[test]
fn each_theme_repaints_the_frame_and_default_restores_it_exactly() {
    use rof::tui::theme::Theme;
    let baseline = App::new();
    let before = rendered_frame(&baseline, 100, 30);
    for theme in [Theme::Dark, Theme::Light] {
        let mut app = App::new();
        app.theme = theme;
        let after = rendered_frame(&app, 100, 30);
        assert_ne!(
            before, after,
            "{theme:?} rendered a byte-identical frame: the palette did not reach the panes"
        );
        // A theme that changes the picture must change MORE THAN ONE pane:
        // a single repainted frame is not a theme.
        let recolored = before
            .iter()
            .zip(after.iter())
            .filter(|(a, b)| a.1 != b.1)
            .count();
        assert!(
            recolored > 1,
            "{theme:?} changed only {recolored} cell colors"
        );
        // And the glyphs did not move: a theme is a palette, not a layout.
        assert!(
            before.iter().map(|c| &c.0).eq(after.iter().map(|c| &c.0)),
            "{theme:?} changed the layout, not just the colors"
        );
        // Back to the default palette, the frame is the pre-change frame.
        app.theme = Theme::default();
        assert_eq!(
            before,
            rendered_frame(&app, 100, 30),
            "{theme:?} -> default did not restore the original frame"
        );
    }
}

/// An unknown theme name is refused and repaints nothing: not the
/// selection, not the palette, not the frame. A quiet fallback to the
/// default theme would be the worst possible outcome, because the user's
/// terminal would be repainted by a command they got wrong. The refusal
/// itself does add a transcript line — the user is owed the reason — so
/// what is compared is the frame's COLORS, which is the claim that no
/// palette was applied.
#[test]
fn an_unknown_theme_name_is_refused_and_repaints_nothing() {
    let app_before = App::new();
    let colors_before = rendered_colors(&app_before, 100, 30);
    let mut app = App::new();
    apply(&mut app, "/theme neon");
    assert_eq!(
        app.theme,
        rof::tui::theme::Theme::default(),
        "a refused name still moved the selection"
    );
    assert_eq!(
        colors_before,
        rendered_colors(&app, 100, 30),
        "a refused name still repainted the frame"
    );
    let said: String = app.transcript.join("\n");
    for name in ["default", "dark", "light"] {
        assert!(
            said.contains(name),
            "the refusal omits the valid name {name}: {said}"
        );
    }
    assert!(
        !said.contains("theme=default") && !said.contains("theme=neon"),
        "a refused name reported a switch it did not make: {said}"
    );
}

/// A switch names the theme it applied, so the transcript confirms the
/// selection instead of leaving the user to guess from the repaint.
#[test]
fn switching_a_theme_confirms_the_new_theme_by_name() {
    let mut app = App::new();
    apply(&mut app, "/theme dark");
    assert_eq!(app.theme, rof::tui::theme::Theme::Dark);
    let said = app
        .transcript
        .last()
        .cloned()
        .expect("no confirmation line");
    assert!(said.contains("dark"), "no confirmation by name: {said}");
    assert!(
        !said.contains("next goal") && !said.contains("next session"),
        "a display-only switch must not claim it is deferred: {said}"
    );
}

/// The selection is App state, so it outlives a run. A theme that reverted
/// on `begin_run` would repaint the screen under the user mid-session, and
/// the goal in flight has no reason to touch the display.
#[test]
fn a_theme_survives_the_run_lifecycle_and_renders_in_both_layouts() {
    use rof::tui::theme::Theme;
    let mut app = App::new();
    app.theme = Theme::Light;
    app.begin_run("watch the run");
    assert_eq!(app.run_mode, RunMode::Running);
    assert_eq!(app.theme, Theme::Light, "begin_run reverted the theme");
    app.run_mode = RunMode::Finished;
    assert_eq!(app.theme, Theme::Light, "finish reverted the theme");

    // 60 columns is the narrow full-width stack, 160 the wide workspace
    // split. The theme follows the selection in both.
    for (width, layout) in [(60u16, "narrow"), (160, "wide")] {
        let dark = rendered_frame(&themed(Theme::Dark, "watch the run"), width, 40);
        assert_ne!(
            dark,
            rendered_frame(&app, width, 40),
            "{layout}: the theme did not reach the {layout} layout"
        );
    }
    // The panes a user reads are all present and all themed, in both sizes.
    for (width, layout) in [(60u16, "narrow"), (160, "wide")] {
        for (title, pane) in ALL_PANES {
            let light = pane_border_color(&app, pane, width, 40);
            let dark = pane_border_color(&themed(Theme::Dark, "watch the run"), pane, width, 40);
            assert_ne!(
                light, dark,
                "{layout}/{title}: the {title} pane ignored the theme"
            );
        }
    }
}

/// Replay renders through the same `draw`, so the selection must reach it
/// too: a user who picks a theme and then replays a trace is still looking
/// at the same console.
#[test]
fn a_theme_applies_in_replay_mode() {
    use rof::tui::theme::Theme;
    let mut app = App::new();
    app.set_replay_events(replay_events());
    assert!(app.replay_mode, "replay mode was not entered");
    let default_frame = rendered_frame(&app, 100, 30);
    app.theme = Theme::Light;
    let light_frame = rendered_frame(&app, 100, 30);
    assert_ne!(
        default_frame, light_frame,
        "replay rendered a byte-identical frame: the theme did not apply"
    );
    // The replay panes follow it, not just the composer's border.
    for (_, pane) in ALL_PANES {
        assert_ne!(
            pane_border_color(&app, pane, 100, 30),
            pane_border_color(&themed(Theme::Dark, ""), pane, 100, 30),
            "replay/{pane}: the pane ignored the theme"
        );
    }
    app.theme = Theme::default();
    assert_eq!(
        default_frame,
        rendered_frame(&app, 100, 30),
        "replay round trip to default did not restore the frame"
    );
}

/// Every pane the user reads follows the selection: the diff, the run
/// activity, the status row, the transcript, and the composer. Asserting
/// more than one pane is what makes "the theme applied" mean something
/// stronger than "some frame got repainted".
#[test]
fn diff_activity_status_transcript_and_composer_all_follow_the_theme() {
    use rof::tui::theme::Theme;
    let mut app = App::new();
    app.begin_run("ship the feature");
    app.on_event(&diff_event(
        &["src/tui/theme.rs"],
        "1 file changed",
        SAMPLE_PATCH,
        false,
    ));
    app.on_event(&TraceEvent::SessionStart {
        session_id: "s".into(),
        goal: "ship it".into(),
    });
    let mut repainted = 0;
    for (title, pane) in ALL_PANES {
        let light = pane_border_color(&app, pane, 160, 40);
        let dark = pane_border_color(&themed(Theme::Dark, "ship the feature"), pane, 160, 40);
        assert_ne!(
            light, dark,
            "{title}: the {title} pane ignored the selected theme"
        );
        repainted += 1;
    }
    assert_eq!(repainted, ALL_PANES.len(), "a pane was not checked");
}

/// A focused pane is the one with the highlight accent, so the focus
/// indicator is part of what a theme owns: a theme that repainted only
/// unfocused borders would leave the focus mark invisible.
#[test]
fn the_focus_accent_follows_the_theme_too() {
    use rof::tui::theme::Theme;
    let mut app = App::new();
    app.focus = Focus::Diff;
    let default_focus = pane_border_color(&app, "diff", 100, 30);
    app.theme = Theme::Light;
    assert_ne!(
        default_focus,
        pane_border_color(&app, "diff", 100, 30),
        "the focused pane's accent ignored the theme"
    );
}

/// While a goal is live, `/theme` is a view-style action: it applies at
/// once, to the display only. This test fails if the live route refuses it,
/// because a repaint never needs a running goal's permission.
#[test]
fn theme_applies_while_a_goal_is_live_and_is_not_refused() {
    let mut app = live_control_app();
    app.theme = rof::tui::theme::Theme::default();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<rof::engine::control::RunCommand>();
    let trace = TraceSink::new();
    let mut awaiting_key: Option<String> = None;
    let outcome = handle_running_action(
        &mut app,
        Action::Theme(Some("light".into())),
        &tx,
        "/theme light",
        &trace,
        &mut awaiting_key,
    );
    assert_eq!(
        outcome,
        RunningActionOutcome::View,
        "/theme must be a view action while live, not a refusal"
    );
    assert_eq!(
        app.theme,
        rof::tui::theme::Theme::Light,
        "/theme did not take effect immediately while live"
    );
    // It is a DISPLAY change: nothing is waiting for the next goal.
    assert!(
        app.deferred_config.is_empty(),
        "/theme deferred a setting: {:?}",
        app.deferred_config
    );
}

/// The same live action must send nothing on the command channel: a theme
/// is a palette, and the worker has no use for one. A send here would mean
/// the action reached into the run it was only supposed to repaint.
#[test]
fn theme_while_live_sends_nothing_on_the_command_channel() {
    let mut app = live_control_app();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<rof::engine::control::RunCommand>();
    let trace = TraceSink::new();
    let mut awaiting_key: Option<String> = None;
    let _ = handle_running_action(
        &mut app,
        Action::Theme(Some("dark".into())),
        &tx,
        "/theme dark",
        &trace,
        &mut awaiting_key,
    );
    assert_eq!(
        app.theme,
        rof::tui::theme::Theme::Dark,
        "the theme did not apply, so this test proved nothing"
    );
    assert!(
        rx.try_recv().is_err(),
        "/theme sent a command to the running goal: {:?}",
        rx.try_recv().ok()
    );
}

/// `/display` reports the mode as NOT available rather than pretending a
/// switch happened. The console owns the alternate screen for the whole
/// session and leaves it on the way out (the P1a terminal-restoration
/// contract), so honouring `regular` would mean giving up one half of that
/// pair. Saying "not available" is the honest answer, and the file records
/// the posture the console actually rendered — see `tui_prefs.rs`.
#[test]
fn display_reports_the_mode_as_not_available_instead_of_faking_a_switch() {
    for mode in ["fullscreen", "regular"] {
        let mut app = App::new();
        apply(&mut app, &format!("/display {mode}"));
        let line = app
            .transcript
            .last()
            .cloned()
            .expect("/display wrote no line");
        assert!(
            line.contains(mode),
            "the line does not name the mode: {line}"
        );
        assert!(
            line.contains("not available"),
            "/display implied a switch it cannot make: {line}"
        );
        // It is a report, not a state change: nothing on the console moved.
        assert_eq!(app.theme, rof::tui::theme::Theme::default());
    }
}

/// The closed parser refuses a mode that is not one, and the refusal names
/// the same two the preferences file validates against.
#[test]
fn display_refuses_a_mode_that_is_not_a_mode() {
    assert!(matches!(
        rof::tui::cmd::parse("/display inline"),
        Some(rof::tui::cmd::Action::Unknown(_))
    ));
    assert!(matches!(
        rof::tui::cmd::parse("/display fullscreen"),
        Some(rof::tui::cmd::Action::Display(ref m)) if m == "fullscreen"
    ));
}

/// The splash mascot is chosen ONCE per console, not once per frame.
///
/// The bug this pins: `splash::draw` read the clock itself, so every one of
/// the pump's ~30 frames a second rolled a new sprite and the mascot
/// flickered through all twelve while you were still reading the title.
/// The index is `App` state now, and this asserts the rendered frame is
/// byte-identical across repeated draws of the same `App`.
#[test]
fn the_splash_mascot_is_chosen_once_and_holds_still() {
    let app = App::new();
    assert!(
        app.mascot < rof::tui::splash::SPRITE_COUNT,
        "the chosen sprite is out of range: {}",
        app.mascot
    );

    // Repeated draws of the same App are identical. Comparing rendered
    // frames, not the index, is the point: a clock read inside `draw` would
    // produce a different frame every time.
    let first = rendered_rows_splash(&app, 100, 30);
    for _ in 0..5 {
        assert_eq!(
            first,
            rendered_rows_splash(&app, 100, 30),
            "the splash redrew a different mascot"
        );
    }

    // The pick is a pure function of its seed, so a console is reproducible
    // once chosen, and the whole set stays reachable.
    for seed in [0u32, 1, 7, 999, u32::MAX] {
        assert_eq!(
            rof::tui::splash::pick(seed),
            rof::tui::splash::pick(seed),
            "the pick is not stable for seed {seed}"
        );
        assert!(rof::tui::splash::pick(seed) < rof::tui::splash::SPRITE_COUNT);
    }
    let seen: std::collections::HashSet<usize> =
        (0..64).map(|n| rof::tui::splash::pick(n * 97)).collect();
    assert!(
        seen.len() > 1,
        "every seed picked the same sprite: the mascot would never vary"
    );
}
