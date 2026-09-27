//! A control acknowledgement as a durable trace event.
//!
//! An ack now reaches a console only as `LiveEvent::Trace(TraceEvent::Control(..))`,
//! emitted by the orchestrator onto the run's own `TraceSink`. So the live
//! view and a recorded replay read the same events from one emission seam,
//! and these tests hold that seam to the three properties it buys:
//!
//! * the recorded JSON shape is pinned, so the durable form cannot drift
//!   silently (a diff in recorded evidence, not a silent field rename);
//! * replaying a recorded trace shows the same control history the live
//!   view showed, word for word and slot for slot;
//! * the path the console used to own is not needed: a run nobody watched
//!   still records its control history, and a console that dropped its
//!   receiver still cannot fail a run.
//!
//! No terminal, no network, no credential store: the goals run on the
//! offline stub client with the provider environment scrubbed.

use rof::config::AppConfig;
use rof::engine::control::{RunCommand, RunControl, RunHooks};
use rof::engine::{Orchestrator, Session};
use rof::llm::{ContextService, ExecutorService, StubClient};
use rof::obs::{ControlAck, ControlKind, ControlStatus, LiveEvent, TraceEvent, TraceSink};
use rof::tools::ToolRegistry;
use rof::tui::app::{control_ack_line, App, BusyMode};
use rof::tui::run::{self, LiveSession};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

/// Serializes the tests that scrub the provider env. Tokio's mutex so the
/// guard is async-aware: the runs below await while the env is scrubbed,
/// and a std mutex would poison on the first re-entrant await.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Distinct temp roots per test in one process.
static ROOT_SEQ: AtomicUsize = AtomicUsize::new(0);

/// The goal text every run here uses, and the two commands it is steered
/// with. The ids match what `App` allocates for the same two submissions
/// (1 for the steer, 2 for the queued goal), which is what lets a replayed
/// acknowledgement be compared against a live console's pending slots.
const GOAL: &str = "replay parity goal";
const STEER: &str = "focus on the parser";
const QUEUED_GOAL: &str = "second goal";

/// The exact JSON one recorded acknowledgement must keep. The variant is
/// externally tagged and the ack's four fields serialize in declaration
/// order, so a rename, a reorder, or a new field is a visible diff in the
/// recorded evidence rather than something a replay silently ignores.
const PINNED_APPLIED_ACK: &str = concat!(
    r#"{"Control":{"id":7,"kind":"Steer","status":"Applied","#,
    r#""note":"applies to the next implementer prompt"}}"#
);
const PINNED_REJECTED_ACK: &str = concat!(
    r#"{"Control":{"id":9,"kind":"Queue","status":"Rejected","#,
    r#""note":"rejected: no next round remains to steer"}}"#
);
/// The two lines the parity run above records, byte for byte. The engine's
/// own note wording is part of the durable evidence, so it is pinned here
/// too: a reworded note is a diff a reader can see, never a silent change.
const PINNED_RECORDED_STEER_ACK: &str = concat!(
    r#"{"Control":{"id":1,"kind":"Steer","status":"Applied","#,
    r#""note":"applies to the next implementer prompt"}}"#
);
const PINNED_RECORDED_QUEUE_ACK: &str = concat!(
    r#"{"Control":{"id":2,"kind":"Queue","status":"Applied","#,
    r#""note":"retained for the next goal"}}"#
);

/// The two commands submitted BEFORE the run, so the only thing that can
/// answer them is a real boundary drain.
fn steer_and_queued_goal() -> Vec<RunCommand> {
    vec![
        RunCommand::Steer {
            id: 1,
            text: STEER.to_string(),
        },
        RunCommand::QueueGoal {
            id: 2,
            goal: QUEUED_GOAL.to_string(),
        },
    ]
}

