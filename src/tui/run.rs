//! Terminal runner for `rof chat`: alternate-screen event pump that owns a
//! whole live session (idle prompt plus one running goal) plus a
//! `--replay <trace.jsonl>` mode that renders a recorded trace with no live
//! loop. This file never touches the orchestrator: the goal itself runs in
//! a worker task supplied by the caller.

use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{backend::CrosstermBackend, Frame, Terminal};

use super::app::{App, BusyMode};
use super::cmd::Action;
use super::splash;
use super::ui::draw;
use crate::engine::control::RunCommand;
use crate::obs::{ControlKind, GoalFinished, LiveEvent, TraceEvent};

/// The goal worker task: one live run, resolving to its terminal outcome.
/// The session never reads the `JoinHandle` result synchronously: the
/// authoritative outcome is the `LiveEvent::Finished` the worker
/// publishes, and a finished handle with no such event yields the generic
/// fallback below rather than the handle's own error.
pub type WorkerHandle = tokio::task::JoinHandle<anyhow::Result<GoalFinished>>;

/// The live half of the terminal pump: owns the worker's event channel, the
/// worker handle, and the stop latch. It is a pure reducer over
/// `LiveEvent` -> `App`, with no terminal, clock, or file access, so the
/// whole live state machine is testable headlessly.
pub struct LiveSession {
    receiver: tokio::sync::mpsc::UnboundedReceiver<LiveEvent>,
    /// The write side of the command channel for the goal that is next to
    /// run. The session holds it for as long as the worker can, so the
    /// worker's inbox is never closed by a dropped sender: a `RunControl`
    /// that sees a closed channel cannot tell an empty inbox from a
    /// console that went away. Nothing writes here yet — command submission
    /// is the next task — but the field is the session's, not the starter's,
    /// for exactly that reason. `take_command_inbox` replaces it together
    /// with the receiver when a later goal needs a fresh pair.
    command_tx: tokio::sync::mpsc::UnboundedSender<RunCommand>,
    /// The receiver half of the command channel that no worker has claimed
    /// yet. `None` once a worker owns it, which is what makes the next
    /// claim mint a fresh pair.
    command_rx: Option<tokio::sync::mpsc::UnboundedReceiver<RunCommand>>,
    worker: Option<WorkerHandle>,
    stop_requested: bool,
}

