//! Live run state: bounded activity, the run lifecycle, and the
//! `LiveSession` channel reducer. No terminal, no network, no model: real
//! `TraceEvent` values and real tokio channels only.

use crossterm::event::KeyCode;
use rof::obs::{
    Boundary, ControlAck, ControlKind, ControlStatus, GoalFinished, LiveEvent, TraceEvent,
    TraceSink,
};
use rof::tui::app::{App, BusyMode, DeferredConfig, RunMode};
use rof::tui::run::{handle_running_key, LiveSession, RunningKeyOutcome};
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

#[test]
fn app_tracks_pending_control_and_acknowledges_it() {
    let mut app = App::new();
    app.begin_run("first");
    app.set_busy_mode(BusyMode::Queue);
    let id = app.submit_pending_goal("second goal");
    assert_eq!(app.pending_goal.as_ref().map(|p| p.id), Some(id));
    assert!(app.control_summary().contains("queued"));

    app.on_control_ack(ControlAck {
        id,
        kind: ControlKind::Queue,
        status: ControlStatus::Applied,
        note: "queued".into(),
    });
    // An applied queue ack means the worker took the goal, not that the
    // goal is over: the slot is still owed to the next boundary.
    assert_eq!(
        app.pending_goal.as_ref().map(|p| p.id),
        Some(id),
        "an applied queue ack freed the goal before the boundary consumed it"
    );
    assert!(app.control_summary().contains("goal queued"));
    assert!(app.transcript.iter().any(|line| line.contains("queued")));

    app.on_live_boundary(Boundary::Started);
    assert!(
        app.pending_goal.is_none(),
        "the boundary did not consume the queued goal"
    );
    assert_eq!(app.run_goal, "second goal");
    assert_eq!(app.run_mode, RunMode::Running);
}

/// A queued goal that the worker refuses never reaches a boundary, so its
/// rejection is the answer that frees the slot and leaves no goal pending.
#[test]
fn a_rejected_queue_ack_frees_the_goal_slot() {
    let mut app = App::new();
    app.begin_run("first");
    let id = app.submit_pending_goal("second goal");

    app.on_control_ack(ControlAck {
        id,
        kind: ControlKind::Queue,
        status: ControlStatus::Rejected,
        note: "queue closed".into(),
    });
    assert!(app.pending_goal.is_none());
    assert!(!app.control_summary().contains("goal queued"));
    assert_eq!(app.run_goal, "first", "a rejected goal started a run");
    assert_eq!(app.run_mode, RunMode::Running);
    assert_eq!(
        app.last_control_ack.as_ref().map(|a| a.status),
        Some(ControlStatus::Rejected)
    );
}

#[test]
fn started_boundary_consumes_only_a_pending_queued_goal() {
    let mut app = App::new();
    app.begin_run("first");
    app.submit_pending_goal("second goal");
    app.on_live_boundary(Boundary::Started);
    assert_eq!(app.run_goal, "second goal");
    assert!(app.pending_goal.is_none());
    assert_eq!(app.run_mode, RunMode::Running);
}

/// Steer and queue each hold exactly one slot: a later submission takes it
/// over under a new id, and the ids are monotonic so the displaced command
/// is still addressable when the worker rejects it.
#[test]
fn a_later_submission_replaces_the_pending_slot_under_a_new_id() {
    let mut app = App::new();
    app.begin_run("busy");
    let first_steer = app.submit_pending_steer("focus on the parser");
    let first_goal = app.submit_pending_goal("next goal");
    let second_steer = app.submit_pending_steer("focus on the lexer");
    let second_goal = app.submit_pending_goal("later goal");

    assert!(second_steer > first_steer && second_goal > first_goal);
    assert_eq!(app.pending_steer.as_ref().map(|p| p.id), Some(second_steer));
    assert_eq!(
        app.pending_steer.as_ref().map(|p| p.text.as_str()),
        Some("focus on the lexer")
    );
    assert_eq!(app.pending_goal.as_ref().map(|p| p.id), Some(second_goal));
    assert_eq!(
        app.pending_goal.as_ref().map(|p| p.text.as_str()),
        Some("later goal")
    );
    assert!(app.take_control_id() > second_goal);
}