/// One goal through the real stack: the real `Orchestrator`, the real
/// `RunControl` boundary drain, and a `TraceSink` mirroring to the JSONL
/// that `ROF_TRACE` names — the same wiring `setup` and
/// `GoalRunner::execute_with_control` build, minus the interactive
/// session, so the recorded file is exactly the artifact a replay reads.
struct Stack {
    trace: Arc<TraceSink>,
    root: PathBuf,
    trace_path: PathBuf,
    env: Vec<(&'static str, Option<String>)>,
}

impl Stack {
    /// The provider env is removed and `ROF_CREDENTIALS` points at a
    /// scratch path that does not exist, so a login that happens to live on
    /// this machine cannot select a real provider. Nothing here reads the
    /// credential store: the services are the offline stub, built
    /// directly, so no export step can refill a removed key.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "rof-a2-{name}-{}-{}",
            std::process::id(),
            ROOT_SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // OUTSIDE the work root on purpose: a trace file inside it would be
        // a file the run changed, which both fakes a write and pollutes the
        // diff the recorded evidence describes.
        let trace_path = std::env::temp_dir().join(format!(
            "rof-a2-{name}-{}-{}.jsonl",
            std::process::id(),
            ROOT_SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&trace_path);
        let env: Vec<(&'static str, Option<String>)> = [
            "ROF_TOKEN",
            "OR_TOKEN",
            "ROF_CHAT_BASE",
            "ROF_CREDENTIALS",
            "ROF_TRACE",
        ]
        .into_iter()
        .map(|key| (key, std::env::var(key).ok()))
        .collect();
        for key in ["ROF_TOKEN", "OR_TOKEN", "ROF_CHAT_BASE"] {
            std::env::remove_var(key);
        }
        std::env::set_var("ROF_CREDENTIALS", root.join("scratch").join("credentials"));
        std::env::set_var("ROF_TRACE", &trace_path);
        let trace = Arc::new(TraceSink::with_file(&trace_path).unwrap());
        Self {
            trace,
            root,
            trace_path,
            env,
        }
    }

