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

use super::app::{App, BusyMode, DeferredConfig};
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

/// One key's meaning while a goal is live, WITH the modifier decision made
/// here rather than in the pump.
///
/// This exists because [`handle_running_key`] cannot see modifiers: a bare
/// `KeyCode` cannot tell Ctrl-C from typing `c`, and the reducer's `Esc`/`q`
/// arms have the side effect of setting the stopping posture. If the pump
/// asked the reducer first and filtered afterwards, a MODIFIED key would move
/// the posture before anyone decided it was a stop key — and a stopping run
/// refuses to start a queued goal, so Shift+q would silently drop one.
/// Deciding here also means the reducer is consulted exactly once per key.
pub fn running_key_outcome(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> RunningKeyOutcome {
    // Ctrl-C is matched on the modifier first, for the reason above.
    if modifiers.contains(KeyModifiers::CONTROL)
        && matches!(code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        return RunningKeyOutcome::StopArmed;
    }
    // Any other modified key is ordinary input: Shift+q is draft text,
    // Alt+Esc is not a request to end the run.
    if !modifiers.is_empty() {
        return RunningKeyOutcome::Ignored;
    }
    handle_running_key(app, code)
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

/// What one slash action did while a goal was live. The pump reads this to
/// decide whether anything else has to happen: a stop has to be asked for,
/// a refusal has to be shown, and everything else has already said what it
/// did in the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunningActionOutcome {
    /// Display-only: the action rendered into the transcript and changed
    /// nothing else.
    View,
    /// Held for the next goal: the setting is in `App::deferred_config`,
    /// the env already carries it, and the transcript says when it lands.
    Deferred,
    /// Not done, with the reason to show. The draft is kept so the line
    /// can be corrected and submitted again.
    Rejected(String),
    /// The busy mode changed, which is the whole of the action: it sends
    /// nothing and the next submission reads the new mode.
    BusyMode(BusyMode),
    /// Ask the running goal to stop. The pump owns the send, so this is
    /// the same request the stop keys make.
    Stop,
}

/// Apply one slash action while a goal is live, as a pure reducer over
/// `App` plus the trace sink and the `/login` key capture the display
/// actions share with the between-goals path.
///
/// The classification is the whole contract, and it is deliberately
/// exhaustive over [`Action`]:
///
/// - **Display-only** — `Help`, `Unknown`, `Models`, `Context`, `Trace`,
///   `Hotkeys`, `Diff`, `ProviderList`, `Display`, and the already
///   unavailable `Retry`/`Approve`/`Reject`/`Undo` answers — go through
///   [`apply_action`] exactly as they do between goals, and return
///   [`RunningActionOutcome::View`]. They read state and write transcript
///   lines, and a live goal is no reason to refuse a read.
/// - **`Busy("steer" | "queue")`** sets the composer's busy mode, sends
///   nothing, and returns the new mode. **`Busy("interrupt")`** sets
///   `BusyMode::Interrupt` and returns [`RunningActionOutcome::Stop`]: it
///   sends nothing itself, because the pump's one stop path does the
///   sending and two sends would not agree.
/// - **Knobs** — `Attempts`, `Rounds`, `Thinking`, `Effort`, `Caps`,
///   `Model` — are appended to `App::deferred_config` in submission
///   order, written to the env by [`apply_deferred_config`], and reported
///   as [`RunningActionOutcome::Deferred`]. The running goal is untouched:
///   it snapshotted its config when it started.
/// - **Credentials** — `Login`, `Logout`, `ProviderAdd`, `ProviderRm` —
///   are refused with [`RunningActionOutcome::Rejected`]: no credential is
///   read, no env is written, and the store is not touched, so a running
///   goal can never have its clients change under it.
/// - **`Quit`** is refused too, and names the two-press stop path. It is
///   deliberately NOT routed through [`apply_action`], whose `Quit` arm
///   returns "end the console": one keystroke must not abandon a worker
///   that is still writing its trace.
///
/// A handled line — view, deferred, busy mode, or stop — consumes the
/// composer line, because it has been fully carried out. A refused line
/// keeps it: nothing was done to it, so the user can fix it and submit
/// again.
pub fn handle_running_action(
    app: &mut App,
    action: Action,
    commands: &tokio::sync::mpsc::UnboundedSender<RunCommand>,
    raw: &str,
    trace: &crate::obs::TraceSink,
    awaiting_key: &mut Option<String>,
) -> RunningActionOutcome {
    // Every action routed here is display-only, a setting, a posture, or
    // a refusal: none of them sends on the command channel. The parameter
    // is part of the caller's one shape for a composer line, and the one
    // send a live session does make — the stop — belongs to the pump, so
    // `/busy interrupt` and the stop keys cannot drift into two
    // disagreeing requests.
    let _ = commands;
    match action {
        Action::Help
        | Action::Unknown(_)
        | Action::Models
        | Action::Context
        | Action::Trace
        | Action::Hotkeys
        | Action::Diff
        | Action::ProviderList
        | Action::Display(_)
        | Action::Retry(_)
        | Action::Approve(_)
        | Action::Reject(_)
        | Action::Undo => {
            // The returned "quit" flag is false for every variant routed
            // here: `Quit` is refused below instead of dispatched.
            apply_action(app, trace, action, raw, awaiting_key);
            app.input.clear();
            RunningActionOutcome::View
        }
        Action::Quit => RunningActionOutcome::Rejected(QUIT_REFUSED.to_string()),
        Action::Login(_) | Action::Logout(_) | Action::ProviderAdd(_) | Action::ProviderRm(_) => {
            RunningActionOutcome::Rejected(credentials_refused(&action).to_string())
        }
        Action::Busy(mode) => {
            let busy = match mode.as_str() {
                "queue" => Some(BusyMode::Queue),
                "interrupt" => Some(BusyMode::Interrupt),
                // `parse` only produces the three known modes; anything
                // else is reported the way the between-goals path reports
                // it rather than guessed at.
                _ => None,
            };
            match busy {
                Some(busy) => {
                    app.set_busy_mode(busy);
                    app.input.clear();
                    // The idle path narrates a mode change, so the live path
                    // does too: a silent switch to `queue` would otherwise
                    // turn the user's next Enter into a queued goal they did
                    // not ask for.
                    app.transcript
                        .push(crate::tui::cmd::busy_line(busy_mode_word(busy)));
                    if busy == BusyMode::Interrupt {
                        RunningActionOutcome::Stop
                    } else {
                        RunningActionOutcome::BusyMode(busy)
                    }
                }
                None => {
                    apply_action(app, trace, Action::Busy(mode), raw, awaiting_key);
                    app.input.clear();
                    RunningActionOutcome::View
                }
            }
        }
        Action::Attempts(attempts) => deferred(app, DeferredConfig::Attempts(attempts)),
        Action::Rounds(rounds) => deferred(app, DeferredConfig::Rounds(rounds)),
        Action::Thinking(thinking) => deferred(app, DeferredConfig::Thinking(thinking)),
        Action::Effort(effort) => deferred(app, DeferredConfig::Effort(effort)),
        Action::Caps(implementer, reviewer) => {
            deferred(app, DeferredConfig::Caps(implementer, reviewer))
        }
        Action::Model(_) => match model_change(raw) {
            Ok(config) => deferred(app, config),
            // The line named no model, so there is nothing to hold. The
            // draft survives: it is one token away from being right.
            Err(refusal) => RunningActionOutcome::Rejected(refusal.message().to_string()),
        },
    }
}

/// Hold one setting for the next goal, apply it to the env, say when it
/// lands, and report it as deferred. The draft is consumed here because a
/// deferred setting is fully carried out: the only outcome that keeps the
/// line is a refusal.
fn deferred(app: &mut App, config: DeferredConfig) -> RunningActionOutcome {
    app.defer_config(config.clone());
    let line = apply_deferred_config(&config);
    app.transcript.push(line);
    app.input.clear();
    RunningActionOutcome::Deferred
}

/// What a `/model ...` line is missing. The two refusals print different
/// advice between goals — the one-token form is a typo and gets the full
/// command list — so they stay apart here rather than sharing a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelRefusal {
    NeedsSpec,
    NeedsForm,
}