/// An acknowledgement is matched by id, not by slot or arrival order: a
/// rejection for the displaced command must leave the command that
/// replaced it pending, while its own rejection frees the slot.
#[test]
fn a_rejected_ack_clears_only_the_slot_it_names() {
    let mut app = App::new();
    app.begin_run("busy");
    let displaced = app.submit_pending_goal("old goal");
    let current = app.submit_pending_goal("new goal");

    app.on_control_ack(ControlAck {
        id: displaced,
        kind: ControlKind::Queue,
        status: ControlStatus::Rejected,
        note: "replaced by a later queue".into(),
    });
    assert_eq!(
        app.pending_goal.as_ref().map(|p| p.id),
        Some(current),
        "a rejection for the displaced id cleared the live slot"
    );
    assert_eq!(app.last_control_ack.as_ref().map(|a| a.id), Some(displaced));

    app.on_control_ack(ControlAck {
        id: current,
        kind: ControlKind::Queue,
        status: ControlStatus::Rejected,
        note: "no next round".into(),
    });
    assert!(app.pending_goal.is_none());
    assert!(app.transcript.iter().any(|l| l.contains("no next round")));
    // A rejection names a kind, so an ack of the wrong kind cannot clear
    // the other slot even when the ids agree. `Rejected` is the status
    // that frees a queue slot, so this only passes while the kind is
    // honored: ignored kind matching would free the steer.
    let steer = app.submit_pending_steer("steer text");
    app.on_control_ack(ControlAck {
        id: steer,
        kind: ControlKind::Queue,
        status: ControlStatus::Rejected,
        note: "queue closed".into(),
    });
    assert!(
        app.pending_steer.is_some(),
        "a queue ack cleared the steer slot it never named"
    );
    let line = app.transcript.last().unwrap();
    assert!(line.contains("queue rejected"), "{line}");
    assert!(line.contains(&steer.to_string()), "{line}");
}

/// `Interrupt` is a posture, not a text submission: it occupies no slot
/// and changes no run lifecycle, and a stop acknowledgement answers
/// without disturbing a pending steer or goal.
#[test]
fn the_stop_busy_mode_holds_no_text_and_moves_no_run_state() {
    let mut app = App::new();
    app.begin_run("busy");
    app.submit_pending_steer("steer text");
    app.set_busy_mode(BusyMode::Interrupt);

    assert_eq!(app.busy_mode, BusyMode::Interrupt);
    assert!(app.control_summary().contains("interrupt"));
    assert_eq!(app.run_mode, RunMode::Running);
    assert!(app.pending_steer.is_some());

    let stop = app.take_control_id();
    app.on_control_ack(ControlAck {
        id: stop,
        kind: ControlKind::Stop,
        status: ControlStatus::Applied,
        note: "stopping at the next boundary".into(),
    });
    assert!(app.pending_steer.is_some(), "a stop ack cleared a steer");
    assert!(app.pending_goal.is_none());
    assert_eq!(
        app.last_control_ack.as_ref().map(|a| a.kind),
        Some(ControlKind::Stop)
    );
    assert!(app.transcript.iter().any(|l| l.contains("stop applied")));
}

