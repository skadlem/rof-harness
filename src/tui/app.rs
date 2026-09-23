use super::render::{render_line, Counters};
use crate::obs::TraceEvent;

/// UI state for the `rof chat` console: transcript lines, running counters,
/// the composer's input buffer, and scroll. Mutated only by `on_event` /
/// `on_key`, so the whole state machine is testable without a terminal
/// (`run.rs` owns the terminal; Plan C feeds this live).
#[derive(Debug, Default)]
pub struct App {
    pub transcript: Vec<String>,
    pub counters: Counters,
    pub input: String,
    pub scroll: usize,
}

impl App {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sole writer of transcript + counters. Plan C calls this live; replay
    /// mode calls this once per trace line.
    pub fn on_event(&mut self, ev: &TraceEvent) {
        self.transcript.push(render_line(ev));
        match ev {
            TraceEvent::ModelCall {
                input_tokens,
                output_tokens,
                cost_usd,
                ..
            } => {
                self.counters.model_calls += 1;
                self.counters.in_tokens += input_tokens;
                self.counters.out_tokens += output_tokens;
                self.counters.cost_usd += cost_usd.unwrap_or(0.0);
            }
            TraceEvent::ReviewVerdict { pass, .. } => {
                if *pass {
                    self.counters.pass += 1;
                } else {
                    self.counters.fail += 1;
                }
            }
            _ => {}
        }
    }

    pub fn status_line(&self) -> String {
        let c = &self.counters;
        format!(
            "calls={} in={} out={} pass={} fail={}",
            c.model_calls, c.in_tokens, c.out_tokens, c.pass, c.fail
        )
    }
}
