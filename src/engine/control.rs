use crate::obs::{ControlAck, ControlKind, ControlStatus};
use tokio::sync::mpsc::UnboundedReceiver;

/// One live command from the console to a running worker. Every command is
/// answered at the next round boundary with a `ControlAck` carrying the same
/// `id`, so the console matches the answer to its pending slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunCommand {
    /// Guidance for the next implementer prompt. Never reaches the round that
    /// is in flight.
    Steer { id: u64, text: String },
    /// One next goal, started by the goal loop after the current goal's
    /// per-goal outcome.
    QueueGoal { id: u64, goal: String },
    /// End the run: no next goal is started, whatever is queued.
    Stop { id: u64 },
}

/// What one boundary drain produced: the commands that survive it and the
/// ordered acknowledgements for every command the drain consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryBatch {
    /// The surviving steer, to be appended to the next round's feedback.
    /// Always `None` at a terminal boundary: there is no next round.
    pub steer: Option<(u64, String)>,
    /// The retained queued goal, if one was queued and not stopped.
    pub goal: Option<(u64, String)>,
    /// One ack per drained command, in channel order.
    pub acks: Vec<ControlAck>,
}

/// The worker side of the live command channel.
///
/// The receiver is drained only at round boundaries, so a command can never
/// mutate an in-flight model call or tool invocation. Steer and queue each
/// have exactly one slot: a newer submission replaces the earlier one and the
/// displaced command is acknowledged as `Rejected` rather than silently lost.
pub struct RunControl {
    rx: UnboundedReceiver<RunCommand>,
    /// The steer waiting to reach the next implementer prompt. A drain hands
    /// it to the boundary batch and clears it, so a steer is delivered once.
    pending_steer: Option<(u64, String)>,
    /// The queued goal, retained across boundaries until the goal loop takes
    /// it exactly once.
    pending_goal: Option<(u64, String)>,
    /// The id of the `Stop` that ended the run, named in the acknowledgements
    /// of the commands it displaces.
    stop_id: Option<u64>,
    /// Sticky: once a stop was seen, no queued goal is started.
    stop_requested: bool,
}

impl RunControl {
    pub fn new(rx: UnboundedReceiver<RunCommand>) -> Self {
        Self {
            rx,
            pending_steer: None,
            pending_goal: None,
            stop_id: None,
            stop_requested: false,
        }
    }

    /// Drain every command available right now, in channel order.
    ///
    /// `terminal` marks a boundary no later round will read feedback at: the
    /// run's last round, a round that ends the attempt, or the run's terminal
    /// boundary. A steer drained there has no prompt left to reach and is
    /// rejected, while a queued goal is retained for the goal loop unless a
    /// stop was requested.
    pub fn drain_boundary(&mut self, terminal: bool) -> BoundaryBatch {
        let mut commands = Vec::new();
        while let Ok(cmd) = self.rx.try_recv() {
            commands.push(cmd);
        }
        // A stop anywhere in this drain — or one seen earlier — decides every
        // queued goal in it, whatever order the two commands arrived in.
        let stop_id = self.stop_id.or_else(|| {
            commands.iter().find_map(|c| match c {
                RunCommand::Stop { id } => Some(*id),
                _ => None,
            })
        });
        let stopping = self.stop_requested || stop_id.is_some();
        // One slot per kind: the newest submission of a kind is the one that
        // survives, and everything before it is displaced.
        let latest_steer = commands.iter().rev().find_map(|c| match c {
            RunCommand::Steer { id, .. } => Some(*id),
            _ => None,
        });
        let latest_goal = commands.iter().rev().find_map(|c| match c {
            RunCommand::QueueGoal { id, .. } => Some(*id),
            _ => None,
        });
        let mut acks = Vec::new();

        // A goal retained from an earlier boundary was submitted before every
        // command in this drain, so it is resolved first and its
        // acknowledgement comes first. Three outcomes, one ack at most: a
        // newer queue displaces it, a stop discards it, and otherwise it stays
        // retained untouched for the goal loop.
        if let Some((old, goal)) = self.pending_goal.take() {
            if let Some(newest) = latest_goal {
                acks.push(rejected(
                    old,
                    ControlKind::Queue,
                    format!("replaced by queued goal {newest}"),
                ));
            } else if stopping {
                acks.push(rejected(old, ControlKind::Queue, stop_note(stop_id)));
            } else {
                self.pending_goal = Some((old, goal));
            }
        }

        for cmd in commands {
            match cmd {
                RunCommand::Steer { id, text } => {
                    if Some(id) != latest_steer {
                        acks.push(rejected(
                            id,
                            ControlKind::Steer,
                            format!("replaced by steer {}", latest_steer.unwrap_or(id)),
                        ));
                    } else if terminal {
                        acks.push(rejected(
                            id,
                            ControlKind::Steer,
                            "rejected: no next round remains to steer".to_string(),
                        ));
                    } else {
                        self.pending_steer = Some((id, text));
                        acks.push(applied(
                            id,
                            ControlKind::Steer,
                            "applies to the next implementer prompt".to_string(),
                        ));
                    }
                }
                RunCommand::QueueGoal { id, goal } => {
                    if Some(id) != latest_goal {
                        acks.push(rejected(
                            id,
                            ControlKind::Queue,
                            format!("replaced by queued goal {}", latest_goal.unwrap_or(id)),
                        ));
                    } else if stopping {
                        acks.push(rejected(id, ControlKind::Queue, stop_note(stop_id)));
                    } else {
                        self.pending_goal = Some((id, goal));
                        acks.push(applied(
                            id,
                            ControlKind::Queue,
                            "retained for the next goal".to_string(),
                        ));
                    }
                }
                RunCommand::Stop { id } => {
                    self.stop_requested = true;
                    self.stop_id = stop_id.or(Some(id));
                    acks.push(applied(
                        id,
                        ControlKind::Stop,
                        "accepted: the run stops at this boundary".to_string(),
                    ));
                }
            }
        }

        BoundaryBatch {
            steer: self.pending_steer.take(),
            goal: self.pending_goal.clone(),
            acks,
        }
    }

