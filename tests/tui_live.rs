//! Live run state: bounded activity, the run lifecycle, and the
//! `LiveSession` channel reducer. No terminal, no network, no model: real
//! `TraceEvent` values and real tokio channels only.

use crossterm::event::KeyCode;
use rof::engine::control::RunCommand;
use rof::obs::{
    Boundary, ControlAck, ControlKind, ControlStatus, GoalFinished, LiveEvent, TraceEvent,
    TraceSink,
};
use rof::tui::app::{control_ack_line, App, BusyMode, DeferredConfig, Focus, RunMode};
use rof::tui::cmd::{parse, Action};
use rof::tui::render::render_line;
use rof::tui::run::{
    apply_action, apply_deferred_config, handle_running_action, handle_running_key, request_stop,
    running_key_outcome, submit_running_input, LiveSession, RunningActionOutcome,
    RunningKeyOutcome, RunningSubmit,
};
use std::sync::Mutex;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

/// A live session, which owns both halves of the command channel until a
/// worker claims the receiver half.
fn live_session(rx: tokio::sync::mpsc::UnboundedReceiver<LiveEvent>) -> LiveSession {
    LiveSession::new(rx)
}

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
    let mut session = live_session(rx);
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

/// The command inbox belongs to the worker that drains it, so it dies with
/// that task. The session therefore mints a fresh pair for every later goal
/// and moves its write side with it, which is what keeps a submission from
/// being written onto a channel nobody will read.
#[tokio::test]
async fn a_later_goal_gets_a_fresh_inbox_and_the_dead_one_receives_nothing() {
    let (_tx, rx) = unbounded_channel();
    let mut session = live_session(rx);
    let first_sender = session.command_sender().clone();

    let mut first_inbox = session.take_command_inbox();
    first_sender
        .send(RunCommand::Stop { id: 1 })
        .expect("the first inbox is open while its worker runs");
    assert_eq!(first_inbox.recv().await, Some(RunCommand::Stop { id: 1 }));

    // The first goal ends, and its task drops the inbox it owned.
    drop(first_inbox);
    let mut second_inbox = session.take_command_inbox();

    // The write side moved with the pair, so this is what a submission
    // after the swap goes on. The old sender is the only thing left of
    // that channel: its inbox is gone and cannot receive anything.
    assert!(first_sender.send(RunCommand::Stop { id: 2 }).is_err());
    session
        .command_sender()
        .send(RunCommand::Stop { id: 3 })
        .expect("the session holds the write side of the new pair");
    assert_eq!(second_inbox.recv().await, Some(RunCommand::Stop { id: 3 }));
    assert!(second_inbox.try_recv().is_err());
}

/// A command the console submits while no goal is live must not be lost at
/// the swap boundary: it was written before any worker owned a receiver, so
/// the first inbox has to be the channel it was written on.
#[tokio::test]
async fn a_command_sent_before_the_first_claim_reaches_the_first_inbox() {
    let (_tx, rx) = unbounded_channel();
    let mut session = live_session(rx);
    let steer = RunCommand::Steer {
        id: 7,
        text: "keep the parser change".to_string(),
    };
    session
        .command_sender()
        .send(steer.clone())
        .expect("the session holds the write side");

    let mut inbox = session.take_command_inbox();
    assert_eq!(inbox.recv().await, Some(steer));
}

