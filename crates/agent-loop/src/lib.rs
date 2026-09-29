//! Turn/step driver. See research/crate-agent-loop.md.
//!
//! NOTE (follow-up): full multi-tick run() wiring against the snapshot /
//! context / bets crates does not exist yet. This crate ships the state
//! machine + step + termination, tested standalone; provider and tool
//! traffic enter as scripted [`ProviderMsg`] / [`ToolMsg`] fakes sent over
//! channels into [`drive_tick`].

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use agent_budget::{BudgetCounters, BudgetHalt};
use agent_log::{InputSource, Item, ItemKind, RecoveryCode, TurnEndReason};
use provider_core::{AssistantMessage, ProviderMessage, StopReason};
use serde_json::Value;
use tool_core::{TerminatePolicy, ToolCall, ToolResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateStatus {
    Open,
    Aborting,
    Closed,
}

#[derive(Debug, Clone)]
pub enum GateError {
    AbortRequested,
    Closed(String),
}

/// Two-phase effect gate: begin_abort decides, signal_abort propagates.
pub struct EffectGate {
    inner: Mutex<GateInner>,
}

struct GateInner {
    status: GateStatus,
    closed_err: String,
}

impl EffectGate {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(GateInner {
                status: GateStatus::Open,
                closed_err: String::new(),
            }),
        }
    }
    pub fn status(&self) -> GateStatus {
        self.inner.lock().unwrap().status
    }
    pub fn is_open(&self) -> bool {
        self.status() == GateStatus::Open
    }
    pub fn admit(&self) -> Result<(), GateError> {
        let gate = self.inner.lock().unwrap();
        match gate.status {
            GateStatus::Open => Ok(()),
            GateStatus::Aborting => Err(GateError::AbortRequested),
            GateStatus::Closed => Err(GateError::Closed(gate.closed_err.clone())),
        }
    }
    pub fn begin_abort(&self) {
        let mut gate = self.inner.lock().unwrap();
        if gate.status == GateStatus::Open {
            gate.status = GateStatus::Aborting;
        }
    }
    pub fn signal_abort(&self, token: &CancellationToken) {
        if self.status() == GateStatus::Aborting {
            token.cancel();
        }
    }
    pub fn close(&self, err: String) {
        let mut gate = self.inner.lock().unwrap();
        gate.status = GateStatus::Closed;
        gate.closed_err = err;
    }
}

impl Default for EffectGate {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Running,
    Maintenance,
}

#[derive(Debug, Clone)]
pub enum PhaseVerdict {
    Return(Outcome),
    Break,
    Continue,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Done,
    Halted(String),
    Cancelled,
    Failed(String),
}

#[derive(Debug, Clone)]
pub enum Input {
    User(String),
    StopHard,
    StopWhenIdle,
}

/// Fake provider traffic: what a single-flight provider task ships back.
#[derive(Debug, Clone)]
pub enum ProviderMsg {
    Partial {
        turn: u64,
        text: String,
    },
    Settled {
        turn: u64,
        message: AssistantMessage,
        stop: StopReason,
    },
    Failed {
        turn: u64,
        err: String,
        cancelled: bool,
    },
}

impl ProviderMsg {
    pub fn turn(&self) -> u64 {
        match *self {
            ProviderMsg::Partial { turn, .. }
            | ProviderMsg::Settled { turn, .. }
            | ProviderMsg::Failed { turn, .. } => turn,
        }
    }
}

/// Fake tool traffic: one finished call.
#[derive(Debug, Clone)]
pub struct ToolMsg {
    pub call_id: String,
    pub result: ToolResult,
}

/// Claim-phase outcome for one settled assistant message.
#[derive(Debug, Clone)]
pub enum ClaimOutcome {
    Dispatch(Vec<ToolCall>),
    Truncated(usize),
    Done,
    Refused,
    HardExit(String),
}

#[derive(Debug, Clone)]
pub struct InFlight {
    pub turn: u64,
    pub token: CancellationToken,
}

#[derive(Debug, Clone)]
pub struct ToolCallState {
    pub name: String,
    pub result: Option<ToolResult>,
}

