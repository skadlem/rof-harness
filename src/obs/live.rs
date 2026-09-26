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

/// What a live subscriber receives while a goal runs. The trace remains the
/// durable source of truth; this channel is ordered to match it.
#[derive(Debug, Clone)]
pub enum LiveEvent {
    Trace(TraceEvent),
    Boundary(Boundary),
    Finished(GoalFinished),
}
