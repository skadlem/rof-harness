//! Live event vocabulary (TUI/headless contract). See research/crate-agent-event.md.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type RunId = u64;
pub type TurnId = u64;
pub type MessageId = u64;
pub type ToolCallId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeltaKind {
    Text,
    Thinking,
    ToolCall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDelta {
    pub kind: DeltaKind,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnEndReason {
    Completed,
    Error,
    Aborted,
    Stopped,
    MaxSteps,
    BudgetExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunOutcome {
    Passed,
    Failed(String),
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlKind {
    Steer,
    Queue,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlStatus {
    Applied,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlAck {
    pub id: u64,
    pub kind: ControlKind,
    pub status: ControlStatus,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentError {
    pub code: String,
    pub message: String,
}

/// The live vocabulary. Flat, tagged, serializable: one enum, one emission seam.
/// Partials are never authoritative; the terminal frame is mandatory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    RunStart {
        run_id: RunId,
        goal: String,
    },
    RunEnd {
        outcome: RunOutcome,
        messages: Vec<Message>,
    },
    TurnStart {
        turn: TurnId,
    },
    TurnEnd {
        turn: TurnId,
        reason: TurnEndReason,
    },
    MessageStart {
        id: MessageId,
        role: Role,
        partial: Message,
    },
    MessageUpdate {
        id: MessageId,
        delta: MessageDelta,
        partial: Message,
    },
    MessageEnd {
        id: MessageId,
        message: Message,
        interrupted: bool,
    },
    ToolStart {
        id: ToolCallId,
        name: String,
        args: Value,
    },
    ToolUpdate {
        id: ToolCallId,
        partial_result: Value,
    },
    ToolEnd {
        id: ToolCallId,
        result: Value,
        is_error: bool,
    },
    Control(ControlAck),
    Error {
        error: AgentError,
    },
}

/// Monotone id source: ids are deterministic from emission order, so a replay
/// driven in order reproduces live ids.
#[derive(Debug, Default)]
pub struct IdAlloc {
    next: u64,
}

impl IdAlloc {
    pub fn alloc(&mut self) -> u64 {
        let id = self.next;
        self.next += 1;
        id
    }
}

/// The live-view reducer: folds one `AgentEvent` stream into what the TUI shows.
/// Partials are never authoritative — only terminal frames mutate state.
/// Replay drives this same reducer over the recorded vec.
#[derive(Debug, Default, PartialEq)]
pub struct Transcript {
    pub messages: Vec<Message>,
    pub tool_results: Vec<(ToolCallId, Value, bool)>,
}

impl Transcript {
    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::MessageEnd { message, .. } => self.messages.push(message.clone()),
            AgentEvent::ToolEnd {
                id,
                result,
                is_error,
            } => {
                self.tool_results
                    .push((id.clone(), result.clone(), *is_error));
            }
            // The terminal frame is authoritative for the whole transcript.
            AgentEvent::RunEnd { messages, .. } => self.messages.clone_from(messages),
            _ => {}
        }
    }

    pub fn replay(events: &[AgentEvent]) -> Self {
        let mut t = Self::default();
        for e in events {
            t.apply(e);
        }
        t
    }
}

/// Pairing invariant: every Start is eventually paired with an End.
/// One-directional — an End without a Start (e.g. a synthesized empty-stream
/// close) passes; a Start without an End fails.
pub fn check_pairing(events: &[AgentEvent]) -> bool {
    use std::collections::HashSet;
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

/// Crash-shape repair: append one synthesized terminal frame per Start that
/// never got its End, in first-seen order. Idempotent.
/// LOOP-OWNED RULE: the loop must durably append the crash facts before
/// emitting these terminals — this fn repairs the live shape, not the log.
pub fn synthesize_missing_ends(events: &mut Vec<AgentEvent>) {
    use std::collections::HashSet;
    let mut msg_seen: HashSet<MessageId> = HashSet::new();
    let mut tool_seen: HashSet<ToolCallId> = HashSet::new();
    let mut turn_seen: HashSet<TurnId> = HashSet::new();
    let mut msg_order: Vec<(MessageId, Role)> = Vec::new();
    let mut tool_order: Vec<ToolCallId> = Vec::new();
    let mut turn_order: Vec<TurnId> = Vec::new();
    let mut msg_ends: HashSet<MessageId> = HashSet::new();
    let mut tool_ends: HashSet<ToolCallId> = HashSet::new();
    let mut turn_ends: HashSet<TurnId> = HashSet::new();
    let mut run_starts = 0;
    let mut run_ends = 0;
    for e in events.iter() {
        match e {
            AgentEvent::RunStart { .. } => run_starts += 1,
            AgentEvent::RunEnd { .. } => run_ends += 1,
            AgentEvent::TurnStart { turn } => {
                if turn_seen.insert(*turn) {
                    turn_order.push(*turn);
                }
            }
            AgentEvent::TurnEnd { turn, .. } => {
                turn_ends.insert(*turn);
            }
            AgentEvent::MessageStart { id, role, .. } => {
                if msg_seen.insert(*id) {
                    msg_order.push((*id, *role));
                }
            }
            AgentEvent::MessageEnd { id, .. } => {
                msg_ends.insert(*id);
            }
            AgentEvent::ToolStart { id, .. } => {
                if tool_seen.insert(id.clone()) {
                    tool_order.push(id.clone());
                }
            }
            AgentEvent::ToolEnd { id, .. } => {
                tool_ends.insert(id.clone());
            }
            _ => {}
        }
    }
    for (id, role) in msg_order {
        if !msg_ends.contains(&id) {
            events.push(AgentEvent::MessageEnd {
                id,
                message: Message {
                    role,
                    content: String::new(),
                },
                interrupted: true,
            });
        }
    }
    for id in tool_order {
        if !tool_ends.contains(&id) {
            events.push(AgentEvent::ToolEnd {
                id,
                result: Value::Null,
                is_error: true,
            });
        }
    }
    for turn in turn_order {
        if !turn_ends.contains(&turn) {
            events.push(AgentEvent::TurnEnd {
                turn,
                reason: TurnEndReason::Aborted,
            });
        }
    }
    if run_starts > run_ends {
        events.push(AgentEvent::RunEnd {
            outcome: RunOutcome::Failed("run ended without RunEnd; synthesized".to_string()),
            messages: Vec::new(),
        });
    }
}

fn call_catch(listener: &dyn Fn(&AgentEvent), event: &AgentEvent) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(event))).is_err()
}

/// One emission seam for live and replay: an ordered record plus fan-out.
/// Listeners are sync callbacks — never awaited, so a hung TUI cannot hang a
/// headless run. By-value clones per tap; `Arc` only if profiling says so.
#[derive(Default)]
pub struct Emitter {
    history: Vec<AgentEvent>,
    #[allow(clippy::type_complexity)]
    listeners: Vec<Box<dyn Fn(&AgentEvent) + Send>>,
    taps: Vec<std::sync::mpsc::SyncSender<AgentEvent>>,
}

impl Emitter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on(&mut self, listener: impl Fn(&AgentEvent) + Send + 'static) {
        self.listeners.push(Box::new(listener));
    }

    // ponytail: bounded sync_channel + try_send never blocks the loop; a full or dead tap drops newest silently while history() stays the lossless record — host sizes capacity, add backpressure only when a consumer needs lossless live delivery.
    pub fn tap(&mut self, capacity: usize) -> std::sync::mpsc::Receiver<AgentEvent> {
        let (tx, rx) = std::sync::mpsc::sync_channel(capacity.max(1));
        self.taps.push(tx);
        rx
    }

    /// Ordered, non-blocking delivery. LOOP-OWNED RULE: the loop must append
    /// the durable fact before emitting the terminal frame — this only records
    /// emission order, it is not the log.
    pub fn emit(&mut self, event: AgentEvent) {
        self.emit_inner(event, false);
    }

    fn emit_inner(&mut self, event: AgentEvent, is_error_delivery: bool) {
        self.history.push(event);
        let last = self.history.len() - 1;
        let mut failed = false;
        for l in &self.listeners {
            if call_catch(l, &self.history[last]) {
                failed = true;
            }
        }
        self.taps.retain(|t| {
            !matches!(
                t.try_send(self.history[last].clone()),
                Err(std::sync::mpsc::TrySendError::Disconnected(_))
            )
        });
        if failed && !is_error_delivery {
            self.emit_inner(
                AgentEvent::Error {
                    error: AgentError {
                        code: "listener-panic".to_string(),
                        message: "a listener panicked handling an event".to_string(),
                    },
                },
                true,
            );
        }
        // Panics during error delivery are swallowed: the recursion guard.
    }

    pub fn history(&self) -> &[AgentEvent] {
        &self.history
    }

    /// Forks never inherit subscribers: a forked run gets a fresh seam.
    pub fn fork(&self) -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn all_variants() -> Vec<AgentEvent> {
        vec![
            AgentEvent::RunStart {
                run_id: 0,
                goal: "g".to_string(),
            },
            AgentEvent::RunEnd {
                outcome: RunOutcome::Passed,
                messages: vec![Message {
                    role: Role::User,
                    content: "hi".to_string(),
                }],
            },
            AgentEvent::TurnStart { turn: 0 },
            AgentEvent::TurnEnd {
                turn: 0,
                reason: TurnEndReason::Completed,
            },
            AgentEvent::MessageStart {
                id: 0,
                role: Role::Assistant,
                partial: Message {
                    role: Role::Assistant,
                    content: String::new(),
                },
            },
            AgentEvent::MessageUpdate {
                id: 0,
                delta: MessageDelta {
                    kind: DeltaKind::Text,
                    text: Some("hi".to_string()),
                },
                partial: Message {
                    role: Role::Assistant,
                    content: "hi".to_string(),
                },
            },
            AgentEvent::MessageEnd {
                id: 0,
                message: Message {
                    role: Role::Assistant,
                    content: "hi".to_string(),
                },
                interrupted: false,
            },
            AgentEvent::ToolStart {
                id: "c1".to_string(),
                name: "read".to_string(),
                args: json!({}),
            },
            AgentEvent::ToolUpdate {
                id: "c1".to_string(),
                partial_result: json!("par"),
            },
            AgentEvent::ToolEnd {
                id: "c1".to_string(),
                result: json!("ok"),
                is_error: false,
            },
            AgentEvent::Control(ControlAck {
                id: 1,
                kind: ControlKind::Stop,
                status: ControlStatus::Applied,
                note: "n".to_string(),
            }),
            AgentEvent::Error {
                error: AgentError {
                    code: "E".to_string(),
                    message: "m".to_string(),
                },
            },
        ]
    }

    #[test]
    fn serde_round_trip_all_variants() {
        for ev in all_variants() {
            let s = serde_json::to_string(&ev).unwrap();
            assert!(
                s.contains("\"type\":"),
                "flat tagged enum must carry a type tag: {s}"
            );
            let back: AgentEvent = serde_json::from_str(&s).unwrap();
            assert_eq!(ev, back);
        }
    }

    #[test]
    fn pairing_invariant_and_synthesis() {
        let mut good = vec![
            AgentEvent::RunStart {
                run_id: 0,
                goal: "g".to_string(),
            },
            AgentEvent::TurnStart { turn: 0 },
            AgentEvent::MessageStart {
                id: 0,
                role: Role::Assistant,
                partial: Message {
                    role: Role::Assistant,
                    content: "hi".to_string(),
                },
            },
            AgentEvent::MessageEnd {
                id: 0,
                message: Message {
                    role: Role::Assistant,
                    content: "hi".to_string(),
                },
                interrupted: false,
            },
            AgentEvent::ToolStart {
                id: "c1".to_string(),
                name: "read".to_string(),
                args: json!({}),
            },
            AgentEvent::ToolEnd {
                id: "c1".to_string(),
                result: json!("ok"),
                is_error: false,
            },
            AgentEvent::TurnEnd {
                turn: 0,
                reason: TurnEndReason::Completed,
            },
            AgentEvent::RunEnd {
                outcome: RunOutcome::Passed,
                messages: vec![],
            },
            AgentEvent::Control(ControlAck {
                id: 1,
                kind: ControlKind::Stop,
                status: ControlStatus::Applied,
                note: "n".to_string(),
            }),
            AgentEvent::Error {
                error: AgentError {
                    code: "E".to_string(),
                    message: "m".to_string(),
                },
            },
        ];
        assert!(check_pairing(&good));
        // One-directional: an End without a Start (synthesized empty-stream close) passes.
        assert!(check_pairing(&[AgentEvent::MessageEnd {
            id: 9,
            message: Message {
                role: Role::Assistant,
                content: String::new()
            },
            interrupted: true,
        }]));
        good.remove(7);
        assert!(!check_pairing(&good));
        let mut broken = vec![
            AgentEvent::RunStart {
                run_id: 0,
                goal: "g".to_string(),
            },
            AgentEvent::TurnStart { turn: 3 },
            AgentEvent::MessageStart {
                id: 7,
                role: Role::Assistant,
                partial: Message {
                    role: Role::Assistant,
                    content: "part".to_string(),
                },
            },
            AgentEvent::ToolStart {
                id: "c9".to_string(),
                name: "sh".to_string(),
                args: json!(null),
            },
        ];
        assert!(!check_pairing(&broken));
        synthesize_missing_ends(&mut broken);
        assert!(check_pairing(&broken));
        assert!(broken.iter().any(|e| matches!(
            e,
            AgentEvent::MessageEnd {
                id: 7,
                interrupted: true,
                ..
            }
        )));
        let len = broken.len();
        synthesize_missing_ends(&mut broken);
        assert_eq!(broken.len(), len);
    }

    #[test]
    fn replay_equals_live() {
        let live = vec![
            AgentEvent::RunStart {
                run_id: 0,
                goal: "g".to_string(),
            },
            AgentEvent::MessageStart {
                id: 0,
                role: Role::Assistant,
                partial: Message {
                    role: Role::Assistant,
                    content: String::new(),
                },
            },
            AgentEvent::MessageUpdate {
                id: 0,
                delta: MessageDelta {
                    kind: DeltaKind::Text,
                    text: Some("hello ".to_string()),
                },
                partial: Message {
                    role: Role::Assistant,
                    content: "hello ".to_string(),
                },
            },
            AgentEvent::MessageUpdate {
                id: 0,
                delta: MessageDelta {
                    kind: DeltaKind::Text,
                    text: Some("world".to_string()),
                },
                partial: Message {
                    role: Role::Assistant,
                    content: "hello world".to_string(),
                },
            },
            AgentEvent::MessageEnd {
                id: 0,
                message: Message {
                    role: Role::Assistant,
                    content: "hello world".to_string(),
                },
                interrupted: false,
            },
            AgentEvent::ToolStart {
                id: "c1".to_string(),
                name: "read".to_string(),
                args: json!({"p": "f"}),
            },
            AgentEvent::ToolUpdate {
                id: "c1".to_string(),
                partial_result: json!("par"),
            },
            AgentEvent::ToolEnd {
                id: "c1".to_string(),
                result: json!("full"),
                is_error: false,
            },
            AgentEvent::RunEnd {
                outcome: RunOutcome::Passed,
                messages: vec![Message {
                    role: Role::Assistant,
                    content: "hello world".to_string(),
                }],
            },
        ];
        let live_state = Transcript::replay(&live);
        let replayed: Vec<AgentEvent> = live
            .iter()
            .map(|e| serde_json::from_str(&serde_json::to_string(e).unwrap()).unwrap())
            .collect();
        assert_eq!(live_state, Transcript::replay(&replayed));
        assert_eq!(live_state.messages.len(), 1);
        assert_eq!(live_state.messages[0].content, "hello world");
        assert_eq!(live_state.tool_results.len(), 1);
    }

    #[test]
    fn listener_isolation_and_bounded_tap() {
        let mut bus = Emitter::new();
        let seen = Arc::new(Mutex::new(0u32));
        let s = seen.clone();
        bus.on(move |_| {
            *s.lock().unwrap() += 1;
        });
        bus.on(|_| panic!("boom"));
        let rx = bus.tap(8);
        bus.emit(AgentEvent::TurnStart { turn: 1 });
        // The panicking listener became one Error event; the guard held (no loop).
        assert_eq!(bus.history().len(), 2);
        assert!(matches!(bus.history()[1], AgentEvent::Error { .. }));
        assert_eq!(*seen.lock().unwrap(), 2);
        assert!(matches!(
            rx.try_recv().unwrap(),
            AgentEvent::TurnStart { .. }
        ));
        assert!(matches!(rx.try_recv().unwrap(), AgentEvent::Error { .. }));
        // A dead tap never errors the loop; a fork inherits no subscribers.
        drop(rx);
        bus.emit(AgentEvent::TurnEnd {
            turn: 1,
            reason: TurnEndReason::Aborted,
        });
        assert_eq!(bus.history().len(), 4);
        assert_eq!(*seen.lock().unwrap(), 4);
        let mut fork = bus.fork();
        assert!(fork.history().is_empty());
        fork.emit(AgentEvent::TurnStart { turn: 9 });
        assert_eq!(*seen.lock().unwrap(), 4);
    }
}