pub struct LoopState {
    pub turn: u64,
    pub step: u32,
    pub phase: Phase,
    pub items: Vec<Item>,
    pub steering: VecDeque<String>,
    pub followups: VecDeque<String>,
    pub wake_requested: bool,
    pub call_model: bool,
    pub in_flight: Option<InFlight>,
    pub tool_calls: HashMap<String, ToolCallState>,
    pub gate: EffectGate,
    pub stop_hard: bool,
    pub stop_when_idle: bool,
    pub turn_reason: Option<TurnEndReason>,
    pub budget: BudgetCounters,
    pub max_steps: u32,
    pub max_trial_actions: u32,
    pub same_action_cycles: u32,
    pub overflow_recovery_pending: bool,
    pub restart_count: u32,
    pub truncation_retries: u32,
    pub step_retries: u32,
    pub fatal_error: Option<String>,
    pub needs_reflect: bool,
    pub last_sig: String,
    pub last_obs: u64,
    pub lessons: VecDeque<String>,
    pub drain_timeout: Duration,
    pub drain_until: Option<Instant>,
    pub prompt_sections: HashMap<String, String>,
}

impl LoopState {
    pub fn new() -> Self {
        Self {
            turn: 0,
            step: 0,
            phase: Phase::Idle,
            items: Vec::new(),
            steering: VecDeque::new(),
            followups: VecDeque::new(),
            wake_requested: false,
            call_model: false,
            in_flight: None,
            tool_calls: HashMap::new(),
            gate: EffectGate::new(),
            stop_hard: false,
            stop_when_idle: false,
            turn_reason: None,
            budget: BudgetCounters::default(),
            max_steps: 20,
            max_trial_actions: 30,
            same_action_cycles: 3,
            overflow_recovery_pending: false,
            restart_count: 0,
            truncation_retries: 0,
            step_retries: 2,
            fatal_error: None,
            needs_reflect: false,
            last_sig: String::new(),
            last_obs: 0,
            lessons: VecDeque::new(),
            drain_timeout: Duration::from_secs(10),
            drain_until: None,
            prompt_sections: HashMap::new(),
        }
    }

    pub fn open_tools(&self) -> usize {
        self.tool_calls
            .values()
            .filter(|c| c.result.is_none())
            .count()
    }

    pub fn batch_complete(&self) -> bool {
        !self.tool_calls.is_empty() && self.open_tools() == 0
    }

    /// Unanimous batch rule (Pi agent-loop.js:463-464), via tool-core.
    pub fn batch_concluded(&self) -> bool {
        if !self.batch_complete() {
            return false;
        }
        let results: Vec<ToolResult> = self
            .tool_calls
            .values()
            .filter_map(|c| c.result.clone())
            .collect();
        tool_core::batch_concluded(&results, TerminatePolicy::Unanimous)
    }

    /// The one boolean: do we talk to the model now?
    pub fn should_call_model(&self) -> bool {
        if !self.gate.is_open() || self.stop_hard {
            return false;
        }
        self.call_model
            || (!self.steering.is_empty() && self.in_flight.is_none() && self.open_tools() == 0)
    }

    pub fn is_idle(&self) -> bool {
        self.in_flight.is_none()
            && self.open_tools() == 0
            && self.steering.is_empty()
            && self.followups.is_empty()
    }

    fn open_turn(&mut self) {
        self.turn += 1;
        self.phase = Phase::Running;
        let prev = if self.turn > 1 {
            Some(turn_id(self.turn - 1))
        } else {
            None
        };
        append_to(
            &mut self.items,
            ItemKind::TurnStart {
                turn_id: turn_id(self.turn),
                prev_turn_id: prev,
            },
        );
    }

    /// Inbox classification by phase: Running steers, Idle opens a turn,
    /// Maintenance latches a wake for replay. Controls are data, not methods.
    pub fn apply_input(&mut self, input: Input) {
        match input {
            Input::User(text) => match self.phase {
                Phase::Idle => {
                    self.open_turn();
                    self.steering.push_back(text);
                }
                Phase::Running => self.steering.push_back(text),
                Phase::Maintenance => {
                    self.steering.push_back(text);
                    self.wake_requested = true;
                }
            },
            Input::StopHard => {
                self.stop_hard = true;
                self.gate.begin_abort();
            }
            Input::StopWhenIdle => self.stop_when_idle = true,
        }
    }

    /// Step pre-boundary: the only place steering enters the transcript.
    pub fn admit_steering(&mut self) -> usize {
        let mut admitted = 0;
        while let Some(text) = self.steering.pop_front() {
            let turn = self.turn;
            append_to(
                &mut self.items,
                ItemKind::Input {
                    input_id: format!("input-{turn}-{admitted}"),
                    text,
                    source: InputSource::External,
                },
            );
            admitted += 1;
        }
        if self.wake_requested && self.phase == Phase::Idle {
            self.wake_requested = false;
            self.open_turn();
        }
        admitted
    }

