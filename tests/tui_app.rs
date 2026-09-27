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
    // Scrolling moves the patch window and nothing else: same frame, same
    // panes, composer still docked. `App` owns the one scroll position, so
    // this is the same scroll the transcript uses, not a second one.
    app.scroll_lines(10_000);
    let up = rendered_rows(&app, 100, 24);
    assert_eq!(up.len(), 24, "scrolling changed the frame height");
    let up: String = up.concat();
    assert!(
        up.contains("+ line 159"),
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