impl ModelRefusal {
    fn message(self) -> &'static str {
        match self {
            ModelRefusal::NeedsSpec => "/model needs <provider>/<model-id>",
            ModelRefusal::NeedsForm => {
                "/model needs <provider>/<model-id> (or /model ctx|verify|fallback <provider>/<model-id>)"
            }
        }
    }

    /// Whether the full command list follows this refusal.
    fn shows_help(self) -> bool {
        matches!(self, ModelRefusal::NeedsSpec)
    }
}

/// The model setting a raw `/model ...` line names, in the one
/// [`DeferredConfig`] shape both paths carry.
///
/// The two-token role forms ride the raw line: `parse()` keeps only the
/// first token, so `Action::Model("ctx")` alone never names a model.
fn model_change(raw: &str) -> Result<DeferredConfig, ModelRefusal> {
    let toks: Vec<&str> = raw.split_whitespace().collect();
    match toks.as_slice() {
        [_, "ctx", id] => Ok(DeferredConfig::Model {
            slot: Some("ctx".to_string()),
            value: (*id).to_string(),
        }),
        [_, "verify", id] => Ok(DeferredConfig::Model {
            slot: Some("verify".to_string()),
            value: (*id).to_string(),
        }),
        [_, "fallback", id] => Ok(DeferredConfig::Model {
            slot: Some("fallback".to_string()),
            value: (*id).to_string(),
        }),
        [_, spec] if spec.contains('/') => Ok(DeferredConfig::Model {
            slot: None,
            value: (*spec).to_string(),
        }),
        [_, _] => Err(ModelRefusal::NeedsSpec),
        _ => Err(ModelRefusal::NeedsForm),
    }
}

