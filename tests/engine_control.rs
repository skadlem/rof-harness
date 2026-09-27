use rof::engine::control::{RunCommand, RunControl, RunHooks};
use rof::obs::{ControlAck, ControlStatus};

/// A goal retained from an earlier boundary was submitted before anything this
/// drain sees, so the stop that drops it is acknowledged in that order: the
/// retained goal's rejection comes first, exactly once, and only then the acks
/// of the commands drained alongside the stop.
#[tokio::test]
async fn a_stop_acking_a_retained_goal_comes_before_this_drain_s_own_acks() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut control = RunControl::new(rx);

    tx.send(RunCommand::QueueGoal {
        id: 1,
        goal: "queued first".into(),
    })
    .unwrap();
    let first = control.drain_boundary(false);
    assert_eq!(first.goal.as_ref().map(|g| g.0), Some(1));
    assert_eq!(first.acks.len(), 1);
    assert_eq!(first.acks[0].status, ControlStatus::Applied);

    // A steer and a stop arrive at the next boundary, both submitted after the
    // goal the stop is about to drop.
    tx.send(RunCommand::Steer {
        id: 2,
        text: "focus the retry".into(),
    })
    .unwrap();
    tx.send(RunCommand::Stop { id: 3 }).unwrap();
    let second = control.drain_boundary(false);

    assert_eq!(
        second.acks.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "the retained goal was submitted first, so it is answered first"
    );
    let retained = &second.acks[0];
    assert_eq!(retained.kind, rof::obs::ControlKind::Queue);
    assert_eq!(retained.status, ControlStatus::Rejected);
    assert!(
        retained.note.contains("stop"),
        "the rejection names the stop: {}",
        retained.note
    );
    assert_eq!(
        second.acks.iter().filter(|a| a.id == 1).count(),
        1,
        "one acknowledgement per command, never two"
    );
    assert!(second.goal.is_none());
    assert!(control.take_queued_goal().is_none());
}

#[tokio::test]
async fn control_drain_keeps_latest_and_rejects_replacements() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut control = RunControl::new(rx);
    tx.send(RunCommand::Steer {
        id: 1,
        text: "old".into(),
    })
    .unwrap();
    tx.send(RunCommand::Steer {
        id: 2,
        text: "new".into(),
    })
    .unwrap();
    tx.send(RunCommand::QueueGoal {
        id: 3,
        goal: "first queued".into(),
    })
    .unwrap();
    tx.send(RunCommand::QueueGoal {
        id: 4,
        goal: "latest queued".into(),
    })
    .unwrap();

    let batch = control.drain_boundary(false);
    assert_eq!(batch.steer.as_ref().map(|s| s.1.as_str()), Some("new"));
    assert_eq!(
        batch.goal.as_ref().map(|g| g.1.as_str()),
        Some("latest queued")
    );
    assert!(batch
        .acks
        .iter()
        .any(|a| a.id == 1 && a.status == ControlStatus::Rejected));
    assert!(batch
        .acks
        .iter()
        .any(|a| a.id == 3 && a.status == ControlStatus::Rejected));
    assert_eq!(
        batch
            .acks
            .iter()
            .filter(|a| a.status == ControlStatus::Applied)
            .count(),
        2
    );
    // The displaced command names the one that replaced it.
    let displaced = batch.acks.iter().find(|a| a.id == 1).unwrap();
    assert!(displaced.note.contains('2'), "note: {}", displaced.note);
    // Ordered by the channel: the first submission is answered first.
    assert_eq!(
        batch.acks.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    // A drained goal is retained for the goal loop and taken exactly once.
    assert_eq!(
        control.take_queued_goal().map(|g| g.1),
        Some("latest queued".into())
    );
    assert!(control.take_queued_goal().is_none());
}

#[tokio::test]
async fn terminal_drain_rejects_steer_but_keeps_queue() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut control = RunControl::new(rx);
    tx.send(RunCommand::Steer {
        id: 1,
        text: "too late".into(),
    })
    .unwrap();
    tx.send(RunCommand::QueueGoal {
        id: 2,
        goal: "next".into(),
    })
    .unwrap();
    let batch = control.drain_boundary(true);
    assert!(batch.steer.is_none());
    assert!(batch
        .acks
        .iter()
        .any(|a| a.id == 1 && a.note.contains("no next round")));
    assert_eq!(control.take_queued_goal().map(|g| g.1), Some("next".into()));
}

/// A later queued goal displaces a goal retained from an earlier boundary, and
/// the displaced one is acknowledged rather than silently overwritten.
#[tokio::test]
async fn a_later_queue_replaces_the_retained_goal_at_the_next_boundary() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut control = RunControl::new(rx);
    tx.send(RunCommand::QueueGoal {
        id: 1,
        goal: "first".into(),
    })
    .unwrap();
    let first = control.drain_boundary(false);
    assert_eq!(first.goal.as_ref().map(|g| g.0), Some(1));
    assert_eq!(first.acks.len(), 1);
    assert_eq!(first.acks[0].status, ControlStatus::Applied);

    tx.send(RunCommand::QueueGoal {
        id: 2,
        goal: "second".into(),
    })
    .unwrap();
    let second = control.drain_boundary(false);
    assert_eq!(second.goal.as_ref().map(|g| g.1.as_str()), Some("second"));
    assert_eq!(second.acks.len(), 2);
    assert_eq!(second.acks[0].id, 1);
    assert_eq!(second.acks[0].status, ControlStatus::Rejected);
    assert!(
        second.acks[0].note.contains('2'),
        "note: {}",
        second.acks[0].note
    );
    assert_eq!(second.acks[1].id, 2);
    assert_eq!(second.acks[1].status, ControlStatus::Applied);
    assert_eq!(
        control.take_queued_goal().map(|g| g.1),
        Some("second".into())
    );
}