    /// Single-flight provider admission. Returns the per-turn child token.
    pub fn start_provider_call(&mut self, root: &CancellationToken) -> Option<CancellationToken> {
        if self.in_flight.is_some() || self.gate.admit().is_err() {
            return None;
        }
        let token = root.child_token();
        self.in_flight = Some(InFlight {
            turn: self.turn,
            token: token.clone(),
        });
        self.step += 1;
        self.budget.steps += 1;
        self.call_model = false;
        // Prior batch results belong to older turns; drop them so the
        // unanimity check only ever sees the current batch.
        self.tool_calls.retain(|_, c| c.result.is_none());
        Some(token)
    }

    /// Returns false when the message is stale (wrong turn) and was dropped.
    pub fn finish_provider_msg(&mut self, msg: ProviderMsg) -> bool {
        if msg.turn() != self.turn {
            return false; // STALE GUARD: late landing from an interrupted turn.
        }
        self.in_flight = None;
        if let ProviderMsg::Failed { err, cancelled, .. } = msg {
            if cancelled {
                return true;
            }
            let retry = self.step_retries > 0;
            if retry {
                self.step_retries -= 1;
            }
            append_to(
                &mut self.items,
                ItemKind::Attempt {
                    error: err.clone(),
                    will_retry: retry,
                },
            );
            if retry {
                self.call_model = true; // in-step retry reuses the same assembly
            } else {
                self.fatal_error = Some(err.clone());
                self.gate.close(err);
            }
        }
        true
    }

    fn push_assistant(&mut self, message: &AssistantMessage, stop_label: &str) {
        let value = serde_json::to_value(message).unwrap_or(Value::Null);
        append_to(
            &mut self.items,
            ItemKind::Assistant {
                message: value,
                stop_reason: stop_label.to_owned(),
                interrupted: false,
            },
        );
    }

    /// Claim: validate the settled message before anything executes.
    pub fn step_claim(&mut self, message: AssistantMessage, stop: StopReason) -> ClaimOutcome {
        let stop_label = format!("{stop:?}");
        match stop {
            StopReason::MaxTokens => {
                // Pi truncation guard: fail the whole batch unexecuted.
                for tc in &message.tool_calls {
                    append_to(
                        &mut self.items,
                        ItemKind::ToolCall {
                            call_id: tc.id.clone(),
                            tool: tc.name.clone(),
                            args: tc.args.clone(),
                        },
                    );
                    let content =
                        "tool call truncated: re-issue with complete arguments".to_owned();
                    append_to(
                        &mut self.items,
                        ItemKind::ToolResult {
                            call_id: tc.id.clone(),
                            content: content.clone(),
                            is_error: true,
                            recovery: None,
                        },
                    );
                    self.tool_calls.insert(
                        tc.id.clone(),
                        ToolCallState {
                            name: tc.name.clone(),
                            result: Some(ToolResult {
                                content,
                                is_error: true,
                                terminate: false,
                            }),
                        },
                    );
                }
                let n = message.tool_calls.len();
                self.truncation_retries += 1;
                self.stick_turn_reason(TurnEndReason::MaxTokens);
                self.push_assistant(&message, &stop_label);
                ClaimOutcome::Truncated(n)
            }
            StopReason::Refused => {
                self.push_assistant(&message, &stop_label);
                self.stick_turn_reason(TurnEndReason::Error("refused".into()));
                ClaimOutcome::Refused
            }
            StopReason::Error | StopReason::Aborted => ClaimOutcome::HardExit(stop_label),
            _ => {
                self.push_assistant(&message, &stop_label);
                if message.tool_calls.is_empty() {
                    return ClaimOutcome::Done;
                }
                let mut calls = Vec::with_capacity(message.tool_calls.len());
                for tc in &message.tool_calls {
                    append_to(
                        &mut self.items,
                        ItemKind::ToolCall {
                            call_id: tc.id.clone(),
                            tool: tc.name.clone(),
                            args: tc.args.clone(),
                        },
                    );
                    self.tool_calls.insert(
                        tc.id.clone(),
                        ToolCallState {
                            name: tc.name.clone(),
                            result: None,
                        },
                    );
                    calls.push(ToolCall {
                        call_id: tc.id.clone(),
                        name: tc.name.clone(),
                        args: tc.args.clone(),
                    });
                }
                ClaimOutcome::Dispatch(calls)
            }
        }
    }

    /// First terminal reason wins: a later Completed must not downgrade MaxTokens.
    pub fn stick_turn_reason(&mut self, reason: TurnEndReason) {
        if self.turn_reason.is_none() {
            self.turn_reason = Some(reason);
        }
    }