/// Deferred configuration is display state for the next goal: it is held
/// on the console and named in the summary without any credential, in
/// submission order, so an earlier setting is never dropped.
#[test]
fn deferred_config_is_held_for_the_next_goal_only() {
    let mut app = App::new();
    app.begin_run("busy");
    assert!(app.deferred_config.is_empty());
    assert!(!app.control_summary().contains("next goal"));

    app.defer_config(DeferredConfig::Attempts(2));
    app.defer_config(DeferredConfig::Model {
        slot: Some("reasoning".into()),
        value: "provider/model".into(),
    });
    let summary = app.control_summary();
    assert!(summary.contains("applies to next goal"), "{summary}");
    assert!(summary.contains("2 deferred"), "{summary}");
    assert!(summary.contains("attempts 2"), "{summary}");
    assert!(summary.contains("reasoning provider/model"), "{summary}");
    assert_eq!(
        app.deferred_config,
        vec![
            DeferredConfig::Attempts(2),
            DeferredConfig::Model {
                slot: Some("reasoning".into()),
                value: "provider/model".into(),
            },
        ]
    );
    // A deferred setting is not a command: no slot is occupied and the
    // running goal is untouched.
    assert!(app.pending_goal.is_none());
    assert!(app.pending_steer.is_none());
    assert_eq!(app.run_mode, RunMode::Running);
    assert!(!app.transcript.iter().any(|l| l.contains("provider/model")));
}