/// A stop is sticky, and a queued goal is dropped rather than started: the user
/// asked for the run to end, not for a new goal to begin.
#[tokio::test]
async fn stop_drops_the_queued_goal_at_the_terminal_boundary() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut control = RunControl::new(rx);
    tx.send(RunCommand::QueueGoal {
        id: 1,
        goal: "next".into(),
    })
    .unwrap();
    tx.send(RunCommand::Stop { id: 5 }).unwrap();

    assert!(!control.stop_requested(), "a stop is only seen at a drain");
    let batch = control.drain_boundary(true);
    assert!(control.stop_requested());
    assert!(batch.goal.is_none());
    let rejected = batch
        .acks
        .iter()
        .find(|a| a.id == 1)
        .expect("the queued goal is acknowledged");
    assert_eq!(rejected.status, ControlStatus::Rejected);
    assert!(
        rejected.note.contains("stop"),
        "the rejection names the stop: {}",
        rejected.note
    );
    assert!(batch
        .acks
        .iter()
        .any(|a| a.id == 5 && a.status == ControlStatus::Applied));
    assert!(control.take_queued_goal().is_none());
}

/// The hook is a boundary addition, not a behavior change: a caller with no
/// control (every existing `run_loop` caller) sees its feedback untouched.
#[test]
fn no_hooks_leave_feedback_untouched() {
    let mut feedback = String::from("reviewer feedback: missing tests");
    let mut hooks = RunHooks::none();
    // No control and therefore nothing to report: a hookless boundary is a
    // no-op that RETURNS no acknowledgements, so a caller that records what
    // it gets has nothing to record. This is the pre-P1b behavior, and it is
    // asserted on the return value too, not only on the feedback.
    assert!(hooks.apply_boundary(&mut feedback, false).is_empty());
    assert!(hooks.apply_boundary(&mut feedback, true).is_empty());
    assert_eq!(feedback, "reviewer feedback: missing tests");
}

/// Every ack the drain produced is published on the live channel, in drain
/// order, so a console matches them to its pending slots by id.
#[tokio::test]
async fn hooks_publish_every_ack_in_order_and_apply_a_surviving_steer() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut control = RunControl::new(cmd_rx);
    cmd_tx
        .send(RunCommand::Steer {
            id: 1,
            text: "old".into(),
        })
        .unwrap();
    cmd_tx
        .send(RunCommand::Steer {
            id: 2,
            text: "focus on the parser".into(),
        })
        .unwrap();
    cmd_tx
        .send(RunCommand::QueueGoal {
            id: 3,
            goal: "next goal".into(),
        })
        .unwrap();

    let mut feedback = String::from("reviewer feedback: missing tests");
    let mut hooks = RunHooks {
        control: Some(&mut control),
    };
    // The hook RETURNS the acknowledgements; the orchestrator is the only
    // emitter, and the sink's live forwarding is how a console sees them.
    let acks = hooks.apply_boundary(&mut feedback, false);
    let ids: Vec<u64> = acks.iter().map(|ack| ack.id).collect();
    assert_eq!(ids, vec![1, 2, 3]);
    assert!(
        feedback.contains("focus on the parser"),
        "the surviving steer reaches the next prompt: {feedback}"
    );
    assert!(!feedback.contains("old"));
    assert_eq!(
        control.take_queued_goal().map(|g| g.1),
        Some("next goal".into())
    );
}

/// A steer drained at a terminal boundary has no prompt left to reach, so it
/// is acknowledged and dropped rather than appended to a dead feedback string.
#[tokio::test]
async fn terminal_hook_rejects_the_steer_and_keeps_the_queue() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut control = RunControl::new(cmd_rx);
    cmd_tx
        .send(RunCommand::Steer {
            id: 1,
            text: "too late".into(),
        })
        .unwrap();
    cmd_tx
        .send(RunCommand::QueueGoal {
            id: 2,
            goal: "next".into(),
        })
        .unwrap();

    let mut feedback = String::from("reviewer feedback: missing tests");
    let mut hooks = RunHooks {
        control: Some(&mut control),
    };
    let acks: Vec<ControlAck> = hooks.apply_boundary(&mut feedback, true);

    assert_eq!(feedback, "reviewer feedback: missing tests");
    assert_eq!(acks.len(), 2);
    assert_eq!(acks[0].id, 1);
    assert_eq!(acks[0].status, ControlStatus::Rejected);
    assert!(acks[0].note.contains("no next round"));
    assert_eq!(acks[1].id, 2);
    assert_eq!(acks[1].status, ControlStatus::Applied);
    assert_eq!(control.take_queued_goal().map(|g| g.1), Some("next".into()));
}