impl LiveSession {
    /// Take ownership of the channel a goal worker publishes to. The session
    /// creates the command channel itself and keeps both halves until a
    /// worker claims the receiver, so no caller can hand out a receiver that
    /// a dead task owned. The event channel outlives any single run; `reset`
    /// prepares it for the next.
    pub fn new(receiver: tokio::sync::mpsc::UnboundedReceiver<LiveEvent>) -> Self {
        let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            receiver,
            command_tx,
            command_rx: Some(command_rx),
            worker: None,
            stop_requested: false,
        }
    }

    /// The write side of the command channel for the next goal. A console
    /// submission goes here; it reaches whichever worker holds the matching
    /// inbox.
    pub fn command_sender(&self) -> &tokio::sync::mpsc::UnboundedSender<RunCommand> {
        &self.command_tx
    }

    /// Hand the next goal worker the command inbox it will drain for the
    /// life of its task, and return the sender for the channel that inbox
    /// belongs to.
    ///
    /// The first call returns the receiver created in `new` — with the
    /// channel already open, so anything the console submitted while idle is
    /// still queued on it. Every later call mints a fresh pair, replaces the
    /// session's write side with the new sender, and returns the new
    /// receiver: the inbox a finished worker owned is unreachable from here,
    /// so a submission can never be written onto a channel nobody reads.
    pub fn take_command_inbox(&mut self) -> tokio::sync::mpsc::UnboundedReceiver<RunCommand> {
        match self.command_rx.take() {
            Some(rx) => rx,
            None => {
                let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
                self.command_tx = command_tx;
                command_rx
            }
        }
    }

    /// Begin a run, keeping the worker present until the session-terminal
    /// `LiveEvent::Finished`. `Control` and `GoalFinished` are per-goal
    /// and per-command, so they route to their own reducers and leave the
    /// handle alone: a queued goal keeps the same worker task alive after
    /// one `GoalFinished`.
    pub fn begin(&mut self, _goal: &str, worker: WorkerHandle) {
        self.worker = Some(worker);
        self.stop_requested = false;
    }

    /// True while a worker is admitted and has not yet been resolved by a
    /// terminal outcome.
    pub fn is_running(&self) -> bool {
        self.worker.is_some()
    }

    /// Ask the running goal to stop. This only sets the latch — the pump
    /// decides what to do with it; the worker is never aborted, so a stop
    /// cannot corrupt the trace the worker is still writing.
    pub fn request_stop(&mut self) {
        self.stop_requested = true;
    }

    pub fn stop_requested(&self) -> bool {
        self.stop_requested
    }

    /// Return to the idle posture: forget the worker, clear the stop latch,
    /// and discard events left over from the finished goal. The channel
    /// stays open so the next goal can publish on it.
    ///
    /// Only valid after `drain` has reported the terminal outcome. While a
    /// worker handle is still present this is a no-op, even if the task
    /// happens to be finished: draining here could discard a queued
    /// `LiveEvent::Finished` and strand the run before the outcome is
    /// applied.
    pub fn reset(&mut self) {
        if self.worker.is_some() {
            return;
        }
        self.worker = None;
        self.stop_requested = false;
        while self.receiver.try_recv().is_ok() {}
    }

    /// Apply every available live event to `app` and report a terminal
    /// outcome at most once. Returns `None` while the run is live, and
    /// `Some(GoalFinished)` on the single drain that carries the outcome.
    ///
    /// A worker that finished without publishing `LiveEvent::Finished` (a
    /// panic, a cancelled task, a worker error) is still terminal: it is
    /// reported as a failed outcome rather than leaving `App` running
    /// forever.
    pub fn drain(&mut self, app: &mut App) -> Option<GoalFinished> {
        let mut outcome = None;
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                LiveEvent::Trace(event) => app.on_event(&event),
                LiveEvent::Boundary(boundary) => app.on_live_boundary(boundary),
                LiveEvent::Control(ack) => app.on_control_ack(ack),
                LiveEvent::GoalFinished(finished) => app.on_goal_finished(&finished),
                LiveEvent::Finished(finished) => outcome = Some(finished),
            }
        }
        if outcome.is_none() {
            if let Some(worker) = self.worker.as_ref() {
                if worker.is_finished() {
                    outcome = Some(GoalFinished {
                        passed: false,
                        error: Some("worker exited without a terminal outcome".to_string()),
                    });
                }
            }
        }
        if let Some(finished) = &outcome {
            app.on_live_finished(finished);
            // The goal is over: forget the handle so a later drain cannot
            // report a second outcome for the same run, and clear the stop
            // latch so a resolved run cannot report a stale stop.
            self.worker = None;
            self.stop_requested = false;
        }
        outcome
    }
}

/// What one key means while a goal is live. The pump keeps ownership of
/// ordinary character input and of the stop latch; this enum covers only
/// the decisions that must not be reachable from a running session, which
/// is what makes the running contract testable without a PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunningKeyOutcome {
    /// Enter while a goal is live: submit the draft. This reducer only
    /// reports the submission — [`submit_running_input`] is what sends the
    /// command, occupies the slot, and clears the draft, because that
    /// decision needs the command channel this key reducer does not have.
    Submit,
    /// `q`/Esc on an empty composer: ask the running goal to stop. The
    /// worker is asked, never aborted, so it still writes its trace, and
    /// a second press detaches it instead of waiting for an
    /// uninterruptible call. Modifiers are the pump's business, not
    /// this reducer's: it sees a bare `KeyCode`, so it cannot tell
    /// Ctrl-C from typing `c` or Ctrl-q from typing `q`, and the pump
    /// matches both before calling it.
    StopArmed,
    /// Everything the pump handles itself: typing, Backspace, scrolling.
    Ignored,
}

/// The running-mode key contract, as a pure `App` reducer. Nothing is
/// parsed or dispatched here, and no run lifecycle moves: a live goal owns
/// the console, so Enter only reports that the composer line should be
/// submitted and the pump hands that line to [`submit_running_input`],
/// which is the only place a submission can become a command.
pub fn handle_running_key(app: &mut App, code: KeyCode) -> RunningKeyOutcome {
    match code {
        KeyCode::Enter => RunningKeyOutcome::Submit,
        KeyCode::Esc => {
            app.set_stopping();
            RunningKeyOutcome::StopArmed
        }
        // `q` quits only on an empty composer, so goal text containing `q`
        // stays typeable (the idle prompt keeps the same rule).
        KeyCode::Char('q') if app.input.is_empty() => {
            app.set_stopping();
            RunningKeyOutcome::StopArmed
        }
        _ => RunningKeyOutcome::Ignored,
    }
}