    /// Returns true when the call id was known and still open.
    pub fn record_tool_result(&mut self, msg: ToolMsg) -> bool {
        let open = matches!(
            self.tool_calls.get(&msg.call_id),
            Some(c) if c.result.is_none()
        );
        if !open {
            return false;
        }
        append_to(
            &mut self.items,
            ItemKind::ToolResult {
                call_id: msg.call_id.clone(),
                content: msg.result.content.clone(),
                is_error: msg.result.is_error,
                recovery: None,
            },
        );
        if let Some(c) = self.tool_calls.get_mut(&msg.call_id) {
            c.result = Some(msg.result);
        }
        if self.batch_complete() && !self.batch_concluded() {
            self.call_model = true;
        }
        true
    }

    /// Same-action tripwire + 30-action abort, wired to budget counters.
    pub fn observe_action(&mut self, sig: &str, observation: &str) -> Option<BudgetHalt> {
        let mut hasher = DefaultHasher::new();
        observation.hash(&mut hasher);
        let obs = hasher.finish();
        if sig == self.last_sig && obs == self.last_obs {
            self.budget.same_action_streak += 1;
        } else {
            self.budget.same_action_streak = 0;
            self.last_sig = sig.to_owned();
            self.last_obs = obs;
        }
        self.budget.actions_this_trial += 1;
        if self.budget.actions_this_trial >= self.max_trial_actions {
            return Some(BudgetHalt::TrialActions);
        }
        if self.budget.same_action_streak >= self.same_action_cycles {
            return Some(BudgetHalt::SameAction);
        }
        None
    }

    /// dsh step-local crash repair: synthetic error results for unanswered calls.
    pub fn crash_repair(&mut self, reason: &str) -> usize {
        let open: Vec<String> = self
            .tool_calls
            .iter()
            .filter(|(_, c)| c.result.is_none())
            .map(|(id, _)| id.clone())
            .collect();
        let mut repaired = 0;
        for id in open {
            let content = format!("unanswered tool call ({reason})");
            append_to(
                &mut self.items,
                ItemKind::ToolResult {
                    call_id: id.clone(),
                    content: content.clone(),
                    is_error: true,
                    recovery: Some(RecoveryCode::ToolOutcomeUnknown),
                },
            );
            if let Some(c) = self.tool_calls.get_mut(&id) {
                c.result = Some(ToolResult {
                    content,
                    is_error: true,
                    terminate: false,
                });
                repaired += 1;
            }
        }
        repaired
    }

    pub fn push_lesson(&mut self, lesson: String) {
        if self.lessons.len() >= 3 {
            self.lessons.pop_front();
        }
        self.lessons.push_back(lesson);
    }

    /// Ordered termination, top-down, first match wins.
    pub fn terminate(&mut self) -> PhaseVerdict {
        // 1. Cancel / StopHard drains to terminal under a bounded deadline.
        if self.stop_hard {
            let until = *self
                .drain_until
                .get_or_insert_with(|| Instant::now() + self.drain_timeout);
            if (self.in_flight.is_some() || self.open_tools() > 0) && Instant::now() < until {
                return PhaseVerdict::Break;
            }
            return PhaseVerdict::Return(Outcome::Cancelled);
        }
        // 2. Hard error: in-step retries exhausted.
        if let Some(err) = self.fatal_error.clone() {
            return PhaseVerdict::Return(Outcome::Failed(err));
        }
        // 3. Budget exhausted.
        if self.budget.steps >= self.max_steps {
            return PhaseVerdict::Return(Outcome::Halted("steps".into()));
        }
        if self.budget.actions_this_trial >= self.max_trial_actions {
            return PhaseVerdict::Return(Outcome::Halted("trial-actions".into()));
        }
        // 4. Refusal is terminal-with-error; sticky max-tokens halts once drained.
        if let Some(TurnEndReason::Error(reason)) = self.turn_reason.clone() {
            return PhaseVerdict::Return(Outcome::Failed(reason));
        }
        if self.turn_reason == Some(TurnEndReason::MaxTokens)
            && self.in_flight.is_none()
            && self.open_tools() == 0
        {
            return PhaseVerdict::Return(Outcome::Halted("max-tokens".into()));
        }
        // Retry and reflection latches keep the run alive without new decisions.
        if self.call_model {
            return PhaseVerdict::Continue;
        }
        if self.needs_reflect || self.budget.same_action_streak >= self.same_action_cycles {
            self.needs_reflect = false;
            self.push_lesson("same action repeated without progress; vary the approach".into());
        }
        // 6. Tool-driven stop is unanimous; pending steering keeps the run alive.
        if self.batch_concluded() && self.steering.is_empty() {
            return PhaseVerdict::Return(Outcome::Done);
        }
        // 5. Semantic termination: drained turn, empty queues, follow-up poll.
        if self.in_flight.is_none() && self.open_tools() == 0 && self.steering.is_empty() {
            if let Some(text) = self.followups.pop_front() {
                self.open_turn();
                self.steering.push_back(text);
                return PhaseVerdict::Continue;
            }
            // A live driver parks here awaiting inbox; StopWhenIdle exits instead.
            if self.stop_when_idle {
                return PhaseVerdict::Return(Outcome::Done);
            }
            self.phase = Phase::Idle;
        }
        PhaseVerdict::Continue
    }

