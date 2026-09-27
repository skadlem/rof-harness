//! §6: one honest context number per agent turn.
//!
//! The competitive axis is context-per-turn. Two harnesses ran the same model
//! at the same thinking effort, tied on quality, and one spent more than twice
//! the cost per task; the cause was how much context each harness fed the
//! model on each turn. Until rof measures its own number, any cut is a guess
//! about the wrong thing — so this module measures, and cuts nothing.
//!
//! Three rules, and they are the whole design:
//!
//! - **Measure the string that goes out.** The call is made with a rendered
//!   prompt, so that is what is measured: the §4.1 assembler's
//!   [`PromptParts::full`](super::assembler::PromptParts::full) for the
//!   implementer (the layered head PLUS the volatile tail below it) and the
//!   `CtxView` prompt for the reviewer. A sum of the layers, a token
//!   estimate, or the layered head alone are all *not* it — each is a number
//!   about a prompt nobody sent.
//! - **One event per call, per agent.** The implementer can be called twice
//!   in a round: the second call is the `reads` re-ask, which re-sends the
//!   whole assembled context plus the files the model asked for. That is a
//!   separate turn of cost and is measured as one, not averaged into the
//!   first ask.
//! - **Emit, don't judge.** Nothing here truncates, filters, reorders or
//!   annotates a prompt. The measurement sits beside the request the model
//!   received and adds no bytes to it.

use crate::obs::{TraceEvent, TraceSink};

/// The first call of a turn — the ask the orchestrator's round began with.
pub const TURN_ASK: &str = "ask";

/// The follow-up call a model that answered `{"reads": [...]}` gets: the same
/// assembled context plus the files it asked for. Measured separately, because
/// re-sending the context is the cost this metric exists to make visible.
pub const TURN_REASK: &str = "re-ask";

/// A single-call agent's call. The reviewer has no read-request turn, so one
/// call per round is its whole turn.
pub const TURN_CALL: &str = "call";

/// Records the context one agent call received, as
/// [`TraceEvent::ContextMeasured`].
///
/// `rendered` is the prompt as it is about to be sent, so the recorded
/// `chars` is the model's actual input size rather than anything derived
/// from it. Pure measurement: the event is durable, replayable and folded by
/// the eval layer, and `rendered` is untouched.
///
/// Agents call this immediately before building their request, because that
/// is the last point at which the string is still the one the model gets.
pub fn measure_turn(trace: &TraceSink, agent: &str, turn: &str, rendered: &str) {
    let chars = rendered.chars().count() as u64;
    trace.emit(TraceEvent::ContextMeasured {
        agent: agent.to_string(),
        turn: turn.to_string(),
        chars,
        est_tokens: chars / 4,
    });
}
