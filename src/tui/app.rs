use std::collections::VecDeque;

use super::render::{render_line, replay_filter, Counters};
use crate::obs::{Boundary, GoalFinished, TraceEvent};

/// Where the one live goal run currently is. `Idle` is also the replay
/// posture, so a recorded trace never enters the run lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Idle,
    Running,
    Stopping,
    Finished,
    Failed,
}

impl RunMode {
    /// Status-bar wording; lowercase so the status line reads as a phrase.
    fn label(self) -> &'static str {
        match self {
            RunMode::Idle => "idle",
            RunMode::Running => "running",
            RunMode::Stopping => "stopping",
            RunMode::Finished => "finished",
            RunMode::Failed => "failed",
        }
    }
}

/// Caps for the live activity deque: past either limit the oldest lines
/// are evicted. These bound `activity` only — `transcript` keeps every
/// line for the whole session, so `App` memory is not capped here.
const ACTIVITY_MAX_LINES: usize = 200;
const ACTIVITY_MAX_CHARS: usize = 32_000;

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
    /// Live run lifecycle; `Idle` for replay and for a fresh console.
    pub run_mode: RunMode,
    /// The goal text of the current/last live run.
    pub run_goal: String,
    /// One concise outcome line for the finished run, `None` until an
    /// outcome arrives.
    pub run_outcome: Option<String>,
    /// Bounded run activity, oldest first; the live-only twin of
    /// `transcript`. Public for the renderer, but only the `App` reducer
    /// (`on_event` / `begin_run`) may mutate it, because it is coupled to
    /// `activity_chars` and the two must not drift apart.
    pub activity: VecDeque<String>,
    /// Sum of the char counts of `activity`'s lines, so eviction never
    /// rescans the deque. Mutated only through the reducer, in step with
    /// `activity`.
    pub activity_chars: usize,
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
            run_mode: RunMode::Idle,
            run_goal: String::new(),
            run_outcome: None,
            activity: VecDeque::new(),
            activity_chars: 0,
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
        // One render per event: the transcript line and the live activity
        // line are the same string, so the caps are measured on real text.
        let line = render_line(ev);
        self.transcript.push(line.clone());
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
        if matches!(self.run_mode, RunMode::Running | RunMode::Stopping) {
            self.push_activity_line(line);
        }
    }

    /// Append one activity line, truncating a single runaway line and
    /// evicting from the front while either cap is exceeded.
    fn push_activity_line(&mut self, line: String) {
        let line: String = if line.chars().count() > ACTIVITY_MAX_CHARS {
            line.chars().take(ACTIVITY_MAX_CHARS).collect()
        } else {
            line
        };
        self.activity_chars += line.chars().count();
        self.activity.push_back(line);
        while self.activity.len() > ACTIVITY_MAX_LINES || self.activity_chars > ACTIVITY_MAX_CHARS {
            if let Some(old) = self.activity.pop_front() {
                self.activity_chars -= old.chars().count();
            } else {
                self.activity_chars = 0;
                break;
            }
        }
        // The running total is the whole point of `activity_chars`; if the
        // deque and the counter can disagree, eviction silently drifts.
        debug_assert_eq!(
            self.activity_chars,
            self.activity
                .iter()
                .map(|l| l.chars().count())
                .sum::<usize>()
        );
    }

    /// The most recent `height` activity lines, oldest first, so a render
    /// region can show a bounded tail without copying the whole deque.
    pub fn activity_tail(&self, height: usize) -> Vec<String> {
        let skip = self.activity.len().saturating_sub(height);
        self.activity.iter().skip(skip).cloned().collect()
    }

    /// Start a live run: a new goal clears the previous run's activity and
    /// outcome, but never the transcript.
    pub fn begin_run(&mut self, goal: &str) {
        self.activity.clear();
        self.activity_chars = 0;
        self.run_outcome = None;
        self.run_goal = goal.to_string();
        self.run_mode = RunMode::Running;
    }

    /// Arm a stop request. Only a running run can become `Stopping`; any
    /// other mode is left alone (a goal that already ended cannot stop).
    pub fn set_stopping(&mut self) {
        if self.run_mode == RunMode::Running {
            self.run_mode = RunMode::Stopping;
        }
    }

    /// A run boundary notification. `Started` marks the run live;
    /// `Finished` alone does not decide the outcome, which arrives with
    /// `GoalFinished`. A resolved run is terminal, so a late `Started`
    /// from the previous goal cannot reopen it as running. The caller must
    /// open a new run with [`Self::begin_run`] before its `Started`
    /// boundary arrives.
    pub fn on_live_boundary(&mut self, boundary: Boundary) {
        match boundary {
            Boundary::Started => {
                if !matches!(self.run_mode, RunMode::Finished | RunMode::Failed) {
                    self.run_mode = RunMode::Running;
                }
            }
            Boundary::Finished => {}
        }
    }

    /// The terminal outcome of the run: one concise line stored as the
    /// outcome, appended to the transcript, and mapped to the final mode.
    pub fn on_live_finished(&mut self, finished: &GoalFinished) {
        let line = match (&finished.passed, &finished.error) {
            (true, _) => "run finished: passed".to_string(),
            (false, Some(error)) => format!("run failed: {error}"),
            (false, None) => "run failed".to_string(),
        };
        self.run_outcome = Some(line.clone());
        self.transcript.push(line);
        self.run_mode = if finished.passed {
            RunMode::Finished
        } else {
            RunMode::Failed
        };
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
        // The run posture is shown only outside `Idle`, which keeps the
        // replay status line byte-identical to before.
        let run = if self.run_mode == RunMode::Idle {
            String::new()
        } else {
            format!("{} · ", self.run_mode.label())
        };
        if self.replay_mode {
            format!(
                "{run}{counters} · replay {}/{} [{}] · help: j/k move · g/G ends · / filter · q quit",
                if self.replay_events.is_empty() {
                    0
                } else {
                    self.replay_idx + 1
                },
                self.replay_events.len(),
                self.replay_filter
            )
        } else {
            format!("{run}{counters}")
        }
    }
}