/// What one composer submission did while a goal is live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunningSubmit {
    /// The command reached the worker's channel and the matching slot is
    /// occupied under its id, awaiting the worker's acknowledgement.
    Sent(ControlKind),
    /// Held for a later boundary rather than sent now. No composer
    /// submission produces this yet: the boundary-time consumer lands with
    /// the pump wiring.
    Deferred,
    /// Not sent, with the reason to show. The draft is kept intact, so a
    /// rejected line can be corrected and submitted again.
    Rejected(String),
    /// Not a submission: the draft held no text. Nothing was spent, sent,
    /// or cleared.
    Ignored,
}

/// Submit the composer line while a goal is live, as a pure reducer over
/// `App` plus the one channel it writes to.
///
/// What the busy mode means decides everything here: `Steer` sends a
/// [`RunCommand::Steer`], `Queue` a [`RunCommand::QueueGoal`], and
/// `Interrupt` refuses the text and points at the stop keys. The draft is
/// cleared only when a command actually reached the channel, and a failed
/// send frees the slot it had occupied — a command the worker never saw is
/// never acknowledged, so a slot left holding it would report the control
/// as pending for the rest of the run.
///
/// A slash line is refused rather than sent: a command must never ride the
/// channel as steer text. The pump routes slash lines to the console
/// command path, so the draft is kept and the reason returned.
pub fn submit_running_input(
    app: &mut App,
    commands: &tokio::sync::mpsc::UnboundedSender<RunCommand>,
    text: &str,
) -> RunningSubmit {
    let text = text.trim();
    if text.is_empty() {
        return RunningSubmit::Ignored;
    }
    if text.starts_with('/') {
        return RunningSubmit::Rejected(
            "/ lines are console commands, not steer text — the draft was kept".to_string(),
        );
    }
    let (kind, id) = match app.busy_mode {
        BusyMode::Steer => {
            let id = app.submit_pending_steer(text);
            (ControlKind::Steer, id)
        }
        BusyMode::Queue => {
            let id = app.submit_pending_goal(text);
            (ControlKind::Queue, id)
        }
        // Interrupt is the stop path, not a text path: a goal in flight is
        // stopped with a stop key, never with steer text.
        BusyMode::Interrupt => {
            return RunningSubmit::Rejected(
                "interrupt mode takes no text — press q/Esc/Ctrl-C to stop the run".to_string(),
            );
        }
    };
    let command = match kind {
        ControlKind::Steer => RunCommand::Steer {
            id,
            text: text.to_string(),
        },
        _ => RunCommand::QueueGoal {
            id,
            goal: text.to_string(),
        },
    };
    if let Err(error) = commands.send(command) {
        // The worker will never acknowledge a command it never received, so
        // the slot has to go back or the console waits on it forever. The
        // draft survives: the submission failed, not the text.
        app.release_control_slot(kind, id);
        return RunningSubmit::Rejected(format!("command not sent: {error}"));
    }
    app.transcript.push(match kind {
        ControlKind::Steer => format!("steer pending ({id})"),
        _ => format!("goal queued ({id})"),
    });
    app.input.clear();
    RunningSubmit::Sent(kind)
}

/// Live session: one alternate-screen pump owns the whole console. Idle
/// Enter starts a goal in a worker task through the injected starter, and
/// the pump keeps drawing and reading keys while it runs. `start_goal`
/// is the only seam that knows how to build a worker, so this file never
/// touches the orchestrator.
///
/// While a goal is live, `q`/Esc/Ctrl-C are stop keys with two
/// presses: the first asks the worker to stop (the in-flight call
/// cannot be interrupted), the second detaches it and ends the
/// session. The worker is dropped, never aborted, so the trace it is
/// still writing stays intact.
///
/// The live sender is attached to the durable sink for the whole session
/// and detached on every return path, so `TraceSink` and the channel
/// cannot drift out of the loop's reach on an error exit.
pub fn run_live<F>(trace: &crate::obs::TraceSink, mut start_goal: F) -> anyhow::Result<()>
where
    F: FnMut(
        String,
        tokio::sync::mpsc::UnboundedSender<LiveEvent>,
        tokio::sync::mpsc::UnboundedReceiver<RunCommand>,
    ) -> anyhow::Result<WorkerHandle>,
{
    enable_raw_mode()?;
    std::io::stdout().execute(EnterAlternateScreen)?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    // The command channel is the session's: it creates one pair in `new`
    // and mints a fresh one for every later goal (the receiver dies with
    // the task that drains it). This function only owns the live event
    // channel, which every goal in the console shares.
    trace.attach_live(tx.clone());
    let out = run_live_inner(trace, rx, &tx, &mut start_goal);
    trace.detach_live();
    disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;
    out
}

