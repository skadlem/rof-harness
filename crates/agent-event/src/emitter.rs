use crate::{AgentError, AgentEvent};

fn call_catch(listener: &dyn Fn(&AgentEvent), event: &AgentEvent) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(event))).is_err()
}

/// One emission seam for live and replay: an ordered record plus fan-out.
/// Listeners are sync callbacks — never awaited, so a hung TUI cannot hang a
/// headless run. By-value clones per listener; `Arc` only if profiling says so.
#[derive(Default)]
pub struct Emitter {
    history: Vec<AgentEvent>,
    #[allow(clippy::type_complexity)]
    listeners: Vec<Box<dyn Fn(&AgentEvent) + Send>>,
}

impl Emitter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on(&mut self, listener: impl Fn(&AgentEvent) + Send + 'static) {
        self.listeners.push(Box::new(listener));
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
