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

/// Live token/cost report: one settle's usage (`MessageEnd.usage`) or the run
/// totals so far (`TurnEnd.usage_totals`). `reasoning_tokens`/`cost_usd`
/// mirror provider-core `Usage`: `None` = the provider reported nothing,
/// never a coerced zero. Live-event-only — the durable log vocabulary is
/// unchanged.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageReport {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub reasoning_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

/// The live vocabulary. Flat, tagged, serializable: one enum, one emission seam.
/// Partials are never authoritative; the terminal frame is mandatory.
///
/// `Eq` is absent because `UsageReport.cost_usd` is an `f64`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
        /// Cumulative over the run so far, not just this turn.
        #[serde(default)]
        usage_totals: UsageReport,
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
        /// This settle's provider usage; `None` = the provider reported none.
        usage: Option<UsageReport>,
    },
    ToolStart {
        id: ToolCallId,
        name: String,
        args: Value,
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

/// Pairing invariant: every Start is eventually paired with an End.
/// One-directional — an End without a Start passes; a Start without an End
/// fails.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn report() -> UsageReport {
        UsageReport {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 2,
            reasoning_tokens: Some(1),
            cost_usd: Some(0.01),
        }
    }

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
                usage_totals: report(),
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
                usage: Some(report()),
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
    fn pairing_invariant() {
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
                usage: None,
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
                usage_totals: UsageReport::default(),
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
        // One-directional: an End without a Start passes.
        assert!(check_pairing(&[AgentEvent::MessageEnd {
            id: 9,
            message: Message {
                role: Role::Assistant,
                content: String::new()
            },
            interrupted: true,
            usage: None,
        }]));
        good.remove(7);
        assert!(!check_pairing(&good));
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
        // A dead tap never errors the loop.
        drop(rx);
        bus.emit(AgentEvent::TurnEnd {
            turn: 1,
            reason: TurnEndReason::Aborted,
            usage_totals: UsageReport::default(),
        });
        assert_eq!(bus.history().len(), 4);
        assert_eq!(*seen.lock().unwrap(), 4);
    }

    #[test]
    fn pre_usage_dump_lines_still_deserialize() {
        // Dumps written before the usage fields keep parsing: `usage` is an
        // Option (serde fills None) and `usage_totals` carries serde(default).
        let end: AgentEvent = serde_json::from_str(
            r#"{"type":"MessageEnd","id":1,"message":{"role":"Assistant","content":"hi"},"interrupted":false}"#,
        )
        .unwrap();
        assert!(matches!(end, AgentEvent::MessageEnd { usage: None, .. }));
        let turn: AgentEvent =
            serde_json::from_str(r#"{"type":"TurnEnd","turn":1,"reason":"Completed"}"#).unwrap();
        let AgentEvent::TurnEnd { usage_totals, .. } = turn else {
            panic!("expected TurnEnd");
        };
        assert_eq!(usage_totals, UsageReport::default());
    }
}