    /// The retained queued goal, removed and returned exactly once. `None`
    /// after the first take, or when a stop discarded it.
    pub fn take_queued_goal(&mut self) -> Option<(u64, String)> {
        self.pending_goal.take()
    }

    /// Sticky: true once any `Stop` has been drained.
    pub fn stop_requested(&self) -> bool {
        self.stop_requested
    }
}

fn applied(id: u64, kind: ControlKind, note: String) -> ControlAck {
    ControlAck {
        id,
        kind,
        status: ControlStatus::Applied,
        note,
    }
}

fn rejected(id: u64, kind: ControlKind, note: String) -> ControlAck {
    ControlAck {
        id,
        kind,
        status: ControlStatus::Rejected,
        note,
    }
}

fn stop_note(stop_id: Option<u64>) -> String {
    match stop_id {
        Some(id) => format!("rejected: stop {id} was requested, so no next goal is started"),
        None => "rejected: a stop was requested, so no next goal is started".to_string(),
    }
}

/// The engine-level boundary hook: it drains the console's commands and hands
/// the surviving steer to the next prompt, and it RETURNS the ordered
/// acknowledgements rather than publishing them itself.
///
/// The caller — the orchestrator, which owns the durable sink — is the only
/// emitter. That is the whole reason this type carries no channel: one
/// emission seam means the live console and a later replay read the same
/// recorded events, in the same order, and a run whose console has gone away
/// still leaves its control history in the trace.
pub struct RunHooks<'a> {
    pub control: Option<&'a mut RunControl>,
}

impl<'a> RunHooks<'a> {
    /// No control: every boundary is a no-op that reports nothing.
    pub fn none() -> Self {
        Self { control: None }
    }

    /// One round boundary: drain the console's commands, return the ordered
    /// acknowledgements for the caller to emit, and hand a surviving steer
    /// to the next implementer prompt through the same `feedback` string the
    /// round loop already carries.
    ///
    /// `terminal` means no further round will consume this feedback: the run's
    /// last round, a round that ends the attempt, or the run's terminal
    /// boundary. A terminal boundary drains for the same reason but no prompt
    /// follows it, so no steer survives it — while a queued goal is still
    /// retained unless a stop was requested.
    ///
    /// `#[must_use]` because dropping the returned list is exactly how a
    /// command goes unanswered: the caller is the only emitter, so a
    /// discarded acknowledgement is neither recorded nor ever shown.
    #[must_use = "the caller must emit these acknowledgements; dropping them leaves a command unanswered"]
    pub fn apply_boundary(&mut self, feedback: &mut String, terminal: bool) -> Vec<ControlAck> {
        let Some(control) = self.control.as_deref_mut() else {
            return Vec::new();
        };
        let batch = control.drain_boundary(terminal);
        if let Some((_, text)) = batch.steer {
            if !feedback.is_empty() {
                feedback.push('\n');
            }
            feedback.push_str("USER STEER: ");
            feedback.push_str(&text);
            feedback.push_str("\n[end of user steer]\n");
        }
        batch.acks
    }
}
