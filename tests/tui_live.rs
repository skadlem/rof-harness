//! Live run state: bounded activity, the run lifecycle, and the
//! `LiveSession` channel reducer. No terminal, no network, no model: real
//! `TraceEvent` values and real tokio channels only.

use rof::obs::{Boundary, GoalFinished, LiveEvent, TraceEvent};
use rof::tui::app::{App, RunMode};
use rof::tui::run::LiveSession;
use tokio::sync::mpsc::unbounded_channel;

fn transition(from: &str, to: &str) -> TraceEvent {
    TraceEvent::StateTransition {
        from: from.to_string(),
        to: to.to_string(),
    }
}

/// Drain until the worker reports a terminal outcome, yielding between
/// attempts. Deterministic (no wall-clock sleep) and bounded, so a stuck
/// worker fails the test instead of hanging it.
async fn drain_until_terminal(session: &mut LiveSession, app: &mut App) -> GoalFinished {
    for _ in 0..10_000 {
        tokio::task::yield_now().await;
        if let Some(outcome) = session.drain(app) {
            return outcome;
        }
    }
    panic!("worker never reached a terminal outcome");
}

#[test]
fn live_events_update_activity_and_lifecycle() {
    let mut app = App::new();
    app.begin_run("fix the parser");
    assert_eq!(app.run_mode, RunMode::Running);
    assert_eq!(app.run_goal, "fix the parser");

    app.on_event(&transition("ready", "implementing"));
    assert_eq!(app.activity_tail(10).len(), 1);
    assert!(app.activity_tail(10)[0].contains("implementing"));
    assert_eq!(app.transcript.len(), 1);

    app.on_live_boundary(Boundary::Finished);
    assert_eq!(app.run_mode, RunMode::Running);
    app.on_live_finished(&GoalFinished {
        passed: true,
        error: None,
    });
    assert_eq!(app.run_mode, RunMode::Finished);
    assert!(app.status_line().contains("finished"));
    assert!(app.transcript.last().unwrap().contains("run finished"));
    assert_eq!(
        app.run_outcome.as_deref(),
        Some(app.transcript.last().unwrap().as_str())
    );
}

#[test]
fn a_failed_outcome_marks_the_run_failed() {
    let mut app = App::new();
    app.begin_run("break it");
    app.set_stopping();
    assert_eq!(app.run_mode, RunMode::Stopping);
    app.on_live_finished(&GoalFinished {
        passed: false,
        error: Some("checks failed".into()),
    });
    assert_eq!(app.run_mode, RunMode::Failed);
    assert!(app.status_line().contains("failed"));
    assert!(app
        .run_outcome
        .as_deref()
        .unwrap()
        .contains("checks failed"));
}

#[test]
fn set_stopping_only_moves_a_running_run() {
    let mut app = App::new();
    app.set_stopping();
    assert_eq!(app.run_mode, RunMode::Idle);
    app.begin_run("goal");
    app.set_stopping();
    app.set_stopping();
    assert_eq!(app.run_mode, RunMode::Stopping);
    // A finished boundary alone decides nothing; the outcome does.
    app.on_live_boundary(Boundary::Finished);
    assert_eq!(app.run_mode, RunMode::Stopping);
    assert!(app.run_outcome.is_none());
}

#[test]
fn live_activity_is_bounded_by_lines_and_characters() {
    let mut app = App::new();
    app.begin_run("bounded");
    for i in 0..260 {
        app.on_event(&transition(&format!("state-{i}"), &format!("next-{i}")));
    }
    assert_eq!(app.activity_tail(1000).len(), 200);
    assert!(app.activity_chars <= 32_000);

    let note = "x".repeat(4_000);
    for _ in 0..20 {
        app.on_event(&TraceEvent::GoalQuality {
            goal: "bounded".into(),
            note: note.clone(),
        });
    }
    assert!(app.activity_tail(1000).len() <= 200);
    assert!(app.activity_chars <= 32_000);
    // The newest line survives eviction; the oldest is gone.
    assert!(app.activity_tail(1)[0].contains(&note));
    assert!(app.activity_chars > 0);
    // The counter caches the deque's own char total; the two agree.
    let sum: usize = app.activity.iter().map(|l| l.chars().count()).sum();
    assert_eq!(app.activity_chars, sum);
}