    /// Transcript fold: the request is derived from the log, not held.
    pub fn derived_messages(&self) -> Vec<ProviderMessage> {
        let mut out = Vec::new();
        for item in &self.items {
            match &item.kind {
                ItemKind::Input { text, .. } => out.push(ProviderMessage {
                    role: "user".into(),
                    content: text.clone(),
                }),
                ItemKind::Assistant { message, .. } => out.push(ProviderMessage {
                    role: "assistant".into(),
                    content: message_content(message),
                }),
                ItemKind::ToolResult { content, .. } => out.push(ProviderMessage {
                    role: "tool".into(),
                    content: content.clone(),
                }),
                _ => {}
            }
        }
        out
    }

    /// Replay system sections into the prompt patch map (Pi transcript fold).
    pub fn replay_sections(&mut self) {
        for item in &self.items {
            if let ItemKind::System { sections, .. } = &item.kind {
                for (name, value) in sections {
                    match value {
                        Some(text) => {
                            self.prompt_sections.insert(name.clone(), text.clone());
                        }
                        None => {
                            self.prompt_sections.remove(name);
                        }
                    }
                }
            }
        }
    }
}

impl Default for LoopState {
    fn default() -> Self {
        Self::new()
    }
}

fn message_content(message: &Value) -> String {
    message
        .get("content")
        .and_then(|c| c.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| message.to_string())
}

fn turn_id(turn: u64) -> String {
    format!("turn-{turn}")
}

fn append_to(items: &mut Vec<Item>, kind: ItemKind) {
    let seq = items.len() as u64;
    items.push(Item {
        seq,
        id: format!("item-{seq}"),
        parent_id: None,
        recorded_at: SystemTime::now(),
        kind,
    });
}

/// RAII turn guard: appends TurnEnd on drop, always, even on unwind.
pub struct TurnGuard<'a> {
    items: Option<&'a mut Vec<Item>>,
    turn_id: String,
    reason: TurnEndReason,
}

impl<'a> TurnGuard<'a> {
    pub fn open(items: &'a mut Vec<Item>, turn: u64) -> Self {
        let turn_id = turn_id(turn);
        append_to(
            items,
            ItemKind::TurnStart {
                turn_id: turn_id.clone(),
                prev_turn_id: None,
            },
        );
        Self {
            items: Some(items),
            turn_id,
            reason: TurnEndReason::Completed,
        }
    }
    pub fn end(mut self, reason: TurnEndReason) {
        self.reason = reason;
    }
}

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        if let Some(items) = self.items.take() {
            append_to(
                items,
                ItemKind::TurnEnd {
                    turn_id: self.turn_id.clone(),
                    reason: self.reason.clone(),
                },
            );
        }
    }
}

/// One select! tick over inbox/provider/tools/cancel (Unreal spine).
/// Classifies a single message, then runs the post-tick termination check.
/// Starting the next provider call (should_call_model -> start_provider_call)
/// stays the caller's job; the multi-tick run() is a follow-up.
pub async fn drive_tick(
    state: &mut LoopState,
    inbox: &mut mpsc::Receiver<Input>,
    provider_rx: &mut mpsc::Receiver<ProviderMsg>,
    tool_rx: &mut mpsc::Receiver<ToolMsg>,
    cancel: &CancellationToken,
) -> PhaseVerdict {
    tokio::select! {
        _ = cancel.cancelled() => {
            state.stop_hard = true;
            state.gate.begin_abort();
        }
        Some(input) = inbox.recv() => { state.apply_input(input); }
        Some(pm) = provider_rx.recv() => { state.finish_provider_msg(pm); }
        Some(tm) = tool_rx.recv() => { state.record_tool_result(tm); }
        else => {}
    }
    state.terminate()
}

/// Bets hook with a no-op default: loop compiles and ships with bets disabled.
pub trait BetsHook: Send + Sync {
    fn on_step(&self) -> PhaseVerdict {
        PhaseVerdict::Continue
    }
}

pub struct NoBets;

impl BetsHook for NoBets {}

#[cfg(test)]
mod tests {
    use super::*;
    use provider_core::ToolCallRef;

    fn assistant(calls: Vec<ToolCallRef>) -> AssistantMessage {
        AssistantMessage {
            content: "thinking".into(),
            tool_calls: calls,
            thinking: None,
        }
    }

