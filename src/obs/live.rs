use super::trace::TraceEvent;
use serde::{Deserialize, Serialize};

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

/// The goal's ONE lesson (learn mode §5), as it rides the run result's
/// `lesson` value.
///
/// Two fields, both produced by `agents::teach`: the `concept` label the
/// model named, and the `text` composed from the model's own `because` (plus
/// whatever that run dropped for the one-concept-per-goal rule). The harness
/// adds no prose of its own, so a consumer shows the text and keys the
/// answer commands off `concept` — never by parsing prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lesson {
    pub concept: String,
    pub text: String,
}

impl Lesson {
    /// Read the `lesson` field of a run result, or `None` when this run
    /// taught nothing.
    ///
    /// The anti-nag gate is silence, so `null` is the COMMON case and the
    /// ordinary one, not a fault. A value with no usable concept label is
    /// also `None`: the two answer commands would have nothing to name, and
    /// a concept guessed out of the prose is a concept the store would then
    /// hold under a name nobody chose.
    pub fn from_result(lesson: Option<&serde_json::Value>) -> Option<Self> {
        let lesson = lesson?;
        let concept = lesson.get("concept")?.as_str()?.trim();
        if concept.is_empty() {
            return None;
        }
        // The text is the model's own composition. A result that carried the
        // concept without it still gets the concept shown, rather than a
        // lesson line with nothing in it.
        let text = lesson
            .get("text")
            .and_then(|t| t.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or(concept);
        Some(Self {
            concept: concept.to_string(),
            text: text.to_string(),
        })
    }
}

/// Which pending slot a live command addressed. `Stop` carries no text:
/// it is the request to end the run at the next boundary.
///
/// Serialized with the acknowledgement, so a recorded trace replays the same
/// words the console saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
///
/// Serializable because an acknowledgement is durable evidence: the recorded
/// trace replays the same control history the live view showed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
/// outcome and is the only event that ends the interactive run. `Lesson` is
/// its own variant rather than a field on `GoalFinished` for the same reason
/// the trace keeps its own: the outcome is constructed in every place a goal
/// can end, and a lesson is not an outcome — it is the one concept the goal
/// taught, and a goal that taught nothing still finished.
///
/// There is deliberately no `Control` variant: an acknowledgement is a
/// [`TraceEvent::Control`], so the live console and a recorded replay read
/// the same event from one emission seam, and a run nobody watched still
/// leaves its control history in the trace.
#[derive(Debug, Clone)]
pub enum LiveEvent {
    Trace(TraceEvent),
    Boundary(Boundary),
    GoalFinished(GoalFinished),
    /// The goal's one lesson, published beside that goal's outcome.
    Lesson(Lesson),
    Finished(GoalFinished),
}