/// Every claim yields its own inbox: while an earlier one is still alive,
/// a later claim is a different channel that the session's write side
/// feeds and the earlier one never sees.
#[tokio::test]
async fn each_claim_yields_a_distinct_inbox() {
    let (_tx, rx) = unbounded_channel();
    let mut session = live_session(rx);
    let mut first_inbox = session.take_command_inbox();
    let mut second_inbox = session.take_command_inbox();

    let queued = RunCommand::QueueGoal {
        id: 4,
        goal: "write the tests".to_string(),
    };
    session
        .command_sender()
        .send(queued.clone())
        .expect("the session holds the write side");
    assert_eq!(second_inbox.recv().await, Some(queued));
    assert!(
        first_inbox.try_recv().is_err(),
        "the earlier inbox must not be fed after the swap"
    );
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

    let mut session = live_session(rx);
    let (hold_tx, mut hold_rx) = unbounded_channel::<()>();
    let handle = tokio::spawn(async move {
        hold_rx.recv().await;
        Ok(GoalFinished {
            passed: true,
            error: None,
        })
    });
    session.begin("first", handle);

    tx.send(LiveEvent::Trace(TraceEvent::Control(ControlAck {
        id: steer,
        kind: ControlKind::Steer,
        status: ControlStatus::Applied,
        note: "steer reaches the next boundary".into(),
    })))
    .unwrap();
    tx.send(LiveEvent::Trace(TraceEvent::Control(ControlAck {
        id: queued,
        kind: ControlKind::Queue,
        status: ControlStatus::Applied,
        note: "queued for the next boundary".into(),
    })))
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

/// The whole two-goal session through the real reducer, with the worker
/// handle deliberately held so every claim below is about the event
/// sequence and not about task completion: a queued goal keeps the same
/// worker present, its `Boundary::Started` consumes the pending goal and
/// opens the next run, both per-goal outcomes are recorded, and only the
/// session-terminal `Finished` produces a terminal outcome and releases
/// the handle.
#[tokio::test]
async fn a_queued_goal_stays_in_one_session_and_only_finished_is_terminal() {
    let (tx, rx) = unbounded_channel();
    let mut session = live_session(rx);
    let mut app = App::new();
    app.begin_run("first goal");
    let queued = app.submit_pending_goal("second goal");

    let (hold_tx, mut hold_rx) = unbounded_channel::<()>();
    let handle = tokio::spawn(async move {
        hold_rx.recv().await;
        Ok(GoalFinished {
            passed: true,
            error: None,
        })
    });
    session.begin("first goal", handle);

    tx.send(LiveEvent::Trace(TraceEvent::Control(ControlAck {
        id: queued,
        kind: ControlKind::Queue,
        status: ControlStatus::Applied,
        note: "retained for the next goal".into(),
    })))
    .unwrap();
    tx.send(LiveEvent::GoalFinished(GoalFinished {
        passed: false,
        error: Some("first goal failed".into()),
    }))
    .unwrap();
    // The worker's per-goal bracket. It closes one goal, not the session:
    // the App reads it as "this goal is over" and the worker handle stays.
    tx.send(LiveEvent::Boundary(Boundary::Finished)).unwrap();
    // Goal two starts in the same session: the boundary consumes the goal
    // the acknowledgement left pending.
    tx.send(LiveEvent::Boundary(Boundary::Started)).unwrap();

    assert!(
        session.drain(&mut app).is_none(),
        "a queued goal reported a terminal outcome before the session ended"
    );
    assert!(
        session.is_running(),
        "the queued goal released the worker handle"
    );
    assert_eq!(app.run_goal, "second goal");
    assert!(
        app.pending_goal.is_none(),
        "the second started boundary did not consume the queued goal"
    );
    assert_eq!(app.run_mode, RunMode::Running);
    // The first goal's outcome is recorded, and it is not the
    // session-terminal line: a queued goal keeps the run live.
    assert_eq!(
        app.transcript.last().unwrap(),
        "goal failed: first goal failed"
    );
    assert!(
        !app.transcript.iter().any(|l| l.contains("run finished")),
        "a per-goal outcome ended the session: {:?}",
        app.transcript
    );

    tx.send(LiveEvent::GoalFinished(GoalFinished {
        passed: true,
        error: None,
    }))
    .unwrap();
    tx.send(LiveEvent::Boundary(Boundary::Finished)).unwrap();
    assert!(
        session.drain(&mut app).is_none(),
        "the last goal's outcome is per-goal, not session-terminal"
    );
    // Both per-goal outcomes are recorded, in order, and neither ended the
    // session.
    assert_eq!(app.transcript.last().unwrap(), "goal finished: passed");
    assert!(
        app.transcript
            .iter()
            .any(|line| line == "goal failed: first goal failed"),
        "the first goal's outcome was lost: {:?}",
        app.transcript
    );
    assert_eq!(app.run_mode, RunMode::Finished);
    assert!(session.is_running(), "the last goal finished the session");

    tx.send(LiveEvent::Finished(GoalFinished {
        passed: true,
        error: None,
    }))
    .unwrap();
    let outcome = drain_until_terminal(&mut session, &mut app).await;
    assert!(outcome.passed);
    assert_eq!(app.transcript.last().unwrap(), "run finished: passed");
    assert!(!session.is_running(), "Finished did not release the handle");
    // Exactly one terminal outcome for the whole two-goal session.
    assert!(session.drain(&mut app).is_none());
    assert_eq!(
        app.transcript
            .iter()
            .filter(|line| line.starts_with("run finished"))
            .count(),
        1
    );
    drop(hold_tx);
}

#[tokio::test]
async fn request_stop_does_not_abort_the_worker() {
    let (_tx, rx) = unbounded_channel::<LiveEvent>();
    let mut app = App::new();
    let mut session = live_session(rx);
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
    let mut session = live_session(rx);
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
    let mut session = live_session(rx);
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

/// P1b: Enter submits. The key reducer only *reports* the submission — it
/// sends nothing, clears nothing, and starts no run — so the draft the user
/// typed survives the key and the real intent of the old P1a test holds: an
/// ordinary composer line never becomes a goal or a run on its own.
#[test]
fn running_enter_reports_a_submission_and_never_starts_a_run() {
    let mut app = App::new();
    app.begin_run("busy");
    app.input.push_str("/model provider/model");

    assert_eq!(
        handle_running_key(&mut app, KeyCode::Enter),
        RunningKeyOutcome::Submit
    );
    assert_eq!(app.input, "/model provider/model");
    // The key is a report, not a dispatch: no run state change, no
    // transcript line, and no occupied control slot.
    assert_eq!(app.run_mode, RunMode::Running);
    assert_eq!(app.run_goal, "busy");
    assert!(app.activity.is_empty());
    assert!(app.transcript.is_empty());
    assert!(app.pending_steer.is_none());
    assert!(app.pending_goal.is_none());

    // An ordinary character is draft text: it is never a submission, and it
    // never becomes a goal or a second run.
    assert_eq!(
        handle_running_key(&mut app, KeyCode::Char('x')),
        RunningKeyOutcome::Ignored
    );
    assert_eq!(app.run_goal, "busy");
    assert_eq!(app.run_mode, RunMode::Running);
    assert!(app.pending_steer.is_none());
    assert!(app.pending_goal.is_none());
}

/// A steer submission is the composer line becoming exactly one command on
/// the worker's channel, the pending slot the console shows, and a cleared
/// draft. Nothing else about the run moves.
#[test]
fn a_steer_submission_sends_the_command_and_occupies_the_slot() {
    let (tx, mut rx) = unbounded_channel::<RunCommand>();
    let mut app = App::new();
    app.begin_run("busy");
    app.set_busy_mode(BusyMode::Steer);
    app.input.push_str("  focus on the parser  ");

    let draft = app.input.clone();
    assert_eq!(
        submit_running_input(&mut app, &tx, &draft),
        RunningSubmit::Sent(ControlKind::Steer)
    );

    let id = app
        .pending_steer
        .as_ref()
        .expect("no occupied steer slot")
        .id;
    assert_eq!(
        rx.try_recv().unwrap(),
        RunCommand::Steer {
            id,
            text: "focus on the parser".to_string(),
        }
    );
    assert!(
        rx.try_recv().is_err(),
        "the submission sent more than one command"
    );
    assert_eq!(app.input, "", "a sent draft was not cleared");
    assert!(app
        .transcript
        .iter()
        .any(|line| line == &format!("steer pending ({id})")));
    assert_eq!(app.run_mode, RunMode::Running);
}

/// The same contract in queue mode, with the queue slot and its wording.
#[test]
fn a_queue_submission_sends_the_goal_and_occupies_the_slot() {
    let (tx, mut rx) = unbounded_channel::<RunCommand>();
    let mut app = App::new();
    app.begin_run("busy");
    app.set_busy_mode(BusyMode::Queue);
    app.input.push_str(" then fix the lexer ");

    let draft = app.input.clone();
    assert_eq!(
        submit_running_input(&mut app, &tx, &draft),
        RunningSubmit::Sent(ControlKind::Queue)
    );

    let id = app
        .pending_goal
        .as_ref()
        .expect("no occupied queue slot")
        .id;
    assert_eq!(
        rx.try_recv().unwrap(),
        RunCommand::QueueGoal {
            id,
            goal: "then fix the lexer".to_string(),
        }
    );
    assert!(
        rx.try_recv().is_err(),
        "the submission sent more than one command"
    );
    assert_eq!(app.input, "");
    assert!(app
        .transcript
        .iter()
        .any(|line| line == &format!("goal queued ({id})")));
    // A queued goal does not start a run here: the worker starts it at the
    // next boundary, and the pump's Enter never began a second goal.
    assert_eq!(app.run_mode, RunMode::Running);
    assert_eq!(app.run_goal, "busy");
}

/// An empty or blank line is not a submission: no id is spent, no slot is
/// occupied, and the draft the user typed stays exactly as it was.
#[test]
fn an_empty_or_blank_submission_is_ignored() {
    let (tx, mut rx) = unbounded_channel::<RunCommand>();
    let mut app = App::new();
    app.begin_run("busy");
    let before = app.next_control_id;

    assert_eq!(
        submit_running_input(&mut app, &tx, ""),
        RunningSubmit::Ignored
    );
    app.input.push_str("   \t ");
    let draft = app.input.clone();
    assert_eq!(
        submit_running_input(&mut app, &tx, &draft),
        RunningSubmit::Ignored
    );

    assert_eq!(app.input, "   \t ", "a blank submission edited the draft");
    assert_eq!(
        app.next_control_id, before,
        "a blank submission spent an id"
    );
    assert!(app.pending_steer.is_none());
    assert!(app.pending_goal.is_none());
    assert!(app.transcript.is_empty());
    assert!(rx.try_recv().is_err(), "a blank submission sent a command");
}

/// A slash line is a console command, never steer text. It is rejected with
/// a reason, the draft is kept so the pump can route it, and nothing rides
/// the command channel.
#[test]
fn a_slash_line_is_rejected_and_never_rides_the_command_channel() {
    let (tx, mut rx) = unbounded_channel::<RunCommand>();
    let mut app = App::new();
    app.begin_run("busy");
    app.input.push_str("/model provider/model");

    let draft = app.input.clone();
    let outcome = submit_running_input(&mut app, &tx, &draft);
    match outcome {
        RunningSubmit::Rejected(reason) => {
            assert!(!reason.is_empty(), "the rejection named no reason");
        }
        other => panic!("a slash line was submitted as {other:?}"),
    }
    assert_eq!(
        app.input, "/model provider/model",
        "the slash draft was cleared"
    );
    assert!(app.pending_steer.is_none());
    assert!(app.pending_goal.is_none());
    assert!(
        rx.try_recv().is_err(),
        "a slash line reached the worker as steer text"
    );
}

/// Interrupt mode is the stop path, not a text path: Enter says so, names
/// the stop keys, keeps the draft, and sends nothing.
#[test]
fn interrupt_mode_rejects_a_submission_and_names_the_stop_key() {
    let (tx, mut rx) = unbounded_channel::<RunCommand>();
    let mut app = App::new();
    app.begin_run("busy");
    app.set_busy_mode(BusyMode::Interrupt);
    app.input.push_str("stop now");

    let draft = app.input.clone();
    let outcome = submit_running_input(&mut app, &tx, &draft);
    match outcome {
        RunningSubmit::Rejected(reason) => {
            assert!(
                reason.contains('q'),
                "the rejection does not name the stop key: {reason}"
            );
        }
        other => panic!("interrupt mode submitted as {other:?}"),
    }
    assert_eq!(
        app.input, "stop now",
        "a rejected interrupt submission cleared the draft"
    );
    assert!(app.pending_steer.is_none());
    assert!(app.pending_goal.is_none());
    assert!(rx.try_recv().is_err(), "interrupt mode sent a text command");
    assert_eq!(app.run_mode, RunMode::Running);
}

/// A channel with no worker behind it fails the send. Nothing reached the
/// worker, so nothing will ever be acknowledged: the slot must be free again
/// or the console shows a pending steer for the rest of the run, and the
/// draft must survive so the submission can be retried.
#[test]
fn a_failed_send_frees_the_slot_and_keeps_the_draft() {
    let (tx, rx) = unbounded_channel::<RunCommand>();
    drop(rx);
    let mut app = App::new();
    app.begin_run("busy");
    app.set_busy_mode(BusyMode::Steer);
    app.input.push_str("focus on the parser");

    let draft = app.input.clone();
    let outcome = submit_running_input(&mut app, &tx, &draft);
    match outcome {
        RunningSubmit::Rejected(reason) => {
            assert!(!reason.is_empty(), "the failed send named no error");
        }
        other => panic!("a dropped receiver still reported {other:?}"),
    }
    assert!(
        app.pending_steer.is_none(),
        "a command that never reached the worker is left pending forever"
    );
    assert_eq!(
        app.input, "focus on the parser",
        "a failed send cleared the draft"
    );
    assert!(
        app.transcript.is_empty(),
        "a failed send announced a submission"
    );
}

/// One slot per kind: a second submission displaces the first under a new
/// id, and the displaced id is no longer pending. Both commands are still on
/// the channel, so the worker can answer the older one.
#[test]
fn a_second_submission_replaces_the_first_slot_under_a_new_id() {
    let (tx, mut rx) = unbounded_channel::<RunCommand>();
    let mut app = App::new();
    app.begin_run("busy");
    app.set_busy_mode(BusyMode::Steer);

    app.input.push_str("focus on the parser");
    let first_draft = app.input.clone();
    assert_eq!(
        submit_running_input(&mut app, &tx, &first_draft),
        RunningSubmit::Sent(ControlKind::Steer)
    );
    let first_id = app.pending_steer.as_ref().unwrap().id;

    app.input.push_str("focus on the lexer");
    let second_draft = app.input.clone();
    assert_eq!(
        submit_running_input(&mut app, &tx, &second_draft),
        RunningSubmit::Sent(ControlKind::Steer)
    );

    let second = app.pending_steer.as_ref().unwrap();
    assert_ne!(second.id, first_id, "the displaced submission kept its id");
    assert_eq!(second.text, "focus on the lexer");
    // The first submission is no longer pending, so the status row cannot
    // claim a steer the console is not waiting for.
    assert!(!app.control_summary().contains(&format!("({})", first_id)));
    assert!(app.control_summary().contains(&format!("({})", second.id)));
    assert_eq!(
        rx.try_recv().unwrap(),
        RunCommand::Steer {
            id: first_id,
            text: "focus on the parser".to_string()
        }
    );
    assert_eq!(
        rx.try_recv().unwrap(),
        RunCommand::Steer {
            id: second.id,
            text: "focus on the lexer".to_string()
        }
    );
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
    let mut session = live_session(rx);

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
            LiveEvent::Boundary(_) | LiveEvent::GoalFinished(_) | LiveEvent::Finished(_) => None,
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

// ---------------------------------------------------------------------------
// P1b Task 4b: routing a console action while a goal is live.
// ---------------------------------------------------------------------------

/// Env-touching tests in this file share one process, so they take this
/// lock: two of them setting the same knob concurrently would make each
/// other's env assertion a race.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Save the named env vars, clear them for the test, and put them back
/// when it ends — a knob set here must not leak into the next test.
struct EnvRestore(Vec<(&'static str, Option<String>)>);

impl EnvRestore {
    fn new(keys: &[&'static str]) -> Self {
        let saved: Vec<(&'static str, Option<String>)> = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();
        for key in keys {
            std::env::remove_var(key);
        }
        Self(saved)
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// The running-path dispatch exactly as the pump's Enter branch calls it:
/// one real command channel, one real sink, no terminal.
fn running_action(
    app: &mut App,
    action: Action,
    commands: &UnboundedSender<RunCommand>,
    raw: &str,
) -> RunningActionOutcome {
    let trace = TraceSink::new();
    let mut awaiting_key: Option<String> = None;
    handle_running_action(app, action, commands, raw, &trace, &mut awaiting_key)
}

/// A live goal and an empty command channel: the state every routing test
/// starts from, so "nothing was sent" is a claim about the action alone.
fn live_goal() -> (
    App,
    UnboundedSender<RunCommand>,
    tokio::sync::mpsc::UnboundedReceiver<RunCommand>,
) {
    let (tx, rx) = unbounded_channel::<RunCommand>();
    let mut app = App::new();
    app.begin_run("busy");
    (app, tx, rx)
}

/// A knob typed while a goal is live is not applied to the running goal
/// and is not a command: it is held for the next one, written to the env
/// at submission time, and reported as deferred. The env write is the
/// point — a queued goal is started by the worker, so the pump can never
/// run just before it and the write cannot wait for one.
#[test]
fn a_knob_while_running_is_deferred_and_written_to_the_env_now() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvRestore::new(&["ROF_ATTEMPTS"]);
    let (mut app, tx, mut rx) = live_goal();
    app.input.push_str("/attempts 2");

    let draft = app.input.clone();
    assert_eq!(
        running_action(&mut app, Action::Attempts(2), &tx, &draft),
        RunningActionOutcome::Deferred
    );

    assert_eq!(app.deferred_config, vec![DeferredConfig::Attempts(2)]);
    assert_eq!(
        std::env::var("ROF_ATTEMPTS").as_deref(),
        Ok("2"),
        "a deferred knob was not written to the env at submission time"
    );
    assert_eq!(
        app.transcript.last().map(String::as_str),
        Some("attempts=2 (ROF_ATTEMPTS, applies to the next goal)")
    );
    // A setting is not a command and occupies no control slot: the run in
    // flight is untouched and the worker sees nothing new.
    assert!(
        rx.try_recv().is_err(),
        "a knob reached the worker as a command"
    );
    assert!(app.pending_goal.is_none());
    assert!(app.pending_steer.is_none());
    assert_eq!(app.run_mode, RunMode::Running);
    assert!(app.input.is_empty(), "a handled line kept the draft");
}

/// Deferring is a record, not a slot: two different settings both stay, in
/// submission order, so a later knob can never displace an earlier one.
#[test]
fn a_second_deferred_setting_keeps_the_first_in_submission_order() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvRestore::new(&["ROF_ATTEMPTS", "ROF_MAX_ROUNDS"]);
    let (mut app, tx, _rx) = live_goal();

    for (action, raw) in [
        (Action::Attempts(2), "/attempts 2"),
        (Action::Rounds(4), "/rounds 4"),
    ] {
        assert_eq!(
            running_action(&mut app, action, &tx, raw),
            RunningActionOutcome::Deferred
        );
    }

    assert_eq!(
        app.deferred_config,
        vec![DeferredConfig::Attempts(2), DeferredConfig::Rounds(4)]
    );
    assert_eq!(std::env::var("ROF_ATTEMPTS").as_deref(), Ok("2"));
    assert_eq!(std::env::var("ROF_MAX_ROUNDS").as_deref(), Ok("4"));
}

/// Credential actions are the one thing a live goal must not absorb:
/// they are refused outright, with no credential read, no env write, and
/// no store call. Nothing in the refusal may echo a secret either.
#[test]
fn credential_actions_are_refused_while_a_goal_is_live() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvRestore::new(&["OR_TOKEN", "ROF_ATTEMPTS", "ROF_CREDENTIALS"]);
    std::env::set_var("OR_TOKEN", "env-token-must-survive");
    let creds = std::env::temp_dir().join(format!("rof-p1b-refused-creds-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&creds);
    std::fs::create_dir_all(&creds).unwrap();
    std::env::set_var("ROF_CREDENTIALS", creds.join("credentials.json"));
    let (mut app, tx, mut rx) = live_goal();

    for (action, raw) in [
        (
            Action::Login(Some("acme".into())),
            "/login acme super-secret",
        ),
        (Action::ProviderRm("acme".into()), "/provider rm acme"),
    ] {
        app.input.push_str(raw);
        let draft = app.input.clone();
        match running_action(&mut app, action, &tx, &draft) {
            RunningActionOutcome::Rejected(reason) => assert!(
                reason.contains("available between goals"),
                "the refusal does not say when it is available: {reason}"
            ),
            other => panic!("a credential action was routed as {other:?}"),
        }
        assert_eq!(app.input, raw, "a refused line cleared the draft");
        app.input.clear();
    }

    assert_eq!(
        std::env::var("OR_TOKEN").as_deref(),
        Ok("env-token-must-survive"),
        "a refused credential action wrote the env"
    );
    assert!(
        std::env::var("ROF_ATTEMPTS").is_err(),
        "a refused credential action deferred a setting"
    );
    assert!(app.deferred_config.is_empty());
    assert!(rx.try_recv().is_err(), "a credential action sent a command");
    assert!(
        !creds.join("credentials.json").exists(),
        "a refused credential action touched the credentials store"
    );
    let _ = std::fs::remove_dir_all(&creds);
}

/// `/busy queue` is a posture change and nothing more: the mode is set so
/// the next submission stores a goal, but no command is sent now and the
/// draft is consumed.
#[test]
fn queue_mode_while_running_sends_nothing() {
    let (mut app, tx, mut rx) = live_goal();
    app.input.push_str("/busy queue");

    let draft = app.input.clone();
    assert_eq!(
        running_action(&mut app, Action::Busy("queue".into()), &tx, &draft),
        RunningActionOutcome::BusyMode(BusyMode::Queue)
    );

    assert_eq!(app.busy_mode, BusyMode::Queue);
    assert!(rx.try_recv().is_err(), "a busy mode sent a command");
    assert_eq!(app.run_mode, RunMode::Running);
    assert!(app.input.is_empty());
}

/// `/busy interrupt` asks for the stop path, so it is routed as a stop
/// request and not as anything this function sends: the pump's shared
/// stop path owns that send, and two sends would not agree.
#[test]
fn interrupt_mode_while_running_arms_the_stop_and_sends_nothing() {
    let (mut app, tx, mut rx) = live_goal();
    app.input.push_str("/busy interrupt");

    let draft = app.input.clone();
    assert_eq!(
        running_action(&mut app, Action::Busy("interrupt".into()), &tx, &draft),
        RunningActionOutcome::Stop
    );

    assert_eq!(app.busy_mode, BusyMode::Interrupt);
    assert!(
        rx.try_recv().is_err(),
        "interrupt mode sent a command of its own"
    );
    assert!(app.input.is_empty());
}

/// A read-only action still works while a goal is live: it renders into
/// the transcript, changes no run state, and sends nothing.
#[test]
fn a_view_action_while_running_still_renders_its_own_line() {
    let (mut app, tx, mut rx) = live_goal();
    app.input.push_str("/context");

    let draft = app.input.clone();
    assert_eq!(
        running_action(&mut app, Action::Context, &tx, &draft),
        RunningActionOutcome::View
    );

    assert!(
        app.transcript
            .iter()
            .any(|line| line.starts_with("context: ")),
        "a view action rendered no line: {:?}",
        app.transcript
    );
    assert!(rx.try_recv().is_err(), "a view action sent a command");
    assert_eq!(app.run_mode, RunMode::Running);
    assert!(app.input.is_empty());
    assert!(app.deferred_config.is_empty());
}

/// The env write happens when the setting is submitted, so a setting typed
/// while a goal is ALREADY QUEUED is picked up by that very goal — the
/// worker rebuilds config when it starts it. Claiming the setting would land
/// "after queued goal" would be a lie, and would hide a live knob change.
#[test]
fn a_setting_typed_while_a_goal_is_queued_applies_to_that_goal() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvRestore::new(&["ROF_ATTEMPTS"]);
    let (mut app, tx, _rx) = live_goal();
    app.submit_pending_goal("then fix the lexer");

    let draft = "/attempts 2".to_string();
    assert_eq!(
        running_action(&mut app, Action::Attempts(2), &tx, &draft),
        RunningActionOutcome::Deferred
    );

    // The value is already in the env, which is what the queued goal reads
    // when the worker starts it.
    assert_eq!(std::env::var("ROF_ATTEMPTS").as_deref(), Ok("2"));
    assert_eq!(
        app.transcript.last().map(String::as_str),
        Some("attempts=2 (ROF_ATTEMPTS, applies to the next goal)"),
        "the wording must not promise to wait past a queued goal"
    );
    assert_eq!(app.deferred_config, vec![DeferredConfig::Attempts(2)]);
}

/// The draft is the user's: a handled line is spent, a refused one stays
/// so it can be corrected. This is what makes a refusal recoverable
/// without retyping the line.
#[test]
fn a_refused_line_keeps_its_draft_while_a_handled_one_is_cleared() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvRestore::new(&["ROF_ATTEMPTS", "OR_TOKEN"]);
    let (mut app, tx, _rx) = live_goal();

    app.input.push_str("/logout openrouter");
    let refused = app.input.clone();
    assert!(matches!(
        running_action(&mut app, Action::Logout("openrouter".into()), &tx, &refused),
        RunningActionOutcome::Rejected(_)
    ));
    assert_eq!(app.input, "/logout openrouter");

    app.input.clear();
    app.input.push_str("/attempts 2");
    let deferred = app.input.clone();
    assert_eq!(
        running_action(&mut app, Action::Attempts(2), &tx, &deferred),
        RunningActionOutcome::Deferred
    );
    assert!(app.input.is_empty());

    app.input.push_str("/context");
    let viewed = app.input.clone();
    assert_eq!(
        running_action(&mut app, Action::Context, &tx, &viewed),
        RunningActionOutcome::View
    );
    assert!(app.input.is_empty());
}

/// The deferred list is a record of what is waiting, so it ends with the
/// goal it was waiting for: a fresh run must not inherit settings that
/// were already consumed.
#[test]
fn begin_run_clears_the_deferred_settings() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvRestore::new(&["ROF_ATTEMPTS"]);
    let (mut app, tx, _rx) = live_goal();

    let draft = "/attempts 2".to_string();
    running_action(&mut app, Action::Attempts(2), &tx, &draft);
    assert_eq!(app.deferred_config.len(), 1);
    assert!(app.control_summary().contains("deferred"));

    app.begin_run("the next goal");
    assert!(
        app.deferred_config.is_empty(),
        "deferred settings outlived the goal they were waiting for"
    );
    assert!(!app.control_summary().contains("deferred"));
    // The env write is not undone: the goal that starts now reads it.
    assert_eq!(std::env::var("ROF_ATTEMPTS").as_deref(), Ok("2"));
}

/// One env mapping, two callers: a setting applied between goals and the
/// same setting applied while one is live must leave the same env behind
/// and say the same thing, or the two paths have drifted.
#[test]
fn the_shared_env_helper_agrees_from_both_paths() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvRestore::new(&["ROF_ATTEMPTS", "ROF_MAX_ROUNDS", "OR_TOKEN"]);

    // Between goals: the between-goals dispatcher, which must reach the
    // same env mapping through the same helper the running path uses.
    let trace = TraceSink::new();
    let mut idle = App::new();
    let mut awaiting_key: Option<String> = None;
    let raw = "/attempts 2".to_string();
    let quit = apply_action(
        &mut idle,
        &trace,
        Action::Attempts(2),
        &raw,
        &mut awaiting_key,
    );
    assert!(!quit, "a knob ended the console");
    let idle_wording = idle.transcript.last().cloned().expect("no wording");

    // While live: the same setting through the running path.
    let (mut app, tx, _rx) = live_goal();
    let draft = raw.clone();
    assert_eq!(
        running_action(&mut app, Action::Attempts(2), &tx, &draft),
        RunningActionOutcome::Deferred
    );
    let running_wording = app.transcript.last().cloned().expect("no wording");

    assert_eq!(std::env::var("ROF_ATTEMPTS").as_deref(), Ok("2"));
    assert_eq!(
        running_wording, idle_wording,
        "the two paths worded the same setting differently"
    );

    // And the helper itself, called directly, is the single env mapping.
    let direct = apply_deferred_config(&DeferredConfig::Rounds(4));
    assert_eq!(std::env::var("ROF_MAX_ROUNDS").as_deref(), Ok("4"));
    assert!(direct.contains("ROF_MAX_ROUNDS"), "{direct}");
}

/// `/display` only draws a line, so it is a view action while live.
/// `/quit` is the one that is refused: a single keystroke must not end the
/// console and abandon a worker that is still writing its trace, and the
/// draft stays so the line can be corrected.
#[test]
fn display_is_a_view_action_and_quit_is_refused_while_running() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvRestore::new(&["OR_TOKEN", "ROF_ATTEMPTS"]);
    std::env::set_var("OR_TOKEN", "env-token-must-survive");
    let (mut app, tx, mut rx) = live_goal();

    app.input.push_str("/display fullscreen");
    let viewed = app.input.clone();
    assert_eq!(
        running_action(&mut app, Action::Display("fullscreen".into()), &tx, &viewed),
        RunningActionOutcome::View
    );
    assert!(
        app.transcript.iter().any(|l| l.contains("fullscreen")),
        "a display action rendered no line: {:?}",
        app.transcript
    );
    assert_eq!(
        std::env::var("OR_TOKEN").as_deref(),
        Ok("env-token-must-survive")
    );

    app.input.clear();
    app.input.push_str("/quit");
    let refused = app.input.clone();
    match running_action(&mut app, Action::Quit, &tx, &refused) {
        RunningActionOutcome::Rejected(reason) => {
            assert!(reason.contains("between goals"), "{reason}");
            for key in ["q", "Esc", "Ctrl-C"] {
                assert!(
                    reason.contains(key),
                    "the refusal does not name {key}: {reason}"
                );
            }
        }
        other => panic!("/quit was routed as {other:?}"),
    }
    assert_eq!(app.input, "/quit", "a refused /quit cleared the draft");
    assert!(rx.try_recv().is_err(), "/quit sent a command");
    assert!(app.pending_goal.is_none() && app.pending_steer.is_none());
    assert_eq!(app.run_mode, RunMode::Running, "/quit moved the run state");
    assert_eq!(
        std::env::var("OR_TOKEN").as_deref(),
        Ok("env-token-must-survive")
    );
    assert!(app.deferred_config.is_empty());
}

/// The pump's Enter branch decides between an action and a submission by
/// parsing the draft, and the two go to different reducers: a slash line
/// never rides the command channel as steer text, and goal text never
/// becomes a slash action.
#[test]
fn the_enter_route_splits_on_the_parse_of_the_draft() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvRestore::new(&["ROF_ATTEMPTS"]);
    let (mut app, tx, mut rx) = live_goal();

    app.input.push_str("/attempts 2");
    let slash = app.input.clone();
    let action = parse(&slash).expect("a slash line did not parse");
    assert_eq!(
        running_action(&mut app, action, &tx, &slash),
        RunningActionOutcome::Deferred
    );
    assert!(
        rx.try_recv().is_err(),
        "an action route put something on the command channel"
    );

    app.input.push_str("focus on the parser");
    let goal = app.input.clone();
    assert!(parse(&goal).is_none(), "goal text parsed as a command");
    assert_eq!(
        submit_running_input(&mut app, &tx, &goal),
        RunningSubmit::Sent(ControlKind::Steer)
    );
    assert!(matches!(rx.try_recv(), Ok(RunCommand::Steer { .. })));
}

/// The modifier decision belongs to the same helper that reads the key, so
/// a MODIFIED key can never reach the reducer's stop arms. Those arms set
/// the stopping posture as a side effect, and a stopping run refuses to
/// start a queued goal — so Shift+q silently dropping a queued goal is the
/// exact regression this pins.
#[test]
fn only_an_unmodified_stop_key_reaches_the_stop_arms() {
    use crossterm::event::KeyModifiers;

    let mut app = App::new();
    app.begin_run("busy");
    app.submit_pending_goal("then fix the lexer");

    // Ctrl-C is the one modified key that IS a stop key.
    assert_eq!(
        running_key_outcome(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL),
        RunningKeyOutcome::StopArmed
    );

    // Every other modified key is ordinary input and must not move the
    // posture, or the queued goal above would be dropped by the guard that
    // refuses to reopen a stopping run.
    for (code, modifiers) in [
        (KeyCode::Char('q'), KeyModifiers::SHIFT),
        (KeyCode::Char('q'), KeyModifiers::ALT),
        (KeyCode::Char('Q'), KeyModifiers::SHIFT),
        (KeyCode::Esc, KeyModifiers::CONTROL),
        (KeyCode::Esc, KeyModifiers::ALT),
    ] {
        assert_eq!(
            running_key_outcome(&mut app, code, modifiers),
            RunningKeyOutcome::Ignored,
            "{code:?} with {modifiers:?} must not be a stop key"
        );
    }
    assert_eq!(
        app.run_mode,
        RunMode::Running,
        "a modified key moved the posture"
    );
    assert!(app.pending_goal.is_some(), "the queued goal survived");

    // The unmodified stop keys still work, and `q` only when the draft is
    // empty so goal text containing `q` stays typeable.
    assert_eq!(
        running_key_outcome(&mut app, KeyCode::Char('q'), KeyModifiers::NONE),
        RunningKeyOutcome::StopArmed
    );
    let mut typed = App::new();
    typed.begin_run("busy");
    typed.input.push('q');
    assert_eq!(
        running_key_outcome(&mut typed, KeyCode::Char('q'), KeyModifiers::NONE),
        RunningKeyOutcome::Ignored
    );
    assert_eq!(
        running_key_outcome(&mut app, KeyCode::Enter, KeyModifiers::NONE),
        RunningKeyOutcome::Submit
    );
}

/// A stop is requested once. `/busy interrupt` after a stop key must not
/// send a second command, repeat the hint, or be read as the second press
/// that detaches and exits — only a key press does that.
#[test]
fn a_stop_is_requested_once_however_many_times_it_is_asked_for() {
    let (command_tx, mut command_rx) = unbounded_channel::<RunCommand>();
    // The sender is held (not dropped) for the session's lifetime, exactly
    // as the pump does, so the receiver is never spuriously closed.
    let (_event_tx, event_rx) = unbounded_channel::<LiveEvent>();
    let mut session = live_session(event_rx);
    let mut app = App::new();
    app.begin_run("busy");

    request_stop(&mut app, &mut session, &command_tx);
    assert!(matches!(command_rx.try_recv(), Ok(RunCommand::Stop { .. })));
    assert!(session.stop_requested());
    assert_eq!(app.run_mode, RunMode::Stopping);
    let hints = app
        .transcript
        .iter()
        .filter(|l| l.contains("stop requested"))
        .count();
    assert_eq!(hints, 1);

    // A second request from the command path changes no state and sends
    // nothing; it only says the stop already stands.
    request_stop(&mut app, &mut session, &command_tx);
    assert!(command_rx.try_recv().is_err(), "a second Stop was sent");
    let hints = app
        .transcript
        .iter()
        .filter(|l| l.contains("stop requested"))
        .count();
    assert_eq!(hints, 1, "the stop hint was repeated");
    assert!(session.stop_requested());
}

/// A mode change is narrated on the live path too. Silently switching to
/// `queue` would turn the user's next Enter into a queued goal they never
/// asked for.
#[test]
fn a_busy_mode_change_is_narrated_while_a_goal_runs() {
    let (mut app, tx, _rx) = live_goal();

    assert_eq!(
        running_action(&mut app, parse("/busy queue").unwrap(), &tx, "/busy queue"),
        RunningActionOutcome::BusyMode(BusyMode::Queue)
    );
    assert_eq!(app.busy_mode, BusyMode::Queue);
    assert!(
        app.transcript
            .iter()
            .any(|line| line.contains("busy=queue")),
        "the mode change was silent: {:?}",
        app.transcript
    );
}

/// The drift guard between the live view and a replayed one: an
/// acknowledgement is formatted in exactly one place, so the line the live
/// reducer pushes and the line `render_line` produces for the same recorded
/// event are byte-identical — in BOTH branches, the noted one and the
/// empty-note one. Two copies of this format would be how a recorded
/// session ends up reading differently from the run that produced it.
#[test]
fn the_live_and_replayed_ack_lines_are_byte_identical() {
    let acks = vec![
        ControlAck {
            id: 1,
            kind: ControlKind::Steer,
            status: ControlStatus::Applied,
            note: "applies to the next implementer prompt".into(),
        },
        // The empty-note branch: an answer with nothing to add must render
        // the same on both paths, with no trailing separator.
        ControlAck {
            id: 2,
            kind: ControlKind::Queue,
            status: ControlStatus::Rejected,
            note: String::new(),
        },
        ControlAck {
            id: 3,
            kind: ControlKind::Stop,
            status: ControlStatus::Applied,
            note: "accepted: the run stops at this boundary".into(),
        },
    ];
    for ack in acks {
        let mut app = App::new();
        app.begin_run("busy");
        app.on_control_ack(ack.clone());
        let live_line = app
            .transcript
            .last()
            .expect("the ack wrote no line")
            .clone();

        let replayed_line = render_line(&TraceEvent::Control(ack.clone()));
        assert_eq!(
            live_line, replayed_line,
            "live and replayed wording diverged for {ack:?}"
        );
        assert_eq!(live_line, control_ack_line(&ack));
        // One acknowledgement is one line: the live path adds no activity
        // line for it either, so nothing about it is doubled.
        assert_eq!(app.transcript.len(), 1, "{:?}", app.transcript);
        assert!(app.activity_tail(10).is_empty());
    }
}

/// Exactly once in a live session: the sink is the one emission seam, so an
/// acknowledgement it emits reaches an attached console once — not dropped
/// (the record is durable, the live forward is a copy) and not doubled (one
/// emit is one delivery). The App's own transcript is the witness.
#[tokio::test]
async fn one_emitted_ack_reaches_the_live_console_exactly_once() {
    let trace = TraceSink::new();
    let (tx, rx) = unbounded_channel();
    trace.attach_live(tx);

    let mut app = App::new();
    app.begin_run("busy");
    let steer = app.submit_pending_steer("focus on the parser");
    let mut session = live_session(rx);
    let (hold_tx, mut hold_rx) = unbounded_channel::<()>();
    let handle = tokio::spawn(async move {
        hold_rx.recv().await;
        Ok(GoalFinished {
            passed: true,
            error: None,
        })
    });
    session.begin("busy", handle);

    let ack = ControlAck {
        id: steer,
        kind: ControlKind::Steer,
        status: ControlStatus::Applied,
        note: "applies to the next implementer prompt".into(),
    };
    trace.emit(TraceEvent::Control(ack.clone()));

    // Draining with nothing more to come is deterministic: the emit above
    // already happened, so no yield or sleep is needed to see the event.
    assert!(session.drain(&mut app).is_none());
    let line = control_ack_line(&ack);
    assert_eq!(
        app.transcript.iter().filter(|seen| *seen == &line).count(),
        1,
        "the acknowledgement was dropped or doubled: {:?}",
        app.transcript
    );
    assert_eq!(app.transcript.len(), 1, "{:?}", app.transcript);
    // The reducer really ran on it: the slot it names is free and the last
    // answer is recorded, so the line is a transcript of the ack rather
    // than a coincidental match.
    assert!(app.pending_steer.is_none());
    assert_eq!(app.last_control_ack.as_ref(), Some(&ack));

    // And the durable side holds exactly one copy of it too.
    assert_eq!(
        trace
            .events()
            .iter()
            .filter(|event| matches!(event, TraceEvent::Control(_)))
            .count(),
        1
    );
    drop(hold_tx);
}

// ---------------------------------------------------------------------------
// P3 Task B: `/providers` as a display command over the BYOK substrate.
// ---------------------------------------------------------------------------

/// A scratch credentials + providers pair for one `/providers` test, both
/// pointed at the override paths the auth substrate already honours, and
/// removed afterwards. No home directory, no network, no real store.
///
/// The env lock and the saved values are held for the WHOLE test, not just
/// the setup: env is process-global, so a helper that released the lock on
/// return would let another test's override race this one's assertions.
struct ProvidersEnv {
    dir: std::path::PathBuf,
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<String>)>,
}

impl ProvidersEnv {
    const VARS: [&'static str; 6] = [
        "ROF_CREDENTIALS",
        "ROF_PROVIDERS",
        "OR_TOKEN",
        "ROF_TOKEN",
        "ROF_CHAT_BASE",
        "ROF_ATTEMPTS",
    ];

    fn new(tag: &str) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(&'static str, Option<String>)> = Self::VARS
            .iter()
            .map(|var| (*var, std::env::var(var).ok()))
            .collect();
        for var in Self::VARS {
            std::env::remove_var(var);
        }
        let dir = std::env::temp_dir().join(format!(
            "rof-providers-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("ROF_CREDENTIALS", dir.join("credentials.json"));
        std::env::set_var("ROF_PROVIDERS", dir.join("providers.json"));
        Self {
            dir,
            _lock: lock,
            saved,
        }
    }
}

impl Drop for ProvidersEnv {
    fn drop(&mut self) {
        for (var, value) in &self.saved {
            match value {
                Some(value) => std::env::set_var(var, value),
                None => std::env::remove_var(var),
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Run `/providers` between goals and hand back the rendered transcript.
fn render_providers() -> Vec<String> {
    let trace = TraceSink::new();
    let mut app = App::new();
    let mut awaiting_key: Option<String> = None;
    let quit = apply_action(
        &mut app,
        &trace,
        Action::Providers,
        "/providers",
        &mut awaiting_key,
    );
    assert!(!quit, "/providers ended the console");
    app.transcript
}

/// No key material, by construction: the fixture key is a value that
/// cannot be spelled by accident, and the assertions below scan every
/// rendered line for it and for each part of it.
const FIXTURE_KEY: &str = "sk-rofdanger-0f1e2d3c-THIS-MUST-NEVER-BE-PRINTED";

/// Assert that nothing in the transcript is the key or any part of it. A
/// row that reported presence by formatting `key_for`'s value would fail
/// here, and so would one that leaked a fragment (prefix, suffix, or the
/// distinctive middle).
fn assert_no_key_material(lines: &[String], key: &str) {
    for line in lines {
        assert!(
            !line.contains(key),
            "the key reached the transcript: {line}"
        );
        for part in [
            "rofdanger",
            "0f1e2d3c",
            "THIS-MUST-NEVER-BE-PRINTED",
            "sk-rof",
        ] {
            assert!(
                !line.contains(part),
                "key material ({part}) reached the transcript: {line}"
            );
        }
    }
}

/// An empty store still lists the three built-ins as key-absent, adds the
/// plain store-is-empty line, and emits no registry rows. A regression that
/// hid the built-ins fails here, because the names are asserted, not a
/// line count.
#[test]
fn providers_with_an_empty_store_lists_the_built_ins_and_says_the_store_is_empty() {
    let _env = ProvidersEnv::new("empty");

    let lines = render_providers();

    for builtin in ["openrouter", "go", "atria"] {
        let row = lines
            .iter()
            .find(|l| l.starts_with(&format!("provider {builtin} ")))
            .unwrap_or_else(|| panic!("the built-in {builtin} was not listed: {lines:?}"));
        assert!(
            row.contains("key absent"),
            "{builtin} has no key but the row does not say so: {row}"
        );
        assert!(
            !row.contains("key present"),
            "{builtin} has no key but the row claims one: {row}"
        );
    }
    assert!(
        lines
            .iter()
            .any(|l| l.contains("no provider logins in the credentials store")),
        "an empty store was not reported: {lines:?}"
    );
    // Nothing is defined, so there is no registry row to name a custom
    // base: this line proves the empty case is stated, not merely reached.
    assert!(
        !lines.iter().any(|l| l.contains("[custom,")),
        "a provider was invented from an empty registry: {lines:?}"
    );
    assert_no_key_material(&lines, FIXTURE_KEY);
}

/// A defined provider with no key is named, and its row says the key is
/// absent. The base comes from the registry; nothing here is a secret.
#[test]
fn providers_names_a_registry_provider_whose_key_is_absent() {
    let _env = ProvidersEnv::new("nokey");
    rof::tui::auth::save_provider("acme", "https://llm.acme.test/v1").unwrap();
    assert!(
        rof::tui::auth::key_for("acme").is_none(),
        "the fixture must have no key for this test to mean anything"
    );

    let lines = render_providers();

    let row = lines
        .iter()
        .find(|l| l.starts_with("provider acme "))
        .unwrap_or_else(|| panic!("a defined provider was not listed: {lines:?}"));
    assert!(row.contains("acme"), "{row}");
    assert!(row.contains("https://llm.acme.test/v1"), "{row}");
    assert!(row.contains("key absent"), "{row}");
    // Defining a provider is not logging in: the store can still be empty
    // here, and saying so is the `Models` arm's behaviour, not a bug.
    assert_no_key_material(&lines, FIXTURE_KEY);
}

/// The safety boundary, stated as a test: a provider with a real key in
/// the real store renders as PRESENT, and the literal key value appears
/// nowhere in the transcript.
#[test]
fn providers_reports_a_present_key_without_ever_printing_it() {
    let _env = ProvidersEnv::new("present");
    rof::tui::auth::save_provider("acme", "https://llm.acme.test/v1").unwrap();
    rof::tui::auth::store().save("acme", FIXTURE_KEY).unwrap();
    assert_eq!(
        rof::tui::auth::key_for("acme").as_deref(),
        Some(FIXTURE_KEY),
        "the fixture key did not reach the store"
    );

    let lines = render_providers();

    let row = lines
        .iter()
        .find(|l| l.starts_with("provider acme "))
        .unwrap_or_else(|| panic!("a logged-in provider was not listed: {lines:?}"));
    assert!(row.contains("key present"), "{row}");
    assert!(
        !row.contains("no provider logins"),
        "a store with a login claimed to be empty: {lines:?}"
    );
    // Every rendered line, not just the one row: the leak this forbids
    // could be in a summary or a note line.
    for line in &lines {
        assert!(!line.contains(FIXTURE_KEY), "leaked: {line}");
    }
    assert_no_key_material(&lines, FIXTURE_KEY);
}

/// The negative form of the same boundary: this test fails the moment a
/// row is built by formatting `key_for`'s value, which is the only way the
/// substrate hands key text to a caller.
#[test]
fn providers_never_uses_the_key_value_to_report_presence() {
    let _env = ProvidersEnv::new("nokeyvalue");
    rof::tui::auth::save_provider("acme", "https://llm.acme.test/v1").unwrap();
    rof::tui::auth::store().save("acme", FIXTURE_KEY).unwrap();

    let lines = render_providers();
    let row = lines
        .iter()
        .find(|l| l.starts_with("provider acme "))
        .expect("no row");

    // Presence is a word, and it is chosen from the same two states the
    // store can be in — the value itself is never a substring of the row
    // in any form, including a prefix of four characters.
    assert!(row.contains("key present"), "{row}");
    for n in 4..FIXTURE_KEY.len() {
        assert!(
            !row.contains(&FIXTURE_KEY[..n]),
            "the row carries the first {n} characters of the key: {row}"
        );
    }
}

/// A read-only listing while a goal is live is a view action: it renders,
/// sends nothing, and touches neither the env nor a pending slot.
#[test]
fn providers_is_a_view_action_while_a_goal_is_live() {
    let _env = ProvidersEnv::new("live");
    rof::tui::auth::save_provider("acme", "https://llm.acme.test/v1").unwrap();
    rof::tui::auth::store().save("acme", FIXTURE_KEY).unwrap();
    let (mut app, tx, mut rx) = live_goal();
    app.input.push_str("/providers");

    let draft = app.input.clone();
    assert_eq!(
        running_action(&mut app, Action::Providers, &tx, &draft),
        RunningActionOutcome::View
    );

    assert!(
        app.transcript
            .iter()
            .any(|l| l.starts_with("provider acme ")),
        "/providers rendered nothing while live: {:?}",
        app.transcript
    );
    assert!(rx.try_recv().is_err(), "/providers sent a command");
    assert!(app.pending_goal.is_none() && app.pending_steer.is_none());
    assert!(app.deferred_config.is_empty());
    assert_eq!(
        app.run_mode,
        RunMode::Running,
        "/providers moved the run state"
    );
    assert!(app.input.is_empty());
    assert!(
        std::env::var("ROF_TOKEN").is_err(),
        "/providers wrote the env"
    );
    assert!(
        std::env::var("OR_TOKEN").is_err(),
        "/providers wrote the env"
    );
    assert_no_key_material(&app.transcript, FIXTURE_KEY);
}

/// Adding a provider is a mutation, so it stays refused while a goal is
/// live even though its read-only sibling is allowed. The boundary is the
/// pair: `/providers` views, `/provider add` does not.
#[test]
fn provider_mutations_stay_refused_while_providers_is_allowed() {
    let _env = ProvidersEnv::new("refused");
    let (mut app, tx, mut rx) = live_goal();

    for (action, raw) in [
        (
            Action::ProviderAdd("acme".into()),
            "/provider add acme https://llm.acme.test/v1",
        ),
        (Action::ProviderRm("acme".into()), "/provider rm acme"),
        (Action::Login(None), "/login acme"),
        (Action::Logout("acme".into()), "/logout acme"),
    ] {
        app.input.clear();
        app.input.push_str(raw);
        let draft = app.input.clone();
        assert!(
            matches!(
                running_action(&mut app, action, &tx, &draft),
                RunningActionOutcome::Rejected(_)
            ),
            "{raw} was not refused while live"
        );
        assert_eq!(app.input, raw, "{raw} cleared the draft");
    }
    assert!(
        !rof::tui::auth::registry().contains_key("acme"),
        "a refused mutation still changed the registry"
    );
    assert!(rx.try_recv().is_err(), "a refused mutation sent a command");
}

// ---- pane focus keys (P3 D): decided without a terminal ----

/// Tab and Shift-Tab are pane keys, not draft text. The mapping is decided
/// in one place so the pump cannot read a Tab as a character on one
/// terminal and as a focus key on another.
#[test]
fn tab_is_a_focus_key_and_shift_tab_reverses_the_cycle() {
    use crossterm::event::KeyModifiers;
    use rof::tui::run::{focus_step, FocusStep};

    assert_eq!(
        focus_step(KeyCode::Tab, KeyModifiers::NONE),
        Some(FocusStep::Next)
    );
    // crossterm reports Shift-Tab as BackTab; a terminal that sends a tab
    // character with SHIFT is the same press and must not become a glyph.
    assert_eq!(
        focus_step(KeyCode::BackTab, KeyModifiers::SHIFT),
        Some(FocusStep::Prev)
    );
    assert_eq!(
        focus_step(KeyCode::Char('\t'), KeyModifiers::SHIFT),
        Some(FocusStep::Prev)
    );
    // An ordinary character is never a focus key, and neither is a bare tab
    // character with no modifier.
    assert_eq!(focus_step(KeyCode::Char('\t'), KeyModifiers::NONE), None);
    assert_eq!(focus_step(KeyCode::Char('a'), KeyModifiers::NONE), None);
}

/// While a goal is live the focus keys reach the composer branch first, so
/// they can never be typed into the draft — and the cycle still runs.
#[test]
fn focus_keys_are_never_draft_text_while_a_run_is_live() {
    use rof::tui::run::{focus_step, FocusStep};

    let mut app = App::new();
    app.begin_run("busy");
    for (code, modifiers, expected) in [
        (
            KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE,
            FocusStep::Next,
        ),
        (
            KeyCode::BackTab,
            crossterm::event::KeyModifiers::SHIFT,
            FocusStep::Prev,
        ),
    ] {
        // The running reducer leaves both keys to the pump: a focus key
        // moves no run lifecycle and edits no draft.
        assert_eq!(
            handle_running_key(&mut app, code),
            RunningKeyOutcome::Ignored
        );
        assert_eq!(app.input, "", "{code:?} typed into the draft");
        assert_eq!(app.run_mode, RunMode::Running, "{code:?} moved the run");
        assert_eq!(focus_step(code, modifiers), Some(expected));
    }
    // The idle branch is unchanged: focus begins on the composer and its
    // character arm is untouched, so a Tab there is still dropped by the
    // idle pump exactly as it was before focus existed, and typing still
    // edits the draft with no focus key pressed.
    let mut idle = App::new();
    assert_eq!(idle.focus, Focus::Composer);
    assert_eq!(
        handle_running_key(&mut idle, KeyCode::Char('a')),
        RunningKeyOutcome::Ignored
    );
    idle.input.push('a');
    assert_eq!(idle.input, "a", "the composer path changed");
    assert_eq!(idle.focus, Focus::Composer, "typing moved the focus");
}