    fn call(id: &str) -> ToolCallRef {
        ToolCallRef {
            id: id.into(),
            name: "edit".into(),
            args: Value::Null,
        }
    }

    fn settled(turn: u64) -> ProviderMsg {
        ProviderMsg::Settled {
            turn,
            message: assistant(vec![]),
            stop: StopReason::Stop,
        }
    }

    fn result(terminate: bool) -> ToolResult {
        ToolResult {
            content: "ok".into(),
            is_error: false,
            terminate,
        }
    }

    #[test]
    fn gate_transitions_idempotent() {
        let gate = EffectGate::new();
        assert_eq!(gate.status(), GateStatus::Open);
        assert!(gate.admit().is_ok());
        gate.begin_abort();
        assert_eq!(gate.status(), GateStatus::Aborting);
        assert!(matches!(gate.admit(), Err(GateError::AbortRequested)));
        gate.begin_abort(); // idempotent: still aborting, not closed
        assert_eq!(gate.status(), GateStatus::Aborting);
        let token = CancellationToken::new();
        gate.signal_abort(&token);
        assert!(token.is_cancelled());
        gate.signal_abort(&token); // idempotent propagate
        assert!(token.is_cancelled());
        gate.close("boom".into());
        assert_eq!(gate.status(), GateStatus::Closed);
        assert!(matches!(gate.admit(), Err(GateError::Closed(_))));
        gate.begin_abort(); // close wins over a late abort
        assert_eq!(gate.status(), GateStatus::Closed);
    }

    #[test]
    fn should_call_model_truth_table() {
        let mut s = LoopState::new();
        assert!(!s.should_call_model()); // fresh: nothing to do
        s.call_model = true;
        assert!(s.should_call_model());
        s.gate.begin_abort();
        assert!(!s.should_call_model()); // aborting gate says nay
        s.gate.close("x".into());
        assert!(!s.should_call_model());
        let mut s = LoopState::new();
        s.call_model = true;
        s.stop_hard = true;
        assert!(!s.should_call_model());
        let mut s = LoopState::new(); // pending steering, free slot
        s.steering.push_back("hi".into());
        assert!(s.should_call_model());
        s.in_flight = Some(InFlight {
            turn: 0,
            token: CancellationToken::new(),
        });
        assert!(!s.should_call_model()); // single-flight occupied
        let mut s = LoopState::new(); // pending steering, tool still open
        s.steering.push_back("hi".into());
        s.tool_calls.insert(
            "c1".into(),
            ToolCallState {
                name: "edit".into(),
                result: None,
            },
        );
        assert!(!s.should_call_model());
    }

    #[test]
    fn stale_turn_guard_drops_late_provider_msg() {
        let mut s = LoopState::new();
        s.turn = 1;
        let root = CancellationToken::new();
        assert!(s.start_provider_call(&root).is_some());
        s.turn = 2; // interrupt opened a newer turn
        assert!(!s.finish_provider_msg(settled(1))); // stale: dropped
        assert!(s.in_flight.is_some());
        assert!(s.finish_provider_msg(settled(2)));
        assert!(s.in_flight.is_none());
    }

    #[test]
    fn single_flight_provider_call() {
        let mut s = LoopState::new();
        s.turn = 1;
        let root = CancellationToken::new();
        let child = s.start_provider_call(&root).expect("first starts");
        assert!(!child.is_cancelled());
        assert!(s.start_provider_call(&root).is_none()); // second refused
        assert_eq!(s.budget.steps, 1);
        assert!(s.finish_provider_msg(settled(1)));
        assert!(s.start_provider_call(&root).is_some()); // slot freed
    }

    #[test]
    fn termination_order() {
        let mut s = LoopState::new(); // fresh parks, it does not exit
        assert!(matches!(s.terminate(), PhaseVerdict::Continue));
        let mut s = LoopState::new(); // cancel beats budget
        s.stop_hard = true;
        s.budget.steps = s.max_steps;
        assert!(matches!(
            s.terminate(),
            PhaseVerdict::Return(Outcome::Cancelled)
        ));
        let mut s = LoopState::new(); // hard error beats budget
        s.fatal_error = Some("e".into());
        s.budget.steps = s.max_steps;
        assert!(matches!(
            s.terminate(),
            PhaseVerdict::Return(Outcome::Failed(_))
        ));
        let mut s = LoopState::new(); // budget beats done-shaped state
        s.stop_when_idle = true;
        s.budget.steps = s.max_steps;
        assert!(matches!(
            s.terminate(),
            PhaseVerdict::Return(Outcome::Halted(_))
        ));
        let mut s = LoopState::new(); // refusal is terminal-with-error
        s.turn_reason = Some(TurnEndReason::Error("refused".into()));
        assert!(matches!(
            s.terminate(),
            PhaseVerdict::Return(Outcome::Failed(_))
        ));
        let mut s = LoopState::new(); // idle + StopWhenIdle exits
        s.stop_when_idle = true;
        assert!(matches!(s.terminate(), PhaseVerdict::Return(Outcome::Done)));
    }

