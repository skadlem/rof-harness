use super::render::{render_line, replay_filter, Counters};
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
    /// Thinking posture for the composer's title accent. Read once from
    /// `ROF_THINKING` at startup by `run.rs`, never at render time: `draw`
    /// is a pure function of `App`.
    pub thinking: String,
    /// When true the composer renders bullets instead of input (key entry).
    /// Set while a `/login` key capture is pending, cleared on submit.
    pub mask_input: bool,
    /// Replay cursor, measured as an event index (the end is the default).
    pub replay_idx: usize,
    /// Case-insensitive replay search text; empty means show all events.
    pub replay_filter: String,
    /// Events loaded for seekable replay. Live chat leaves this empty.
    pub replay_events: Vec<TraceEvent>,
    /// Lenient markers retained for malformed JSONL lines.
    pub replay_unknown: Vec<String>,
    pub replay_mode: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            transcript: Vec::new(),
            counters: Counters::default(),
            input: String::new(),
            scroll: 0,
            fresh: true,
            thinking: String::new(),
            mask_input: false,
            replay_idx: 0,
            replay_filter: String::new(),
            replay_events: Vec::new(),
            replay_unknown: Vec::new(),
            replay_mode: false,
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

    /// Load the complete trace and display through its final event.
    pub fn set_replay_events(&mut self, events: Vec<TraceEvent>) {
        self.replay_events = events;
        self.replay_unknown.clear();
        self.replay_mode = true;
        self.replay_idx = self.replay_events.len().saturating_sub(1);
        self.replay_filter.clear();
        // Replay has no live startup splash; show its help/status immediately.
        self.fresh = false;
        self.refresh_replay();
    }

    /// Move the replay cursor, clamped to the loaded event range.
    pub fn replay_step(&mut self, delta: isize) {
        if self.replay_events.is_empty() {
            self.replay_idx = 0;
            return;
        }
        let max = self.replay_events.len() as isize - 1;
        self.replay_idx = (self.replay_idx as isize)
            .saturating_add(delta)
            .clamp(0, max) as usize;
        self.refresh_replay();
    }

    pub fn set_replay_unknown(&mut self, lines: Vec<String>) {
        self.replay_unknown = lines;
        self.refresh_replay();
    }

    pub fn replay_start(&mut self) {
        self.replay_idx = 0;
        self.refresh_replay();
    }

    pub fn replay_end(&mut self) {
        self.replay_idx = self.replay_events.len().saturating_sub(1);
        self.refresh_replay();
    }

    /// Set the replay search. Matching events are shown only through the
    /// current cursor, so stepping remains a chronological seek operation.
    pub fn set_replay_filter(&mut self, query: &str) {
        self.replay_filter = query.trim().to_string();
        self.refresh_replay();
    }

    fn refresh_replay(&mut self) {
        if !self.replay_mode {
            return;
        }
        self.transcript.clear();
        self.counters = Counters::fold(&self.replay_events);
        let matching = replay_filter(&self.replay_events, &self.replay_filter);
        for index in matching
            .into_iter()
            .filter(|index| *index <= self.replay_idx)
        {
            self.transcript
                .push(render_line(&self.replay_events[index]));
        }
        if self.replay_filter.is_empty() {
            self.transcript.extend(self.replay_unknown.iter().cloned());
        }
        self.scroll = 0;
    }

    pub fn status_line(&self) -> String {
        let c = &self.counters;
        let counters = format!(
            "calls={} in={} out={} pass={} fail={}",
            c.model_calls, c.in_tokens, c.out_tokens, c.pass, c.fail
        );
        if self.replay_mode {
            format!(
                "{} · replay {}/{} [{}] · help: j/k move · g/G ends · / filter · q quit",
                counters,
                if self.replay_events.is_empty() {
                    0
                } else {
                    self.replay_idx + 1
                },
                self.replay_events.len(),
                self.replay_filter
            )
        } else {
            counters
        }
    }
}