    /// Run one goal. `control` decides whether the boundary drains the
    /// commands at all; `live` is attached to the sink when the caller
    /// wants the console's channel — and a dropped receiver is a
    /// legitimate case, because the run must survive it.
    async fn run_goal(
        &self,
        commands: Vec<RunCommand>,
        control: bool,
        live: Option<UnboundedSender<LiveEvent>>,
    ) -> serde_json::Value {
        // Two rounds, so the FIRST boundary is non-terminal: a steer
        // drained there has a prompt left to reach, and its acknowledgement
        // is Applied rather than Rejected. The bare word `false` is
        // resolved on `PATH` and exits 1, so the check fails for a reason
        // that has nothing to do with the model.
        let cfg = AppConfig {
            max_review_rounds: 2,
            permissions: rof::config::PermissionPolicy {
                allowed_commands: vec!["false".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };
        if let Some(tx) = live {
            self.trace.attach_live(tx);
        }
        let context = ContextService::new(Arc::new(StubClient), "context-stub".to_string());
        let executor =
            ExecutorService::new(Arc::new(StubClient), "executor-stub".to_string(), None);
        let verify = ExecutorService::new(Arc::new(StubClient), "verify-stub".to_string(), None);
        let registry = ToolRegistry::with_defaults(
            self.root.clone(),
            cfg.permissions.clone(),
            cfg.skills.clone(),
        );
        let orch = Orchestrator::new(cfg, self.trace.clone(), context, executor, verify);
        let session = Session::new(GOAL.to_string())
            // The run must be expected to change a file it cannot change:
            // the stub writes nothing, so the harness write gate refuses
            // the reviewer's pass and the goal really reaches a second
            // round instead of ending at the first verdict.
            .expecting_writes(true)
            .with_checks(vec!["false".to_string()]);
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        for command in commands {
            cmd_tx.send(command).unwrap();
        }
        let mut drained = RunControl::new(cmd_rx);
        let mut hooks = if control {
            RunHooks {
                control: Some(&mut drained),
            }
        } else {
            RunHooks::none()
        };
        orch.run_loop_with_hooks(&session, &registry, &self.root, &mut hooks)
            .await
    }

    /// The recorded file, one line per event, exactly as written.
    fn recorded_lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.trace_path)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        for (key, value) in self.env.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_file(&self.trace_path);
    }
}

/// The recorded JSONL ingested the way `tui::run::replay` ingests it: one
/// trimmed non-empty line per event, and a line that does not parse kept as
/// a lenient marker rather than failing the replay.
fn ingest_replay(path: &Path) -> (Vec<TraceEvent>, Vec<String>) {
    // The SAME function `replay` uses, not a copy of it: a test that
    // re-implemented the parser could pass while the real replay path
    // drifted.
    run::ingest_trace(&std::fs::read_to_string(path).unwrap())
}

/// The acknowledgements a recorded run left, in emission order.
fn recorded_acks(events: &[TraceEvent]) -> Vec<ControlAck> {
    events
        .iter()
        .filter_map(|event| match event {
            TraceEvent::Control(ack) => Some(ack.clone()),
            _ => None,
        })
        .collect()
}

/// A console that submitted the same two commands the run was steered
/// with. The submitted ids are asserted, so a recorded acknowledgement
/// can only line up with a slot if the two really are the same command.
fn console_with_pending_commands() -> App {
    let mut app = App::new();
    app.begin_run(GOAL);
    app.set_busy_mode(BusyMode::Steer);
    assert_eq!(app.submit_pending_steer(STEER), 1);
    assert_eq!(app.submit_pending_goal(QUEUED_GOAL), 2);
    app
}

/// The point of the change: one emission seam means the live view and a
/// recorded replay read the SAME control history.
///
/// The run is real, the recording is a real JSONL file, and the replay is
/// ingested by the same parse `tui::run::replay` performs. The live
/// console is fed by the sink's own live forwarding; the replayed console
/// is fed from the file. Both must end with the same slot state, the same
/// busy mode, and the same wording — one transcript line per
/// acknowledgement, and no more.
#[tokio::test]
async fn a_replayed_run_shows_the_control_history_the_live_view_showed() {
    let _env = ENV_LOCK.lock().await;
    let stack = Stack::new("replay-parity");
    let (live_tx, live_rx) = tokio::sync::mpsc::unbounded_channel();
    let out = stack
        .run_goal(steer_and_queued_goal(), true, Some(live_tx))
        .await;
    // Two rounds really ran, so the first boundary was non-terminal and the
    // steer's Applied answer below is asserting against a configuration that
    // had a second round to reach.
    assert_eq!(
        out["rounds"], 2,
        "the run did not reach its second round: {out}"
    );

    // The live console: the sink forwarded each event, and `LiveSession`
    // routed them to the App exactly as the pump does.
    let mut live_app = console_with_pending_commands();
    let mut session = LiveSession::new(live_rx);
    assert!(
        session.drain(&mut live_app).is_none(),
        "a run with no terminal event reported an outcome anyway"
    );
    // The run had already returned, so one drain took every event; a
    // second must be a no-op rather than a source of duplicates.
    let after_first = live_app.transcript.len();
    assert!(session.drain(&mut live_app).is_none());
    assert_eq!(
        live_app.transcript.len(),
        after_first,
        "a second drain applied events again"
    );

    // The replayed console: the same recorded file, ingested the way
    // `replay` ingests it.
    let (events, unknown) = ingest_replay(&stack.trace_path);
    assert!(
        unknown.is_empty(),
        "the recorded file has lines replay cannot parse: {unknown:?}"
    );
    // The recorded file itself, byte for byte: the durable form of an
    // acknowledgement is evidence, so its shape is asserted here as well
    // as in the pure pin below.
    let recorded = stack.recorded_lines();
    assert!(
        recorded.contains(&PINNED_RECORDED_STEER_ACK.to_string())
            && recorded.contains(&PINNED_RECORDED_QUEUE_ACK.to_string()),
        "the recorded control lines are not the pinned shape: {recorded:?}"
    );
    let acks = recorded_acks(&events);
    assert_eq!(
        acks.len(),
        2,
        "expected one ack per submitted command: {acks:?}"
    );
    assert_eq!(acks[0].id, 1);
    assert_eq!(acks[0].kind, ControlKind::Steer);
    assert_eq!(acks[0].status, ControlStatus::Applied);
    assert_eq!(acks[1].id, 2);
    assert_eq!(acks[1].kind, ControlKind::Queue);
    assert_eq!(acks[1].status, ControlStatus::Applied);

    // The replay view itself: one line per acknowledgement, byte-identical
    // to what the live console wrote for the same acknowledgements.
    let mut replay_app = App::new();
    replay_app.set_replay_events(events.clone());
    replay_app.set_replay_unknown(unknown);
    for ack in &acks {
        let line = control_ack_line(ack);
        assert_eq!(
            live_app
                .transcript
                .iter()
                .filter(|seen| *seen == &line)
                .count(),
            1,
            "the live console did not write exactly one line for {ack:?}: {:?}",
            live_app.transcript
        );
        assert_eq!(
            replay_app
                .transcript
                .iter()
                .filter(|seen| *seen == &line)
                .count(),
            1,
            "the replayed console did not write exactly one line for {ack:?}: {:?}",
            replay_app.transcript
        );
        assert_eq!(
            live_app.transcript.last().map(String::as_str),
            replay_app.transcript.last().map(String::as_str),
            "live and replayed wording diverged"
        );
    }
    // The busy mode is still reported, live and replayed, and the queue
    // answer left the goal owed to the next boundary in both.
    assert!(
        live_app.control_summary().starts_with("steer"),
        "{summary}",
        summary = live_app.control_summary()
    );
    assert!(
        replay_app.control_summary().starts_with("steer"),
        "{summary}",
        summary = replay_app.control_summary()
    );
    assert!(
        live_app.control_summary().contains("goal queued (2)"),
        "{summary}",
        summary = live_app.control_summary()
    );
    assert!(
        live_app.pending_steer.is_none(),
        "the applied steer ack freed nothing"
    );
    assert_eq!(
        live_app.pending_goal.as_ref().map(|pending| pending.id),
        Some(2),
        "an applied queue ack freed the goal before a boundary consumed it"
    );

    // And the recorded events, folded into a console that had submitted the
    // same two commands, leave the same slots occupied as the live fold
    // did: the replay reads the answer, it does not re-derive it.
    let mut folded = console_with_pending_commands();
    for event in &events {
        folded.on_event(event);
    }
    assert_eq!(folded.pending_steer, live_app.pending_steer);
    assert_eq!(folded.pending_goal, live_app.pending_goal);
    assert_eq!(folded.control_summary(), live_app.control_summary());
    assert_eq!(
        folded.transcript.last().map(String::as_str),
        live_app.transcript.last().map(String::as_str),
        "the replayed acknowledgement line differs from the live one"
    );
}

/// The durable form of an acknowledgement, byte for byte, and its round
/// trip. A field rename, a reorder, or a new field changes the recorded
/// evidence — which is the point: the shape is pinned so it cannot drift
/// silently under a reader that tolerates unknown fields.
#[test]
fn a_recorded_ack_has_a_pinned_json_shape_and_round_trips() {
    let applied = ControlAck {
        id: 7,
        kind: ControlKind::Steer,
        status: ControlStatus::Applied,
        note: "applies to the next implementer prompt".to_string(),
    };
    let rejected = ControlAck {
        id: 9,
        kind: ControlKind::Queue,
        status: ControlStatus::Rejected,
        note: "rejected: no next round remains to steer".to_string(),
    };
    for (ack, pinned) in [
        (applied.clone(), PINNED_APPLIED_ACK),
        (rejected.clone(), PINNED_REJECTED_ACK),
    ] {
        let line = serde_json::to_string(&TraceEvent::Control(ack.clone())).unwrap();
        assert_eq!(line, pinned, "the recorded shape of {ack:?} drifted");
        // The same line parses back to the same acknowledgement, which is
        // what makes the file a replayable record rather than a log.
        let parsed = serde_json::from_str::<TraceEvent>(pinned).unwrap();
        let TraceEvent::Control(round_tripped) = parsed else {
            panic!("a recorded ack line did not parse back as Control: {pinned}");
        };
        assert_eq!(round_tripped, ack);
    }
}

/// A run with no boundary hook is the pre-P1b behavior: the boundary
/// reports nothing, so the run records no control event at all — on the
/// durable sink or in the file a replay would read.
#[tokio::test]
async fn a_run_with_no_hooks_records_no_control_event() {
    let _env = ENV_LOCK.lock().await;
    let stack = Stack::new("no-hooks");
    stack.run_goal(steer_and_queued_goal(), false, None).await;

    assert!(
        !stack
            .trace
            .events()
            .iter()
            .any(|event| matches!(event, TraceEvent::Control(_))),
        "a run with no hooks recorded a control event"
    );
    let (events, unknown) = ingest_replay(&stack.trace_path);
    assert!(
        unknown.is_empty(),
        "unparseable recorded lines: {unknown:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, TraceEvent::Control(_))),
        "the recorded file of a hookless run carries a control event"
    );
}

/// A console that dropped its receiver is still the console's choice, and
/// it must not fail a run: the acknowledgement is on the durable sink
/// before the live forward is attempted, so the run completes and its
/// control history is still recorded.
#[tokio::test]
async fn a_dropped_live_receiver_cannot_fail_the_run() {
    let _env = ENV_LOCK.lock().await;
    let stack = Stack::new("dropped-live");
    let (live_tx, live_rx) = tokio::sync::mpsc::unbounded_channel::<LiveEvent>();
    drop(live_rx);
    let out = stack
        .run_goal(steer_and_queued_goal(), true, Some(live_tx))
        .await;

    assert_eq!(
        out["rounds"], 2,
        "the run did not finish its rounds with no receiver attached: {out}"
    );
    let acks = recorded_acks(&stack.trace.events());
    assert_eq!(
        acks.len(),
        2,
        "the acknowledgements were lost with the receiver: {acks:?}"
    );
    assert_eq!(acks[0].id, 1);
    assert_eq!(acks[1].id, 2);
    let (events, unknown) = ingest_replay(&stack.trace_path);
    assert!(
        unknown.is_empty(),
        "unparseable recorded lines: {unknown:?}"
    );
    assert_eq!(recorded_acks(&events), acks);
}