    #[test]
    fn truncation_fails_batch_unexecuted() {
        let mut s = LoopState::new();
        s.turn = 1;
        let before = s.items.len();
        let outcome = s.step_claim(assistant(vec![call("a"), call("b")]), StopReason::MaxTokens);
        assert!(matches!(outcome, ClaimOutcome::Truncated(2)));
        for id in ["a", "b"] {
            let state = &s.tool_calls[id];
            let res = state.result.as_ref().expect("answered, not dispatched");
            assert!(res.is_error);
            assert!(!res.terminate);
        }
        assert!(s.items.len() > before);
        assert_eq!(s.turn_reason, Some(TurnEndReason::MaxTokens));
        s.stick_turn_reason(TurnEndReason::Completed); // sticky: no downgrade
        assert_eq!(s.turn_reason, Some(TurnEndReason::MaxTokens));
        assert!(matches!(
            s.terminate(),
            PhaseVerdict::Return(Outcome::Halted(_))
        ));
    }

    #[test]
    fn steering_admitted_only_at_pre_boundary() {
        let mut s = LoopState::new();
        s.phase = Phase::Running;
        let items = s.items.len();
        s.apply_input(Input::User("steer".into()));
        assert_eq!(s.steering.len(), 1);
        assert_eq!(s.items.len(), items); // invisible until the boundary
        assert_eq!(s.admit_steering(), 1);
        assert!(s.steering.is_empty());
        assert_eq!(s.items.len(), items + 1);

        let mut s = LoopState::new(); // idle input opens a new turn
        s.apply_input(Input::User("hi".into()));
        assert_eq!((s.turn, s.phase), (1, Phase::Running));
        assert!(matches!(s.items[0].kind, ItemKind::TurnStart { .. }));

        let mut s = LoopState::new(); // maintenance latches, idle replays
        s.phase = Phase::Maintenance;
        s.apply_input(Input::User("m".into()));
        assert!(s.wake_requested);
        s.phase = Phase::Idle;
        s.admit_steering();
        assert_eq!((s.phase, s.turn), (Phase::Running, 1));
        assert!(!s.wake_requested);
    }

    #[test]
    fn turn_guard_drop_appends_turn_end() {
        let mut items = Vec::new();
        {
            let _guard = TurnGuard::open(&mut items, 3);
        }
        assert!(matches!(
            items.last().map(|i| &i.kind),
            Some(ItemKind::TurnEnd { turn_id, reason: TurnEndReason::Completed })
            if turn_id == "turn-3"
        ));
        {
            let guard = TurnGuard::open(&mut items, 4);
            guard.end(TurnEndReason::Budget);
        }
        assert!(matches!(
            items.last().map(|i| &i.kind),
            Some(ItemKind::TurnEnd {
                reason: TurnEndReason::Budget,
                ..
            })
        ));
    }

    #[test]
    fn stop_hard_drains_bounded() {
        let mut s = LoopState::new(); // busy + generous deadline: keep draining
        s.stop_hard = true;
        s.tool_calls.insert(
            "c1".into(),
            ToolCallState {
                name: "edit".into(),
                result: None,
            },
        );
        assert!(matches!(s.terminate(), PhaseVerdict::Break));
        s.drain_timeout = Duration::ZERO; // deadline passes: terminal now
        s.drain_until = None;
        assert!(matches!(
            s.terminate(),
            PhaseVerdict::Return(Outcome::Cancelled)
        ));
        let mut s = LoopState::new(); // idle cancels at once
        s.stop_hard = true;
        assert!(matches!(
            s.terminate(),
            PhaseVerdict::Return(Outcome::Cancelled)
        ));
    }

    #[test]
    fn in_step_retry_then_hard_exit() {
        let mut s = LoopState::new();
        s.turn = 1;
        let root = CancellationToken::new();
        s.start_provider_call(&root);
        assert!(s.finish_provider_msg(ProviderMsg::Failed {
            turn: 1,
            err: "flaky".into(),
            cancelled: false,
        }));
        assert!(s.call_model); // retry reuses the open step
        assert!(s.in_flight.is_none());
        assert!(matches!(s.terminate(), PhaseVerdict::Continue));
        s.step_retries = 0;
        s.start_provider_call(&root);
        assert!(s.finish_provider_msg(ProviderMsg::Failed {
            turn: 1,
            err: "dead".into(),
            cancelled: false,
        }));
        assert_eq!(s.fatal_error.as_deref(), Some("dead"));
        assert!(matches!(
            s.terminate(),
            PhaseVerdict::Return(Outcome::Failed(_))
        ));
        assert!(!s.should_call_model()); // closed gate says nay
    }