#[test]
fn activity_tail_returns_the_last_lines_in_order() {
    let mut app = App::new();
    app.begin_run("tail");
    for i in 0..10 {
        app.on_event(&transition(&format!("s{i}"), &format!("t{i}")));
    }
    let tail = app.activity_tail(3);
    assert_eq!(tail.len(), 3);
    assert!(tail[0].contains("t7") && tail[2].contains("t9"));
    assert!(app.activity_tail(0).is_empty());
}

#[test]
fn a_started_boundary_never_reopens_a_resolved_run() {
    let mut app = App::new();
    app.begin_run("first goal");
    app.on_live_finished(&GoalFinished {
        passed: true,
        error: None,
    });
    assert_eq!(app.run_mode, RunMode::Finished);
    // A `Started` from a late or stale worker must not undo the outcome.
    app.on_live_boundary(Boundary::Started);
    assert_eq!(app.run_mode, RunMode::Finished);

    let mut app = App::new();
    app.begin_run("second goal");
    app.on_live_finished(&GoalFinished {
        passed: false,
        error: Some("checks failed".into()),
    });
    assert_eq!(app.run_mode, RunMode::Failed);
    app.on_live_boundary(Boundary::Started);
    assert_eq!(app.run_mode, RunMode::Failed);
}

#[tokio::test]
async fn live_session_reports_finished_once() {
    let (tx, rx) = unbounded_channel();
    let mut app = App::new();
    let mut session = LiveSession::new(rx);
    // The worker stays pending: the session reports the channel's terminal
    // event, not the task's completion.
    let (hold_tx, mut hold_rx) = unbounded_channel::<()>();
    let handle = tokio::spawn(async move {
        hold_rx.recv().await;
        Ok(GoalFinished {
            passed: true,
            error: None,
        })
    });
    session.begin("goal", handle);
    assert!(session.is_running());
    assert!(!session.stop_requested());
    assert!(session.drain(&mut app).is_none());
    assert_eq!(app.run_mode, RunMode::Idle);
    // A stop requested during the run is answered by the outcome, so a
    // resolved run cannot report a stale stop latch.
    session.request_stop();

    tx.send(LiveEvent::Trace(transition("a", "b"))).unwrap();
    tx.send(LiveEvent::Boundary(Boundary::Started)).unwrap();
    tx.send(LiveEvent::Boundary(Boundary::Finished)).unwrap();
    tx.send(LiveEvent::Finished(GoalFinished {
        passed: true,
        error: None,
    }))
    .unwrap();

    let outcome = session.drain(&mut app).expect("terminal outcome");
    assert!(outcome.passed);
    assert_eq!(app.run_mode, RunMode::Finished);
    assert!(!session.stop_requested());
    assert!(session.drain(&mut app).is_none());
    assert!(!session.is_running());
    assert!(!session.stop_requested());
    drop(hold_tx);
}

#[tokio::test]
async fn request_stop_does_not_abort_the_worker() {
    let (_tx, rx) = unbounded_channel::<LiveEvent>();
    let mut app = App::new();
    let mut session = LiveSession::new(rx);
    let (hold_tx, hold_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, mut done_rx) = unbounded_channel::<()>();
    let handle = tokio::spawn(async move {
        hold_rx.await.ok();
        done_tx.send(()).ok();
        Ok(GoalFinished {
            passed: true,
            error: None,
        })
    });
    session.begin("stop me", handle);
    session.request_stop();
    assert!(session.stop_requested());
    assert!(session.is_running());
    // The worker is still running, so a drain reports no outcome.
    assert!(session.drain(&mut app).is_none());

    // A stop is a request, not an abort: the worker still reaches its end.
    // An aborted task would drop `done_tx` and close the channel, which
    // `recv` reports as `None` rather than a value.
    hold_tx.send(()).unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), done_rx.recv())
            .await
            .expect("worker did not finish after the stop request")
            .is_some(),
        "the worker was aborted instead of asked to stop"
    );
}