/// `/quit` while a goal is live, naming the only way out that does not
/// abandon a running worker.
const QUIT_REFUSED: &str =
    "/quit is available between goals — press q/Esc/Ctrl-C twice to stop the run and exit";

/// Why a credential action cannot run against a live goal. The action is
/// named, never its arguments: a refused `/login` line can carry a key.
fn credentials_refused(action: &Action) -> &'static str {
    match action {
        Action::Login(_) => "/login is available between goals",
        Action::Logout(_) => "/logout is available between goals",
        Action::ProviderAdd(_) => "/provider add is available between goals",
        _ => "/provider rm is available between goals",
    }
}

/// Apply one deferred setting to the process env and return the transcript
/// wording for it. This is the only place a knob's env name is written,
/// so the between-goals path and the running path cannot drift apart.
///
/// The write happens HERE, at submission time, and never "just before the
/// next goal starts": a QUEUED goal is started by the worker task, not by
/// the pump, so the pump can never run at that instant for one. The next
/// goal — queued, or typed later — reads these env names when
/// `execute_with_control` builds its config, and the goal in flight is
/// unaffected because it snapshotted its config and built its clients when
/// it started. `App::deferred_config` is then only the ordered,
/// user-visible record of what is waiting, which is why `App::begin_run`
/// clears it: the record cannot outlive the goal the settings were waiting
/// for. That timing is also why the wording never claims to wait past a
/// queued goal — a setting submitted while one is queued does reach it.
pub fn apply_deferred_config(config: &DeferredConfig) -> String {
    let timing = "applies to the next goal";
    match config {
        DeferredConfig::Attempts(attempts) => {
            std::env::set_var("ROF_ATTEMPTS", attempts.to_string());
            format!("attempts={attempts} (ROF_ATTEMPTS, {timing})")
        }
        DeferredConfig::Rounds(rounds) => {
            std::env::set_var("ROF_MAX_ROUNDS", rounds.to_string());
            format!("rounds={rounds} (ROF_MAX_ROUNDS, {timing})")
        }
        DeferredConfig::Thinking(thinking) => {
            std::env::set_var("ROF_THINKING", thinking);
            format!("thinking={thinking} (ROF_THINKING, {timing})")
        }
        DeferredConfig::Effort(effort) => {
            std::env::set_var("ROF_REASONING_EFFORT", effort);
            format!("effort={effort} (ROF_REASONING_EFFORT, {timing})")
        }
        DeferredConfig::Caps(implementer, reviewer) => {
            std::env::set_var("ROF_IMPLEMENTER_MAX_TOKENS", implementer.to_string());
            std::env::set_var("ROF_REVIEWER_MAX_TOKENS", reviewer.to_string());
            format!("caps implementer={implementer} reviewer={reviewer} ({timing})")
        }
        DeferredConfig::Model { slot, value } => {
            match slot.as_deref() {
                Some("ctx") => {
                    std::env::set_var("ROF_CTX_MODEL", value);
                    format!("context model={value} (ROF_CTX_MODEL, {timing} — clients rebuild per goal)")
                }
                Some("verify") => {
                    std::env::set_var("ROF_VERIFY_MODEL", value);
                    format!("verify model={value} (ROF_VERIFY_MODEL, {timing} — clients rebuild per goal)")
                }
                Some("fallback") if value == "none" => {
                    std::env::remove_var("ROF_EXEC_FALLBACK");
                    format!("executor fallback cleared ({timing})")
                }
                Some("fallback") => {
                    std::env::set_var("ROF_EXEC_FALLBACK", value);
                    format!("executor fallback={value} (ROF_EXEC_FALLBACK, {timing})")
                }
                // No slot, or one the console does not name: the executor
                // model. `model_change` is the only producer of this variant
                // and it uses exactly the three role slots above.
                _ => {
                    std::env::set_var("ROF_EXEC_MODEL", value);
                    format!("model={value} (ROF_EXEC_MODEL, {timing} — clients rebuild per goal)")
                }
            }
        }
    }
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
                // Running posture: the run is watched in place, and the
                // composer is both a steer/queue line and the place a
                // console command is typed. `running_key_outcome` decides
                // the key once, modifiers included, so nothing here can ask
                // the reducer about a key it must not see.
                match running_key_outcome(&mut app, key.code, key.modifiers) {
                    RunningKeyOutcome::Submit => {
                        // The reducers take `&mut App` and the line they
                        // are to act on, so the line is read out first: the
                        // pump must not decide anything about the draft
                        // itself, because clearing it and keeping it belong
                        // to the reducer.
                        let draft = app.input.clone();
                        match super::cmd::parse(&draft) {
                            Some(action) => {
                                match handle_running_action(
                                    &mut app,
                                    action,
                                    session.command_sender(),
                                    &draft,
                                    trace,
                                    &mut awaiting_key,
                                ) {
                                    // A refusal is the only outcome the
                                    // transcript does not already carry: the
                                    // display actions and the deferred setting
                                    // each wrote their own line, so adding one
                                    // here would say it twice.
                                    RunningActionOutcome::Rejected(reason) => {
                                        app.transcript.push(reason)
                                    }
                                    // The one send for a stop request lives in
                                    // the shared path below, so `/busy
                                    // interrupt` and the stop keys cannot
                                    // produce two disagreeing requests.
                                    RunningActionOutcome::Stop => {
                                        let commands = session.command_sender().clone();
                                        request_stop(&mut app, &mut session, &commands);
                                    }
                                    RunningActionOutcome::View
                                    | RunningActionOutcome::Deferred
                                    | RunningActionOutcome::BusyMode(_) => {}
                                }
                            }
                            None => {
                                if let RunningSubmit::Rejected(reason) =
                                    submit_running_input(&mut app, session.command_sender(), &draft)
                                {
                                    app.transcript.push(reason);
                                }
                            }
                        }
                        continue;
                    }
                    RunningKeyOutcome::StopArmed => {
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
                        let commands = session.command_sender().clone();
                        request_stop(&mut app, &mut session, &commands);
                        continue;
                    }
                    // Ignored: the key belongs to the composer or the
                    // scrollback below.
                    RunningKeyOutcome::Ignored => {}
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

/// First stop press on a live run, from either a stop key or a `/busy
/// interrupt` action: the worker is asked to stop, and only asked. The
/// command goes out under a fresh id so its acknowledgement is
/// addressable, and a send that fails changes nothing else — the latch,
/// the `Stopping` posture, and the hint the user reads are all here, and
/// the worker still has to be asked even if the channel is gone.
/// The composer word `/busy` uses for a mode, so the live path and the
/// between-goals path narrate the same switch with the same text.
fn busy_mode_word(mode: BusyMode) -> &'static str {
    match mode {
        BusyMode::Steer => "steer",
        BusyMode::Queue => "queue",
        BusyMode::Interrupt => "interrupt",
    }
}

/// Ask the running goal to stop, once.
///
/// Idempotent by design: the latch and the hint belong to the FIRST request.
/// A second `/busy interrupt` after a stop key must not send a second
/// `RunCommand::Stop`, repeat the hint, or — above all — be mistaken for the
/// second key press that detaches and exits; only a key press does that.
/// A failed send changes nothing here: the latch, the stopping posture, and
/// the hint are the P1a stop contract and do not depend on the channel.
pub fn request_stop(
    app: &mut App,
    session: &mut LiveSession,
    commands: &tokio::sync::mpsc::UnboundedSender<RunCommand>,
) {
    if session.stop_requested() {
        app.transcript.push(
            "stop already requested — press q/Esc/Ctrl-C again to detach and exit".to_string(),
        );
        return;
    }
    session.request_stop();
    app.set_stopping();
    let id = app.take_control_id();
    let _ = commands.send(RunCommand::Stop { id });
    app.transcript.push(stop_hint());
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
/// transcript; knob actions go through [`apply_deferred_config`], which
/// owns every env name they write; `/model` re-points the model vars and
/// `/login` verifies + saves, both taking effect on the next goal because
/// main rebuilds the config and the provider clients per goal. `raw` is
/// the full composer line — `parse()` keeps only the first token, so the
/// two-token `/model ctx` and `/login <prov> <key>` forms read from here.
/// Returns true on quit.
pub fn apply_action(
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
        // The pump has already taken the composer line out of `input`
        // before dispatch, so the shared holder's clear is a no-op here;
        // what this path shares with the running one is the env mapping
        // and the wording.
        Action::Attempts(attempts) => {
            deferred(app, DeferredConfig::Attempts(attempts));
        }
        Action::Rounds(rounds) => {
            deferred(app, DeferredConfig::Rounds(rounds));
        }
        Action::Thinking(thinking) => {
            deferred(app, DeferredConfig::Thinking(thinking));
        }
        Action::Effort(effort) => {
            deferred(app, DeferredConfig::Effort(effort));
        }
        Action::Caps(implementer, reviewer) => {
            deferred(app, DeferredConfig::Caps(implementer, reviewer));
        }
        Action::Model(_) => match model_change(raw) {
            Ok(config) => {
                deferred(app, config);
            }
            Err(refusal) => {
                app.transcript.push(refusal.message().to_string());
                if refusal.shows_help() {
                    app.transcript.push(super::cmd::help_text());
                }
            }
        },
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
        Action::Busy(m) => app.transcript.push(super::cmd::busy_line(&m)),
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