    #[test]
    fn crash_repair_synthesizes_results() {
        let mut s = LoopState::new();
        s.turn = 1;
        let outcome = s.step_claim(assistant(vec![call("a"), call("b")]), StopReason::ToolUse);
        assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
        assert_eq!(s.crash_repair("boom"), 2);
        assert_eq!(s.crash_repair("boom"), 0); // idempotent
        for id in ["a", "b"] {
            let res = s.tool_calls[id].result.as_ref().unwrap();
            assert!(res.is_error);
        }
        assert!(s.batch_complete());
        assert!(!s.batch_concluded());
    }

    #[test]
    fn tripwire_and_thirty_action_abort() {
        let mut s = LoopState::new();
        assert_eq!(s.observe_action("a", "o"), None);
        assert_eq!(s.observe_action("a", "o"), None);
        assert_eq!(s.observe_action("a", "o"), None);
        assert_eq!(s.observe_action("a", "o"), Some(BudgetHalt::SameAction));
        let mut s = LoopState::new();
        let mut last = None;
        for i in 0..30 {
            last = s.observe_action(&format!("act-{i}"), "o");
        }
        assert!(matches!(last, Some(BudgetHalt::TrialActions)));
        assert!(matches!(
            s.terminate(),
            PhaseVerdict::Return(Outcome::Halted(_))
        ));
        // varying the observation resets the streak
        let mut s = LoopState::new();
        for _ in 0..3 {
            s.observe_action("a", "o");
        }
        assert_eq!(s.observe_action("a", "different"), None);
    }

    #[test]
    fn unanimous_batch_concluded() {
        let mut s = LoopState::new();
        assert!(!s.batch_concluded()); // empty batch concludes nothing
        s.tool_calls.insert(
            "a".into(),
            ToolCallState {
                name: "x".into(),
                result: None,
            },
        );
        assert!(!s.batch_concluded()); // pending call blocks
        s.record_tool_result(ToolMsg {
            call_id: "a".into(),
            result: result(true),
        });
        s.tool_calls.insert(
            "b".into(),
            ToolCallState {
                name: "x".into(),
                result: Some(result(false)),
            },
        );
        assert!(!s.batch_concluded()); // one holdout vetoes
        s.tool_calls.get_mut("b").unwrap().result = Some(result(true));
        assert!(s.batch_concluded());
        assert!(!s.record_tool_result(ToolMsg {
            call_id: "nope".into(),
            result: result(true),
        }));
    }

    #[test]
    fn derived_messages_and_section_replay() {
        let mut s = LoopState::new();
        s.phase = Phase::Running;
        s.apply_input(Input::User("build it".into()));
        s.admit_steering();
        s.items.push(Item {
            seq: s.items.len() as u64,
            id: "sys".into(),
            parent_id: None,
            recorded_at: SystemTime::now(),
            kind: ItemKind::System {
                sections: [("goal".to_owned(), Some("ship".to_owned()))]
                    .into_iter()
                    .collect(),
                tools_added: vec![],
                tools_removed: vec![],
            },
        });
        s.replay_sections();
        assert_eq!(
            s.prompt_sections.get("goal").map(String::as_str),
            Some("ship")
        );
        let msgs = s.derived_messages();
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            (msgs[0].role.as_str(), msgs[0].content.as_str()),
            ("user", "build it")
        );
    }

    #[tokio::test]
    async fn drive_tick_select_spine() {
        let mut s = LoopState::new();
        let (itx, mut irx) = mpsc::channel(8);
        let (_ptx, mut prx) = mpsc::channel(8);
        let (_ttx, mut trx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        itx.send(Input::User("go".into())).await.unwrap();
        let verdict = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel).await;
        assert!(matches!(verdict, PhaseVerdict::Continue));
        assert_eq!(s.turn, 1);
        itx.send(Input::StopHard).await.unwrap();
        let verdict = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel).await;
        assert!(matches!(verdict, PhaseVerdict::Return(Outcome::Cancelled)));
    }

    #[tokio::test]
    async fn cancel_token_drives_stop() {
        let mut s = LoopState::new();
        let (_itx, mut irx) = mpsc::channel(8);
        let (_ptx, mut prx) = mpsc::channel(8);
        let (_ttx, mut trx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let verdict = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel).await;
        assert!(matches!(verdict, PhaseVerdict::Return(Outcome::Cancelled)));
    }
}