/// The pump itself: owns `App`, the `LiveSession`, and the idle-only
/// `/login` key capture. One goal at a time — Enter starts a goal
/// immediately and the run is watched in place; a goal the worker
/// continues after a queued one never comes back through here, because
/// that is the same worker task.
fn run_live_inner<F>(
    trace: &crate::obs::TraceSink,
    rx: tokio::sync::mpsc::UnboundedReceiver<LiveEvent>,
    tx: &tokio::sync::mpsc::UnboundedSender<LiveEvent>,
    start_goal: &mut F,
) -> anyhow::Result<()>
where
    F: FnMut(
        String,
        tokio::sync::mpsc::UnboundedSender<LiveEvent>,
        tokio::sync::mpsc::UnboundedReceiver<RunCommand>,
    ) -> anyhow::Result<WorkerHandle>,
{
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new();
    // The posture accent is startup state, not render state: read once here
    // so `draw` stays a pure function of `App`.
    app.thinking = std::env::var("ROF_THINKING").unwrap_or_default();
    app.transcript
        .push("rof chat — type a goal, or /help for commands.".to_string());
    let mut session = LiveSession::new(rx);
    let mut awaiting_key: Option<String> = None;
    // Double-press quit while idle: a goal is never preempted from the
    // idle prompt either, so the first press only arms (with an
    // indicator line naming that fact) and the second press exits. Any
    // other key disarms.
    let mut quit_armed = false;
    // A stopped run that reached its outcome ends the session, but the
    // outcome line is the whole point of the stop, so the return waits
    // for the draw that shows it instead of pre-empting that frame.
    let mut exit_after_draw = false;
    loop {
        // Drain before every draw, with or without a key: the worker
        // publishes on its own schedule, not ours.
        let stop_armed = session.stop_requested();
        if session.drain(&mut app).is_some() {
            // Terminal: back to the idle posture, keeping the activity
            // pane. A stop requested before the outcome ends the session
            // now that the run is over.
            session.reset();
            exit_after_draw = stop_armed;
        }
        terminal.draw(|f| draw_frame(f, &app))?;
        if exit_after_draw {
            return Ok(());
        }
        if !event::poll(Duration::from_millis(33))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            // The splash consumes the first keypress outright.
            if app.fresh {
                app.fresh = false;
                continue;
            }
            if session.is_running() {
                // Running posture: the composer is a scratch pad and the
                // run is read-only. No slash action, no config mutation, no
                // second goal is reachable from this branch.
                //
                // Ctrl-C is matched on the modifier first, because a bare
                // `KeyCode` cannot tell it from typing `c`; the remaining
                // stop keys (`q`/Esc) go through the pure helper, guarded
                // by an empty modifier set so Shift/Alt/Ctrl+q stays draft
                // text.
                let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'));
                let stop_key = ctrl_c
                    || (key.modifiers.is_empty()
                        && handle_running_key(&mut app, key.code) == RunningKeyOutcome::StopArmed);
                // A stop key ends the key here: it must not also be typed
                // into the draft.
                if stop_key {
                    // Once a stop is requested, do not clear the latch on
                    // later typing: the user asked to stop, and a stray
                    // draft keystroke must not silently cancel that request.
                    if session.stop_requested() {
                        // Second press: the call cannot be interrupted, so
                        // the worker is detached rather than waited for.
                        // Dropping the handle never aborts the task, and
                        // whatever it has already written stays in the
                        // JSONL trace.
                        app.transcript.push(detach_hint());
                        terminal.draw(|f| draw_frame(f, &app))?;
                        return Ok(());
                    }
                    session.request_stop();
                    app.set_stopping();
                    app.transcript.push(stop_hint());
                    continue;
                }
                match key.code {
                    KeyCode::Char(c)
                        if key.modifiers.is_empty()
                            || key.modifiers == KeyModifiers::SHIFT
                            // A modified `q` is ordinary text, not a stop
                            // key, so it must still reach the draft.
                            || c == 'q' =>
                    {
                        app.input.push(c);
                    }
                    KeyCode::Backspace => {
                        app.input.pop();
                    }
                    KeyCode::Up => app.scroll_lines(1),
                    KeyCode::Down => app.scroll_lines(-1),
                    KeyCode::PageUp => app.scroll_lines(10),
                    KeyCode::PageDown => app.scroll_lines(-10),
                    KeyCode::Home => app.scroll_lines(isize::MAX),
                    KeyCode::End => app.scroll_lines(isize::MIN),
                    _ => {}
                }
                continue;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
            {
                if quit_armed {
                    return Ok(());
                }
                quit_armed = true;
                app.transcript.push(quit_hint());
                continue;
            }
            match key.code {
                KeyCode::Esc => {
                    if quit_armed {
                        return Ok(());
                    }
                    quit_armed = true;
                    app.transcript.push(quit_hint());
                }
                // `q` quits only on an empty composer so goal text containing
                // `q` stays typeable (run_repl quits on any `q`; the console
                // cannot afford that).
                KeyCode::Char('q') if key.modifiers.is_empty() && app.input.is_empty() => {
                    if quit_armed {
                        return Ok(());
                    }
                    quit_armed = true;
                    app.transcript.push(quit_hint());
                }
                KeyCode::Char(c)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    quit_armed = false;
                    app.input.push(c);
                }
                KeyCode::Backspace => {
                    quit_armed = false;
                    app.input.pop();
                }
                KeyCode::Up => app.scroll_lines(1),
                KeyCode::Down => app.scroll_lines(-1),
                KeyCode::PageUp => app.scroll_lines(10),
                KeyCode::PageDown => app.scroll_lines(-10),
                KeyCode::Home => app.scroll_lines(isize::MAX),
                KeyCode::End => app.scroll_lines(isize::MIN),
                KeyCode::Enter => {
                    quit_armed = false;
                    let text = std::mem::take(&mut app.input).trim().to_string();
                    if text.is_empty() {
                        continue;
                    }
                    // A pending /login key capture wins over starting a goal:
                    // the next non-slash line is the key (never echoed, never
                    // run as a goal). A slash line cancels it.
                    if let Some(prov) = awaiting_key.take() {
                        app.mask_input = false;
                        if text.starts_with('/') {
                            app.transcript
                                .push(format!("/login {prov} cancelled — key was not captured"));
                        } else {
                            match verify_blocking(&prov, text.trim()) {
                                Ok(ids) => {
                                    if let Err(e) = super::auth::store().save(&prov, text.trim()) {
                                        app.transcript
                                            .push(format!("login verified but save failed: {e}"));
                                    } else {
                                        app.transcript.push(format!(
                                            "logged in {prov} ({} models) — applies to the next goal",
                                            ids.len()
                                        ));
                                    }
                                }
                                Err(e) => app.transcript.push(format!("login failed: {e}")),
                            }
                            continue;
                        }
                    }
                    match super::cmd::parse(&text) {
                        None => {
                            // A non-slash line starts a goal immediately and
                            // is watched in place; a goal the worker
                            // continues after a queued one never comes back
                            // through here. The run state opens first so the
                            // worker's first event is already a `Running`
                            // activity line.
                            if session.is_running() {
                                continue;
                            }
                            app.begin_run(&text);
                            // The worker owns the command inbox for the
                            // life of its task, so the session hands out a
                            // fresh pair for every goal: the first claim
                            // takes the one it opened, and a later goal in
                            // the same console gets a new one because the
                            // old receiver died with its task. A queued goal
                            // never reaches here: that is the same worker
                            // task, continuing.
                            let command_rx = session.take_command_inbox();
                            match start_goal(text.clone(), tx.clone(), command_rx) {
                                Ok(handle) => session.begin(&text, handle),
                                // A starter that cannot spawn must not take
                                // the terminal with it: record the failure and
                                // stay idle.
                                Err(error) => {
                                    app.on_live_finished(&GoalFinished {
                                        passed: false,
                                        error: Some(format!("could not start the run: {error}")),
                                    });
                                    session.reset();
                                }
                            }
                        }
                        Some(action) => {
                            if apply_action(&mut app, trace, action, &text, &mut awaiting_key) {
                                return Ok(());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// One console frame: the startup splash until the first keypress, then
/// the live view. The pump and its force-exit share this, so the last
/// frame a user sees is drawn exactly like every other one.
fn draw_frame(f: &mut Frame, app: &App) {
    if app.fresh {
        splash::draw(f);
    } else {
        draw(f, app);
    }
}

fn quit_hint() -> String {
    "quit armed — press q/Esc/Ctrl-C again to exit".to_string()
}

/// First stop press on a live run: the worker is asked, not interrupted.
fn stop_hint() -> String {
    "stop requested — the current call cannot be interrupted; press q/Esc/Ctrl-C again to detach and exit"
        .to_string()
}

/// Second stop press: the run is abandoned, not aborted.
fn detach_hint() -> String {
    "detaching the worker — completed events remain in the JSONL trace; an in-flight tool may finish on its own timeout".to_string()
}

fn env_present(k: &str) -> bool {
    std::env::var(k)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

/// Run the async key check from the sync prompt pump. Inside `chat` this
/// runs on a tokio worker, so `block_in_place` hands the thread back while
/// the verify call is in flight; outside a runtime a throwaway
/// current-thread runtime does the same job.
fn verify_blocking(provider: &str, key: &str) -> Result<Vec<String>, String> {
    match tokio::runtime::Handle::try_current() {
        Ok(h) => tokio::task::block_in_place(|| h.block_on(super::auth::verify(provider, key))),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?
            .block_on(super::auth::verify(provider, key)),
    }
}

/// Apply a slash action between goals. Display-only actions render into the
/// transcript; knob actions set the same env names the loop reads at call
/// time; `/model` re-points the model vars and `/login` verifies + saves,
/// both taking effect on the next goal because main rebuilds the config
/// and the provider clients per goal. `raw` is the full composer line —
/// `parse()` keeps only the first token, so the two-token `/model ctx`
/// and `/login <prov> <key>` forms read from here. Returns true on quit.
fn apply_action(
    app: &mut App,
    trace: &crate::obs::TraceSink,
    action: Action,
    raw: &str,
    awaiting_key: &mut Option<String>,
) -> bool {
    match action {
        Action::Quit => return true,
        Action::Help => app.transcript.push(super::cmd::help_text()),
        Action::Unknown(msg) => {
            app.transcript.push(msg);
            app.transcript.push(super::cmd::help_text());
        }
        Action::Models => {
            let logged = super::auth::store().providers();
            let st = super::auth::statuses();
            if logged.is_empty() {
                app.transcript.push(
                    "models: no provider logins in the credentials store (env keys still apply at call time)"
                        .to_string(),
                );
            } else {
                for p in &logged {
                    let s = st
                        .get(p)
                        .cloned()
                        .unwrap_or_else(|| "unverified".to_string());
                    app.transcript.push(format!("models: {p} [store, {s}]"));
                }
            }
            let mut envp: Vec<String> = Vec::new();
            if env_present("OR_TOKEN") {
                envp.push("openrouter (env OR_TOKEN)".to_string());
            }
            if env_present("ROF_TOKEN") {
                let base = std::env::var("ROF_CHAT_BASE").unwrap_or_default();
                envp.push(if base.trim().is_empty() {
                    "compat (env ROF_TOKEN)".to_string()
                } else {
                    format!("compat (env ROF_TOKEN + {base})")
                });
            }
            if !envp.is_empty() {
                app.transcript
                    .push(format!("models: env: {}", envp.join(", ")));
            }
        }
        Action::Context => app.transcript.push(format!(
            "context: {} | transcript lines: {}",
            app.status_line(),
            app.transcript.len()
        )),
        Action::Trace => app
            .transcript
            .push(format!("trace events: {}", trace.len())),
        Action::Hotkeys => app
            .transcript
            .push("hotkeys: q/Esc/Ctrl-C (twice) quit · Enter send · Backspace delete".to_string()),
        Action::Diff => app
            .transcript
            .push("diff view is not available in this console yet".to_string()),
        Action::Attempts(n) => {
            std::env::set_var("ROF_ATTEMPTS", n.to_string());
            app.transcript.push(format!(
                "attempts={n} (ROF_ATTEMPTS, applies to the next goal)"
            ));
        }
        Action::Rounds(n) => {
            std::env::set_var("ROF_MAX_ROUNDS", n.to_string());
            app.transcript.push(format!(
                "rounds={n} (ROF_MAX_ROUNDS, applies to the next goal)"
            ));
        }
        Action::Thinking(m) => {
            std::env::set_var("ROF_THINKING", &m);
            app.transcript.push(format!(
                "thinking={m} (ROF_THINKING, applies to the next goal)"
            ));
        }
        Action::Effort(m) => {
            std::env::set_var("ROF_REASONING_EFFORT", &m);
            app.transcript.push(format!(
                "effort={m} (ROF_REASONING_EFFORT, applies to the next goal)"
            ));
        }
        Action::Caps(a, b) => {
            std::env::set_var("ROF_IMPLEMENTER_MAX_TOKENS", a.to_string());
            std::env::set_var("ROF_REVIEWER_MAX_TOKENS", b.to_string());
            app.transcript.push(format!(
                "caps implementer={a} reviewer={b} (applies to the next goal)"
            ));
        }
        Action::Model(_) => {
            // Two-token role forms ride the raw line: parse() keeps only
            // the first token, so Model("ctx") alone never names a model.
            let toks: Vec<&str> = raw.split_whitespace().collect();
            match toks.as_slice() {
                [_, "ctx", id] => {
                    std::env::set_var("ROF_CTX_MODEL", id);
                    app.transcript.push(format!(
                        "context model={id} (ROF_CTX_MODEL, applies to the next goal — clients rebuild per goal)"
                    ));
                }
                [_, "verify", id] => {
                    std::env::set_var("ROF_VERIFY_MODEL", id);
                    app.transcript.push(format!(
                        "verify model={id} (ROF_VERIFY_MODEL, applies to the next goal — clients rebuild per goal)"
                    ));
                }
                [_, "fallback", id] => {
                    if *id == "none" {
                        std::env::remove_var("ROF_EXEC_FALLBACK");
                        app.transcript.push(
                            "executor fallback cleared (applies to the next goal)".to_string(),
                        );
                    } else {
                        std::env::set_var("ROF_EXEC_FALLBACK", id);
                        app.transcript.push(format!(
                            "executor fallback={id} (ROF_EXEC_FALLBACK, applies to the next goal)"
                        ));
                    }
                }
                [_, spec] => {
                    if spec.contains('/') {
                        std::env::set_var("ROF_EXEC_MODEL", spec);
                        app.transcript.push(format!(
                            "model={spec} (ROF_EXEC_MODEL, applies to the next goal — clients rebuild per goal)"
                        ));
                    } else {
                        app.transcript
                            .push("/model needs <provider>/<model-id>".to_string());
                        app.transcript.push(super::cmd::help_text());
                    }
                }
                _ => {
                    app.transcript.push(
                        "/model needs <provider>/<model-id> (or /model ctx|verify|fallback <provider>/<model-id>)"
                            .to_string(),
                    );
                }
            }
        }
        Action::Login(_) => {
            let toks: Vec<&str> = raw.split_whitespace().collect();
            match toks.as_slice() {
                [_, prov, key] => match verify_blocking(prov, key) {
                    Ok(ids) => match super::auth::store().save(prov, key) {
                        Ok(()) => app.transcript.push(format!(
                            "logged in {prov} ({} models) — applies to the next goal",
                            ids.len()
                        )),
                        Err(e) => app
                            .transcript
                            .push(format!("login verified but save failed: {e}")),
                    },
                    Err(e) => app.transcript.push(format!("login failed: {e}")),
                },
                [_, prov] => {
                    *awaiting_key = Some((*prov).to_string());
                    app.mask_input = true;
                    app.transcript.push(format!(
                        "/login {prov}: type the key and press Enter (input hidden)"
                    ));
                }
                _ => app.transcript.push(
                    "/login needs <provider> (openrouter/go/atria/custom, or /provider add <name> <base> first)"
                        .to_string(),
                ),
            }
        }
        Action::ProviderAdd(name) => {
            // parse() validated three tokens; the base rides the raw line
            // like /login's key does.
            let base = raw.split_whitespace().nth(2).unwrap_or("");
            match super::auth::save_provider(&name, base) {
                Ok(()) => app.transcript.push(format!(
                    "provider {name} → {base} (verify with /login {name})"
                )),
                Err(e) => app.transcript.push(format!("provider add failed: {e}")),
            }
        }
        Action::ProviderList => {
            let st = super::auth::statuses();
            let status = |p: &str| {
                st.get(p)
                    .cloned()
                    .unwrap_or_else(|| "unverified".to_string())
            };
            for p in ["openrouter", "go", "atria"] {
                let base = super::auth::base_for_test(p).unwrap_or_default();
                app.transcript
                    .push(format!("provider {p} → {base} [built-in, {}]", status(p)));
            }
            for (name, base) in super::auth::registry() {
                app.transcript.push(format!(
                    "provider {name} → {base} [custom, {}]",
                    status(&name)
                ));
            }
            app.transcript.push(
                "keys live in the credentials store or env; /login <provider> verifies + saves"
                    .to_string(),
            );
        }
        Action::ProviderRm(name) => {
            if ["openrouter", "go", "atria", "custom"].contains(&name.as_str()) {
                app.transcript
                    .push(format!("{name} is built-in and cannot be removed"));
            } else {
                match super::auth::remove_provider(&name) {
                    Ok(true) => app.transcript.push(format!("provider {name} removed")),
                    Ok(false) => app.transcript.push(format!("no provider {name}")),
                    Err(e) => app.transcript.push(format!("provider rm failed: {e}")),
                }
            }
        }
        Action::Logout(p) => match super::auth::store().remove(&p) {
            Ok(true) => app.transcript.push(format!("logged out {p}")),
            Ok(false) => app.transcript.push(format!(
                "no login for {p} (env keys still apply at call time)"
            )),
            Err(e) => app.transcript.push(format!("logout failed: {e}")),
        },
        Action::Undo => app.transcript.push(
            "per-task rollback is automatic; cross-goal undo lands with Plan C Task 4".to_string(),
        ),
        Action::Retry(note) => app.transcript.push(format!(
            "retry is not available between goals yet{}",
            note.map(|n| format!(": {n}")).unwrap_or_default()
        )),
        Action::Approve(id) => app
            .transcript
            .push(format!("no pending proposal {id} in this console yet")),
        Action::Reject(id) => app
            .transcript
            .push(format!("no pending proposal {id} in this console yet")),
        Action::Display(m) => app.transcript.push(format!(
            "display={m}: single fullscreen view in this console"
        )),
        Action::Busy(m) => app.transcript.push(format!(
            "busy={m}: one goal at a time; Enter during a run is read-only (P1a has no queue)"
        )),
    }
    false
}

/// Render a recorded trace file in the fullscreen console. `q`/Esc/Ctrl-C
/// quits; anything else edits the (replay-inert) composer.
pub fn replay(path: &std::path::Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut app = App::new();
    app.thinking = std::env::var("ROF_THINKING").unwrap_or_default();
    let mut events = Vec::new();
    let mut unknown = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        match serde_json::from_str::<TraceEvent>(line) {
            Ok(event) => events.push(event),
            Err(_) => unknown.push(super::render::parse_lenient_line(line)),
        }
    }
    app.set_replay_events(events);
    app.set_replay_unknown(unknown);
    run_repl(app)
}

/// Alternate-screen event pump: 30fps render cap, `q`/Esc/Ctrl-C quits,
/// chars/backspace/enter drive the composer buffer.
fn run_repl(mut app: App) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = pump(&mut terminal, &mut app);

    disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;
    result
}

fn pump(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> anyhow::Result<()> {
    let mut filtering = false;
    loop {
        terminal.draw(|f| {
            if app.fresh {
                splash::draw(f);
            } else {
                draw(f, app);
            }
        })?;
        if !event::poll(Duration::from_millis(33))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            // The splash consumes the first keypress outright.
            if app.fresh {
                app.fresh = false;
                continue;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
            {
                break;
            }
            if filtering {
                match key.code {
                    KeyCode::Esc => {
                        filtering = false;
                        app.input.clear();
                    }
                    KeyCode::Backspace => {
                        app.input.pop();
                    }
                    KeyCode::Enter => {
                        let query = app.input.clone();
                        app.set_replay_filter(&query);
                        app.input.clear();
                        filtering = false;
                    }
                    KeyCode::Char(c)
                        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                    {
                        app.input.push(c);
                    }
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Esc => break,
                KeyCode::Char('q') if key.modifiers.is_empty() => break,
                // Replay-only nav: a failed guard falls through to the live
                // arms below, so live typing/scroll never loses keys.
                KeyCode::Char('j') if key.modifiers.is_empty() && app.replay_mode => {
                    app.replay_step(1)
                }
                KeyCode::Char('k') if key.modifiers.is_empty() && app.replay_mode => {
                    app.replay_step(-1)
                }
                KeyCode::Char('g') if key.modifiers.is_empty() && app.replay_mode => {
                    app.replay_start()
                }
                KeyCode::Char('G')
                    if (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
                        && app.replay_mode =>
                {
                    app.replay_end()
                }
                KeyCode::Char('/') if key.modifiers.is_empty() && app.replay_mode => {
                    app.input.clear();
                    filtering = true;
                }
                KeyCode::Char(c)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    app.input.push(c);
                }
                KeyCode::Backspace => {
                    app.input.pop();
                }
                KeyCode::Up if app.replay_mode => app.replay_step(-1),
                KeyCode::Down if app.replay_mode => app.replay_step(1),
                KeyCode::Up => app.scroll_lines(1),
                KeyCode::Down => app.scroll_lines(-1),
                KeyCode::PageUp => app.scroll_lines(10),
                KeyCode::PageDown => app.scroll_lines(-10),
                KeyCode::Home => app.scroll_lines(isize::MAX),
                KeyCode::End => app.scroll_lines(isize::MIN),
                KeyCode::Enter => {
                    app.input.clear();
                }
                _ => {}
            }
        }
    }
    Ok(())
}
