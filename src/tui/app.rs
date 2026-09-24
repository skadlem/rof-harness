use super::render::{render_line, Counters};
use crate::obs::TraceEvent;

/// UI state for the `rof chat` console: transcript lines, running counters,
/// the composer's input buffer, and scroll. Mutated only by `on_event` /
/// `on_key`, so the whole state machine is testable without a terminal
/// (`run.rs` owns the terminal; Plan C feeds this live).
#[derive(Debug)]
pub struct App {
    pub transcript: Vec<String>,
    pub counters: Counters,
    pub input: String,
    pub scroll: usize,
    /// True until the first keypress; `run.rs` draws the splash overlay
    /// while set and consumes that keypress.
    pub fresh: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            transcript: Vec::new(),
            counters: Counters::default(),
            input: String::new(),
            scroll: 0,
            fresh: true,
        }
    }
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

    /// Scroll the transcript window: positive moves toward older lines,
    /// negative toward the tail. `scroll == 0` is tailed. Clamped to
    /// `0..=transcript.len()` so it never panics, viewport math included —
    /// `draw` clamps again against the visible height.
    pub fn scroll_lines(&mut self, delta: isize) {
        let max = self.transcript.len() as isize;
        self.scroll = (self.scroll as isize).saturating_add(delta).clamp(0, max) as usize;
    }

    pub fn status_line(&self) -> String {
        let c = &self.counters;
        format!(
            "calls={} in={} out={} pass={} fail={}",
            c.model_calls, c.in_tokens, c.out_tokens, c.pass, c.fail
        )
    }
}
