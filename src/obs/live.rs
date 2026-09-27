use super::trace::TraceEvent;

/// A goal's run boundary, so a live view can bracket activity without
/// re-deriving it from the trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    Started,
    Finished,
}

/// The terminal outcome of one goal run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalFinished {
    pub passed: bool,
    pub error: Option<String>,
}

/// Which pending slot a live command addressed. `Stop` carries no text:
/// it is the request to end the run at the next boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Steer,
    Queue,
    Stop,
}

impl ControlKind {
    /// Lowercase label for transcript and status wording.
    pub fn label(self) -> &'static str {
        match self {
            ControlKind::Steer => "steer",
            ControlKind::Queue => "queue",
            ControlKind::Stop => "stop",
        }
    }
}

/// Whether a command was honored at a boundary or refused. A displaced
/// or too-late command is `Rejected`, never silently dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlStatus {
    Applied,
    Rejected,
}

impl ControlStatus {
    pub fn label(self) -> &'static str {
        match self {
            ControlStatus::Applied => "applied",
            ControlStatus::Rejected => "rejected",
        }
    }
}

/// The ordered answer to one live command, identified by the `id` the
/// console allocated when the user submitted it. Acknowledgements are
/// ordered by the channel, so a console matches them to its pending slot
/// by `id` and never by arrival time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlAck {
    pub id: u64,
    pub kind: ControlKind,
    pub status: ControlStatus,
    /// Human-readable reason, e.g. `replaced by a later steer`. Never
    /// carries command text or credentials.
    pub note: String,
}

/// What a live subscriber receives while a goal runs. The trace remains the
/// durable source of truth; this channel is ordered to match it.
///
/// `GoalFinished` is one goal's outcome; `Finished` is the session-terminal
/// outcome and is the only event that ends the interactive run.
#[derive(Debug, Clone)]
pub enum LiveEvent {
    Trace(TraceEvent),
    Boundary(Boundary),
    Control(ControlAck),
    GoalFinished(GoalFinished),
    Finished(GoalFinished),
}
