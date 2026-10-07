use std::collections::HashSet;

use crate::event::AgentEvent;
use crate::types::{MessageId, ToolCallId, TurnId};

/// Pairing invariant: every Start is eventually paired with an End.
/// One-directional — an End without a Start passes; a Start without an End
/// fails.
pub fn check_pairing(events: &[AgentEvent]) -> bool {
    let mut run_starts = 0;
    let mut run_ends = 0;
    let mut turn_starts: HashSet<TurnId> = HashSet::new();
    let mut turn_ends: HashSet<TurnId> = HashSet::new();
    let mut msg_starts: HashSet<MessageId> = HashSet::new();
    let mut msg_ends: HashSet<MessageId> = HashSet::new();
    let mut tool_starts: HashSet<ToolCallId> = HashSet::new();
    let mut tool_ends: HashSet<ToolCallId> = HashSet::new();
    for e in events {
        match e {
            AgentEvent::RunStart { .. } => run_starts += 1,
            AgentEvent::RunEnd { .. } => run_ends += 1,
            AgentEvent::TurnStart { turn } => {
                turn_starts.insert(*turn);
            }
            AgentEvent::TurnEnd { turn, .. } => {
                turn_ends.insert(*turn);
            }
            AgentEvent::MessageStart { id, .. } => {
                msg_starts.insert(*id);
            }
            AgentEvent::MessageEnd { id, .. } => {
                msg_ends.insert(*id);
            }
            AgentEvent::ToolStart { id, .. } => {
                tool_starts.insert(id.clone());
            }
            AgentEvent::ToolEnd { id, .. } => {
                tool_ends.insert(id.clone());
            }
            _ => {}
        }
    }
    run_starts <= run_ends
        && turn_starts.iter().all(|t| turn_ends.contains(t))
        && msg_starts.iter().all(|m| msg_ends.contains(m))
        && tool_starts.iter().all(|t| tool_ends.contains(t))
}