/// A goal outcome is per-goal, not session-terminal: while a queued goal
/// is pending the run stays live, and the two reducers keep their own
/// wording so a transcript reader can tell them apart.
#[test]
fn a_goal_finished_outcome_is_not_the_terminal_run_finished() {
    let mut app = App::new();
    app.begin_run("first");
    app.submit_pending_goal("second goal");
    app.on_goal_finished(&GoalFinished {
        passed: true,
        error: None,
    });
    assert_eq!(app.run_mode, RunMode::Running, "a queued goal lost its run");
    assert_eq!(app.transcript.last().unwrap(), "goal finished: passed");
    // The queued goal then opens the next run.
    app.on_live_boundary(Boundary::Started);
    assert_eq!(app.run_goal, "second goal");

    // With nothing queued, the same per-goal reducer resolves the run and
    // still uses its own line.
    let mut app = App::new();
    app.begin_run("only goal");
    app.on_goal_finished(&GoalFinished {
        passed: false,
        error: Some("checks failed".into()),
    });
    assert_eq!(app.run_mode, RunMode::Failed);
    assert_eq!(app.transcript.last().unwrap(), "goal failed: checks failed");
    assert!(!app.transcript.iter().any(|l| l.contains("run failed")));
    // The session-terminal reducer owns the run's own outcome line.
    app.on_live_finished(&GoalFinished {
        passed: true,
        error: None,
    });
    assert_eq!(app.transcript.last().unwrap(), "run finished: passed");
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

#[test]
fn a_steer_ack_does_not_clear_the_queue_slot() {
    let mut app = App::new();
    app.begin_run("busy");
    let steer = app.submit_pending_steer("focus on the parser");
    let goal = app.submit_pending_goal("second goal");

    app.on_control_ack(ControlAck {
        id: steer,
        kind: ControlKind::Steer,
        status: ControlStatus::Applied,
        note: "steer acknowledged".into(),
    });

    assert!(app.pending_steer.is_none());
    assert_eq!(app.pending_goal.as_ref().map(|p| p.id), Some(goal));
}

/// A stop request must win over a stale Started event, including a queued
/// goal that must not be consumed while the run is stopping.
#[test]
fn a_stopping_run_does_not_reopen_on_a_stale_started_boundary() {
    let mut app = App::new();
    app.begin_run("first");
    app.submit_pending_goal("second goal");
    app.set_stopping();

    app.on_live_boundary(Boundary::Started);

    assert_eq!(app.run_mode, RunMode::Stopping);
    assert_eq!(app.run_goal, "first");
    assert_eq!(
        app.pending_goal.as_ref().map(|p| p.text.as_str()),
        Some("second goal")
    );
}

/// The session-terminal outcome releases every control slot: a steer and a
/// queued goal that outlived the session would otherwise be consumed by the
/// next one, which would start a run for a goal the user never submitted.
#[test]
fn the_terminal_outcome_releases_every_control_slot() {
    let mut app = App::new();
    app.begin_run("first");
    app.submit_pending_steer("focus on the parser");
    app.submit_pending_goal("second goal");

    app.on_live_finished(&GoalFinished {
        passed: true,
        error: None,
    });
    assert!(app.pending_steer.is_none(), "a steer outlived the session");
    assert!(
        app.pending_goal.is_none(),
        "a stale queued goal outlived the session and can start a later run"
    );
    assert_eq!(app.run_mode, RunMode::Finished);
    assert_eq!(app.run_outcome.as_deref(), Some("run finished: passed"));

    // A later session's boundary therefore has no stale goal to pick up.
    let mut later = App::new();
    later.begin_run("later session");
    later.on_live_finished(&GoalFinished {
        passed: false,
        error: Some("checks failed".into()),
    });
    later.on_live_boundary(Boundary::Started);
    assert_eq!(later.run_goal, "later session");
    assert_eq!(later.run_mode, RunMode::Failed);
}

/// The two live notifications a channel-level worker publishes are routed
/// by `LiveSession::drain` to the reducers that own them: a `Control` ack
/// is transcripted and frees its own slot, a `GoalFinished` is the
/// per-goal outcome, and the worker handle is kept until the terminal
/// `LiveEvent::Finished` arrives.
#[tokio::test]
async fn the_live_channel_routes_acks_and_goal_outcomes_to_the_app() {
    let (tx, rx) = unbounded_channel();
    let mut app = App::new();
    app.begin_run("first");
    let steer = app.submit_pending_steer("focus on the parser");
    let queued = app.submit_pending_goal("second goal");

    let mut session = LiveSession::new(rx);
    let (hold_tx, mut hold_rx) = unbounded_channel::<()>();
    let handle = tokio::spawn(async move {
        hold_rx.recv().await;
        Ok(GoalFinished {
            passed: true,
            error: None,
        })
    });
    session.begin("first", handle);

    tx.send(LiveEvent::Control(ControlAck {
        id: steer,
        kind: ControlKind::Steer,
        status: ControlStatus::Applied,
        note: "steer reaches the next boundary".into(),
    }))
    .unwrap();
    tx.send(LiveEvent::Control(ControlAck {
        id: queued,
        kind: ControlKind::Queue,
        status: ControlStatus::Applied,
        note: "queued for the next boundary".into(),
    }))
    .unwrap();
    tx.send(LiveEvent::GoalFinished(GoalFinished {
        passed: true,
        error: None,
    }))
    .unwrap();

    // The worker is still held, so the run is not over and no outcome is
    // reported for a session that has not finished.
    assert!(session.drain(&mut app).is_none());
    assert!(session.is_running());
    // The steer ack freed its slot; the applied queue ack did not, because
    // the queued goal is still owed to the next boundary.
    assert!(app.pending_steer.is_none(), "the steer ack freed nothing");
    assert_eq!(app.pending_goal.as_ref().map(|p| p.id), Some(queued));
    assert!(app
        .transcript
        .iter()
        .any(|line| line.contains("steer reaches the next boundary")));
    assert_eq!(
        app.last_control_ack.as_ref().map(|ack| ack.id),
        Some(queued),
        "the acknowledgements were not recorded in channel order"
    );
    // The per-goal outcome with a queued goal pending keeps the run live.
    assert_eq!(app.run_mode, RunMode::Running);
    assert_eq!(app.transcript.last().unwrap(), "goal finished: passed");
    assert!(!app.transcript.iter().any(|l| l.contains("run finished")));

    tx.send(LiveEvent::Finished(GoalFinished {
        passed: true,
        error: None,
    }))
    .unwrap();
    let outcome = drain_until_terminal(&mut session, &mut app).await;
    assert!(outcome.passed);
    assert_eq!(app.transcript.last().unwrap(), "run finished: passed");
    assert!(app.pending_goal.is_none(), "the session kept a stale goal");
    assert!(!session.is_running());
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

/// P1a is a read-only monitor: a running goal owns the console's attention,
/// so Enter may only record the read-only posture. Nothing is parsed,
/// dispatched, or cleared — the draft the user typed survives untouched.
#[test]
fn running_mode_does_not_dispatch_composer_text() {
    let mut app = App::new();
    app.begin_run("busy");
    app.input.push_str("/model provider/model");

    assert_eq!(
        handle_running_key(&mut app, KeyCode::Enter),
        RunningKeyOutcome::ReadOnlyNotice
    );
    assert_eq!(app.input, "/model provider/model");
    assert!(app.transcript.iter().any(|line| line.contains("read-only")));
    assert_eq!(
        app.transcript.last().map(String::as_str),
        Some("run in progress — composer is read-only in P1a")
    );
    // The notice is the only thing the key produced: no run state change.
    assert_eq!(app.run_mode, RunMode::Running);
    assert!(app.activity.is_empty());
}

#[test]
fn running_quit_keys_arm_a_stop_and_never_edit_the_draft() {
    let mut app = App::new();
    app.begin_run("busy");

    // An empty composer is the only place `q` is a quit key: goal text
    // containing `q` must stay typeable while a run is live.
    assert_eq!(
        handle_running_key(&mut app, KeyCode::Esc),
        RunningKeyOutcome::StopArmed
    );
    assert_eq!(
        handle_running_key(&mut app, KeyCode::Char('q')),
        RunningKeyOutcome::StopArmed
    );
    assert_eq!(app.run_mode, RunMode::Stopping);
    assert!(app.input.is_empty(), "the quit keys edited the draft");

    // A non-empty draft keeps `q` as ordinary composer text; the pump, not
    // this helper, appends it.
    let mut app = App::new();
    app.begin_run("busy");
    app.input.push_str("qq");
    assert_eq!(
        handle_running_key(&mut app, KeyCode::Char('q')),
        RunningKeyOutcome::Ignored
    );
    assert_eq!(app.input, "qq");
    assert_eq!(app.run_mode, RunMode::Running);
}

/// A modified `q` is text, not a stop: the pump reads the modifier
/// before the helper, and a shifted `q` is exactly what a user typing
/// Shift+q into the draft produces. Nothing about the run may change.
#[test]
fn a_shifted_q_is_ordinary_text_while_running() {
    let mut app = App::new();
    app.begin_run("busy");

    assert_eq!(
        handle_running_key(&mut app, KeyCode::Char('Q')),
        RunningKeyOutcome::Ignored
    );
    assert_eq!(app.run_mode, RunMode::Running);
    assert!(
        app.transcript.is_empty(),
        "a typed key wrote to the transcript"
    );
}

/// The second stop key is the pump's to act on: the helper is a pure
/// reducer, so a repeated `q`/Esc re-reports the same posture and
/// leaves the stop latch standing. The pump reads that latch to tell
/// "ask the worker to stop" from "detach it and exit".
#[test]
fn a_repeated_stop_key_leaves_the_latch_for_the_pump() {
    let (_tx, rx) = unbounded_channel::<LiveEvent>();
    let mut app = App::new();
    app.begin_run("busy");
    let mut session = LiveSession::new(rx);

    assert_eq!(
        handle_running_key(&mut app, KeyCode::Esc),
        RunningKeyOutcome::StopArmed
    );
    session.request_stop();
    assert_eq!(
        handle_running_key(&mut app, KeyCode::Char('q')),
        RunningKeyOutcome::StopArmed
    );
    assert!(
        session.stop_requested(),
        "the second stop key cleared the latch the pump detaches on"
    );
    assert_eq!(app.run_mode, RunMode::Stopping);
    assert!(app.input.is_empty());
}

/// The live channel is worth subscribing to only if it carries exactly what
/// the durable sink recorded: the same events, in the same order, through a
/// real orchestrator worker instead of a hand-fed `emit` loop. The
/// boundaries come from the runner around the worker, so they must bracket
/// the trace it emitted. Stub client, temp work root: no network, no
/// credentials, no wall-clock sleep.
#[tokio::test]
async fn stub_worker_delivers_the_same_trace_order_as_the_durable_sink() {
    use rof::config::AppConfig;
    use rof::engine::{Orchestrator, Session};
    use rof::llm::{ContextService, ExecutorService, StubClient};
    use rof::tools::ToolRegistry;
    use std::sync::Arc;

    let root =
        std::env::temp_dir().join(format!("rof-p1a-live-{}-stub-worker", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let trace = Arc::new(TraceSink::new());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    trace.attach_live(tx.clone());
    // A GoalRunner announces the run before the worker starts, so every
    // trace event the worker emits lands inside the boundary.
    tx.send(LiveEvent::Boundary(Boundary::Started)).unwrap();

    let worker_trace = trace.clone();
    let worker_root = root.clone();
    let handle = tokio::spawn(async move {
        let cfg = AppConfig::default();
        let context = ContextService::new(Arc::new(StubClient), "context-stub".into());
        let executor = ExecutorService::new(Arc::new(StubClient), "executor-stub".into(), None);
        let verify = ExecutorService::new(Arc::new(StubClient), "verify-stub".into(), None);
        let registry = ToolRegistry::with_defaults(
            worker_root.clone(),
            cfg.permissions.clone(),
            cfg.skills.clone(),
        );
        let orch = Orchestrator::new(cfg, worker_trace, context, executor, verify);
        orch.run_loop(
            &Session::new("stub integration goal".into()),
            &registry,
            &worker_root,
        )
        .await
    });
    handle.await.expect("the stub worker panicked");

    // The notifications a GoalRunner sends around the worker, on the same
    // channel: the sink is detached once the worker is done, then the run
    // is bracketed and reported.
    trace.detach_live();
    tx.send(LiveEvent::Boundary(Boundary::Finished)).unwrap();
    tx.send(LiveEvent::Finished(GoalFinished {
        passed: true,
        error: None,
    }))
    .unwrap();

    // Every send happened above, so draining the closed-out channel is
    // deterministic: no waiting, no sleep.
    let live: Vec<LiveEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let stored = trace.events();
    assert!(!stored.is_empty(), "the worker emitted no trace events");

    // The runner's boundary notifications share this channel with the
    // worker's trace events, so the traces are projected out and the
    // notifications are asserted separately below.
    let delivered: Vec<String> = live
        .iter()
        .filter_map(|event| match event {
            LiveEvent::Trace(event) => Some(serde_json::to_string(event).unwrap()),
            LiveEvent::Boundary(_)
            | LiveEvent::Control(_)
            | LiveEvent::GoalFinished(_)
            | LiveEvent::Finished(_) => None,
        })
        .collect();
    let durable: Vec<String> = stored
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect();
    assert_eq!(delivered, durable);
    // Started, then one trace notification per durable event, then the
    // finished boundary and the outcome: nothing else travels this channel.
    assert_eq!(live.len(), stored.len() + 3);
    // The worker's own first event lands after the run was announced.
    assert!(matches!(
        live.get(1),
        Some(LiveEvent::Trace(TraceEvent::SessionStart { .. }))
    ));

    assert!(matches!(
        live.first(),
        Some(LiveEvent::Boundary(Boundary::Started))
    ));
    assert!(matches!(
        live.get(live.len() - 2),
        Some(LiveEvent::Boundary(Boundary::Finished))
    ));
    assert!(matches!(live.last(), Some(LiveEvent::Finished(_))));
    // Nothing but trace events between the two boundaries.
    assert!(
        live[1..live.len() - 2]
            .iter()
            .all(|event| matches!(event, LiveEvent::Trace(_))),
        "unexpected notification between the boundaries: {live:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
    assert!(!root.exists(), "the temp root was left behind: {root:?}");
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