#[tokio::test]
async fn a_worker_that_exits_without_an_outcome_fails_the_run() {
    let (tx, rx) = unbounded_channel();
    let mut app = App::new();
    let mut session = LiveSession::new(rx);
    let handle = tokio::spawn(async {
        Ok(GoalFinished {
            passed: true,
            error: None,
        })
    });
    session.begin("silent worker", handle);
    // The queued event is applied by the same drain that reports the
    // fallback, so it must land before the outcome line.
    tx.send(LiveEvent::Trace(transition("queued", "applied")))
        .unwrap();

    let outcome = drain_until_terminal(&mut session, &mut app).await;
    assert!(!outcome.passed);
    assert!(outcome
        .error
        .unwrap()
        .contains("without a terminal outcome"));
    assert_eq!(app.run_mode, RunMode::Failed);
    let event_at = app
        .transcript
        .iter()
        .position(|line| line.contains("applied"))
        .expect("the queued trace event was not applied");
    let outcome_at = app
        .transcript
        .iter()
        .position(|line| line.contains("run failed"))
        .expect("the outcome line is missing");
    assert!(
        event_at < outcome_at,
        "the outcome preceded its trace event"
    );
    assert!(session.drain(&mut app).is_none());
}

#[tokio::test]
async fn reset_is_a_no_op_until_the_worker_is_terminal() {
    let (tx, rx) = unbounded_channel();
    let mut app = App::new();
    let mut session = LiveSession::new(rx);
    let (hold_tx, mut hold_rx) = unbounded_channel::<()>();
    let handle = tokio::spawn(async move {
        hold_rx.recv().await;
        Ok(GoalFinished {
            passed: true,
            error: None,
        })
    });
    session.begin("goal", handle);
    session.request_stop();

    // A stale event from the previous run plus a still-publishing worker:
    // reset must neither strand the run nor drop what it still owes.
    tx.send(LiveEvent::Trace(transition("stale", "while-running")))
        .unwrap();
    session.reset();
    assert!(session.is_running(), "reset stranded the live run");
    assert!(session.stop_requested(), "reset cleared a live stop latch");
    assert!(session.drain(&mut app).is_none());
    assert!(
        app.transcript
            .iter()
            .any(|line| line.contains("while-running")),
        "reset erased an event the live run still owed"
    );

    // Terminal: the finished handle resolves through the fallback, which
    // also clears the stop latch the run was holding.
    hold_tx.send(()).unwrap();
    let outcome = drain_until_terminal(&mut session, &mut app).await;
    assert!(!outcome.passed);
    assert!(!session.is_running());
    assert!(!session.stop_requested());

    // Only now may reset drain the leftovers of the finished run.
    tx.send(LiveEvent::Trace(transition("stale", "after-reset")))
        .unwrap();
    session.reset();
    assert!(!session.is_running());

    let (hold_tx, mut hold_rx) = unbounded_channel::<()>();
    let handle = tokio::spawn(async move {
        hold_rx.recv().await;
        Ok(GoalFinished {
            passed: true,
            error: None,
        })
    });
    session.begin("next goal", handle);
    app.begin_run("next goal");
    assert!(session.drain(&mut app).is_none());
    assert!(
        !app.transcript
            .iter()
            .any(|line| line.contains("after-reset")),
        "the next run applied the previous run's event"
    );
    drop(hold_tx);
}

#[test]
fn a_scrolled_transcript_holds_its_anchor_until_end() {
    let mut app = App::new();
    app.begin_run("scroll");
    for i in 0..5 {
        app.on_event(&TraceEvent::SessionStart {
            session_id: format!("s{i}"),
            goal: format!("line {i}"),
        });
    }
    app.scroll_lines(1);
    let scroll = app.scroll;
    let oldest = app.transcript.first().cloned().unwrap();

    app.on_event(&TraceEvent::SessionStart {
        session_id: "live".into(),
        goal: "live line".into(),
    });

    assert_eq!(app.scroll, scroll);
    assert_eq!(app.transcript.first().cloned().unwrap(), oldest);
    assert_eq!(app.transcript.last().unwrap(), "▶ live line");
    assert!(app
        .activity_tail(10)
        .iter()
        .any(|l| l.contains("live line")));

    app.scroll_lines(isize::MIN);
    assert_eq!(app.scroll, 0);
}

#[test]
fn replay_mode_records_no_live_activity() {
    let mut app = App::new();
    app.set_replay_events(vec![TraceEvent::SessionStart {
        session_id: "s".into(),
        goal: "recorded".into(),
    }]);
    assert!(app.replay_mode);
    app.on_event(&transition("ready", "implementing"));
    assert!(app.activity.is_empty());
    assert_eq!(app.activity_chars, 0);
    assert_eq!(app.run_mode, RunMode::Idle);
    let status = app.status_line();
    assert!(status.contains("replay"), "{status}");
    assert!(!status.contains("running"), "{status}");
}
