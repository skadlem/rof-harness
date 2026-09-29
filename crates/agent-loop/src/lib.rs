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

use agent_budget::{config_for, BudgetGuard, BudgetHalt, Capability, Nudge};
use agent_event::{
    AgentError, AgentEvent, ControlAck, ControlKind, ControlStatus, DeltaKind, Emitter, Message,
    MessageDelta, Role, TurnEndReason as EventTurnEndReason,
};
use agent_log::{InputSource, Item, ItemKind, RecoveryCode, TurnEndReason};
use provider_core::{AssistantMessage, ProviderMessage, StopReason, Usage};
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
    Crash(String),
    StopHard,
    StopWhenIdle,
}

/// One queued steering line plus the source the log must record. `From`
/// defaults to External so existing call sites keep `push_back("hi".into())`.
#[derive(Debug, Clone)]
pub struct QueuedInput {
    pub text: String,
    pub source: InputSource,
}

impl From<String> for QueuedInput {
    fn from(text: String) -> Self {
        Self {
            text,
            source: InputSource::External,
        }
    }
}

impl From<&str> for QueuedInput {
    fn from(s: &str) -> Self {
        Self {
            text: s.to_owned(),
            source: InputSource::External,
        }
    }
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
        usage: Option<Usage>,
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
    pub steering: VecDeque<QueuedInput>,
    pub followups: VecDeque<String>,
    pub wake_requested: bool,
    pub call_model: bool,
    pub in_flight: Option<InFlight>,
    pub tool_calls: HashMap<String, ToolCallState>,
    pub gate: EffectGate,
    pub stop_hard: bool,
    pub stop_when_idle: bool,
    pub turn_reason: Option<TurnEndReason>,
    pub budget: BudgetGuard,
    pub emit_next: u64,
    pub open_msg: Option<u64>,
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
            budget: BudgetGuard::new(config_for(Capability::UnattendedBatch), Instant::now()),
            emit_next: 0,
            open_msg: None,
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
        // Crash queues exactly like User; only the recorded source differs.
        // Resume semantics are deferred: the Crash row is data for a later pass.
        let queued = match input {
            Input::User(text) => QueuedInput {
                text,
                source: InputSource::External,
            },
            Input::Crash(text) => QueuedInput {
                text,
                source: InputSource::Crash,
            },
            Input::StopHard => {
                self.stop_hard = true;
                self.gate.begin_abort();
                return;
            }
            Input::StopWhenIdle => {
                self.stop_when_idle = true;
                return;
            }
        };
        match self.phase {
            Phase::Idle => {
                self.open_turn();
                self.steering.push_back(queued);
            }
            Phase::Running => self.steering.push_back(queued),
            Phase::Maintenance => {
                self.steering.push_back(queued);
                self.wake_requested = true;
            }
        }
    }

    /// Step pre-boundary: the only place steering enters the transcript.
    pub fn admit_steering(&mut self) -> usize {
        let mut admitted = 0;
        while let Some(q) = self.steering.pop_front() {
            let turn = self.turn;
            append_to(
                &mut self.items,
                ItemKind::Input {
                    input_id: format!("input-{turn}-{admitted}"),
                    text: q.text,
                    source: q.source,
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
    /// Step head: the BudgetGuard AND-gate runs here, once per step, so its
    /// one grace step fires exactly once; never recreate the guard per step.
    pub fn start_provider_call(&mut self, root: &CancellationToken) -> Option<CancellationToken> {
        if self.in_flight.is_some() || self.gate.admit().is_err() {
            return None;
        }
        if self.budget.may_step().is_err() {
            return None;
        }
        let token = root.child_token();
        self.in_flight = Some(InFlight {
            turn: self.turn,
            token: token.clone(),
        });
        self.step += 1;
        self.budget.record_step();
        self.call_model = false;
        // Prior batch results belong to older turns; drop them so the
        // unanimity check only ever sees the current batch.
        self.tool_calls.retain(|_, c| c.result.is_none());
        Some(token)
    }

    /// Returns false when the message is stale (wrong turn) and was dropped.
    /// Spend/tokens land here from provider Usage: steps at the step head,
    /// tokens + spend on settle. Tokens/spend are never refunded.
    pub fn finish_provider_msg(&mut self, msg: ProviderMsg) -> bool {
        if msg.turn() != self.turn {
            return false; // STALE GUARD: late landing from an interrupted turn.
        }
        self.in_flight = None;
        if let ProviderMsg::Settled {
            usage: Some(usage), ..
        } = &msg
        {
            self.record_usage(usage);
        }
        if let ProviderMsg::Failed { err, cancelled, .. } = msg {
            if cancelled {
                self.budget.refund_step(); // no progress: bounded inside the guard
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

    /// Tokens meter every settle; spend converts cost_usd to cents.
    pub fn record_usage(&mut self, usage: &Usage) {
        self.budget.record_tokens(usage.total_tokens());
        if let Some(cost) = usage.cost_usd {
            self.budget
                .record_spend_cents((cost * 100.0).round().max(0.0) as u64);
        }
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
        let (same_cycles, per_trial) = (
            self.budget.config().same_action_cycles,
            self.budget.config().actions_per_trial,
        );
        let counters = self.budget.counters_mut();
        if sig == self.last_sig && obs == self.last_obs {
            counters.same_action_streak += 1;
        } else {
            counters.same_action_streak = 0;
            self.last_sig = sig.to_owned();
            self.last_obs = obs;
        }
        counters.actions_this_trial += 1;
        if per_trial > 0 && counters.actions_this_trial >= per_trial {
            return Some(BudgetHalt::TrialActions);
        }
        if same_cycles > 0 && counters.same_action_streak >= same_cycles {
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

    fn next_emit_id(&mut self) -> u64 {
        let id = self.emit_next;
        self.emit_next += 1;
        id
    }

    /// One-shot wrap-up notice. Caller-append contract: the text lands on the
    /// newest ToolResult tail in place, never as a synthetic user/system row;
    /// with no ToolResult tail the text is returned unappended and the caller
    /// skips it (keeps a provider-cached prefix intact).
    pub fn apply_budget_nudge(&mut self) -> Option<String> {
        let Nudge::WrapUp(text) = self.budget.nudge_due()?;
        if let Some(item) = self
            .items
            .iter_mut()
            .rev()
            .find(|i| matches!(i.kind, ItemKind::ToolResult { .. }))
        {
            if let ItemKind::ToolResult { content, .. } = &mut item.kind {
                content.push('\n');
                content.push_str(&text);
            }
        }
        Some(text)
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
        // 3. Budget exhausted: the guard AND-gate owns every cap; the loop
        // holds no shadow caps and halts the moment any counter trips.
        if let Some(exceeded) = self.budget.exceeded() {
            return PhaseVerdict::Return(Outcome::Halted(budget_halt_label(&exceeded.halt).into()));
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
        if self.needs_reflect
            || self.budget.counters().same_action_streak >= self.budget.config().same_action_cycles
        {
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
                self.steering.push_back(QueuedInput::from(text));
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

fn budget_halt_label(halt: &BudgetHalt) -> &'static str {
    match halt {
        BudgetHalt::Steps => "steps",
        BudgetHalt::Trials => "trials",
        BudgetHalt::Refines => "refines",
        BudgetHalt::Tokens => "tokens",
        BudgetHalt::Wallclock => "wallclock",
        BudgetHalt::Spend => "spend",
        BudgetHalt::SameAction => "same-action",
        BudgetHalt::TrialActions => "trial-actions",
    }
}

/// One tested mapping: durable log vocabulary to live event vocabulary.
/// MaxTokens has no event-side variant; MaxSteps is the ceiling-halt.
pub fn turn_end_reason_to_event(reason: &TurnEndReason) -> EventTurnEndReason {
    match reason {
        TurnEndReason::Completed => EventTurnEndReason::Completed,
        TurnEndReason::Error(_) => EventTurnEndReason::Error,
        TurnEndReason::Interrupted => EventTurnEndReason::Aborted,
        TurnEndReason::Budget => EventTurnEndReason::BudgetExceeded,
        TurnEndReason::MaxTokens => EventTurnEndReason::MaxSteps,
    }
}

fn outcome_log_reason(outcome: &Outcome, sticky: Option<&TurnEndReason>) -> TurnEndReason {
    if let Some(r) = sticky {
        return r.clone();
    }
    match outcome {
        Outcome::Done => TurnEndReason::Completed,
        Outcome::Halted(s) if s == "max-tokens" => TurnEndReason::MaxTokens,
        Outcome::Halted(_) => TurnEndReason::Budget,
        Outcome::Cancelled => TurnEndReason::Interrupted,
        Outcome::Failed(e) => TurnEndReason::Error(e.clone()),
    }
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

/// Emit one assistant message frame set: Start, one full-content Update, End.
/// Reuses the open Partial id when a stream preceded the settle.
fn emit_message_frames(state: &mut LoopState, message: &AssistantMessage, emitter: &mut Emitter) {
    let partial = Message {
        role: Role::Assistant,
        content: message.content.clone(),
    };
    let id = match state.open_msg.take() {
        Some(id) => id,
        None => {
            let id = state.next_emit_id();
            emitter.emit(AgentEvent::MessageStart {
                id,
                role: Role::Assistant,
                partial: Message {
                    role: Role::Assistant,
                    content: String::new(),
                },
            });
            id
        }
    };
    emitter.emit(AgentEvent::MessageUpdate {
        id,
        delta: MessageDelta {
            kind: DeltaKind::Text,
            text: Some(message.content.clone()),
        },
        partial: partial.clone(),
    });
    emitter.emit(AgentEvent::MessageEnd {
        id,
        message: partial,
        interrupted: false,
    });
}

/// One select! tick over inbox/provider/tools/cancel (Unreal spine), with the
/// live event seam attached. Ordering rule: the durable log append always
/// precedes its terminal frame in code order — `step_claim` /
/// `record_tool_result` / the TurnEnd append below run before the matching
/// `emit`, so replay from the log agrees with replay from events.
/// Starting the next provider call (should_call_model -> start_provider_call)
/// stays the caller's job; the multi-tick run() is a follow-up.
pub async fn drive_tick(
    state: &mut LoopState,
    inbox: &mut mpsc::Receiver<Input>,
    provider_rx: &mut mpsc::Receiver<ProviderMsg>,
    tool_rx: &mut mpsc::Receiver<ToolMsg>,
    cancel: &CancellationToken,
    emitter: &mut Emitter,
) -> PhaseVerdict {
    let turn_before = state.turn;
    tokio::select! {
        _ = cancel.cancelled() => {
            state.stop_hard = true;
            state.gate.begin_abort();
            let id = state.next_emit_id();
            emitter.emit(AgentEvent::Control(ControlAck {
                id,
                kind: ControlKind::Stop,
                status: ControlStatus::Applied,
                note: "cancel token fired".into(),
            }));
        }
        Some(input) = inbox.recv() => {
            // Control acks ride the same stream, so live and replay agree.
            let (kind, note) = match &input {
                Input::User(_) => (ControlKind::Steer, "steering queued"),
                Input::Crash(_) => (ControlKind::Steer, "crash queued"),
                Input::StopHard | Input::StopWhenIdle => (ControlKind::Stop, "stop latched"),
            };
            state.apply_input(input); // TurnStart append happens here when idle...
            let id = state.next_emit_id();
            emitter.emit(AgentEvent::Control(ControlAck {
                id,
                kind,
                status: ControlStatus::Applied,
                note: note.into(),
            }));
            if state.turn != turn_before {
                emitter.emit(AgentEvent::TurnStart { turn: state.turn });
            }
        }
        Some(pm) = provider_rx.recv() => {
            if pm.turn() == state.turn {
                let owned = pm.clone();
                state.finish_provider_msg(pm); // usage metered + Attempt appended here...
                match owned {
                    ProviderMsg::Partial { text, .. } => {
                        let id = match state.open_msg {
                            Some(id) => id,
                            None => {
                                let id = state.next_emit_id();
                                emitter.emit(AgentEvent::MessageStart {
                                    id,
                                    role: Role::Assistant,
                                    partial: Message {
                                        role: Role::Assistant,
                                        content: String::new(),
                                    },
                                });
                                state.open_msg = Some(id);
                                id
                            }
                        };
                        emitter.emit(AgentEvent::MessageUpdate {
                            id,
                            delta: MessageDelta {
                                kind: DeltaKind::Text,
                                text: Some(text.clone()),
                            },
                            partial: Message {
                                role: Role::Assistant,
                                content: text,
                            },
                        });
                    }
                    ProviderMsg::Settled { message, stop, .. } => {
                        // ...and Assistant + ToolCall appends inside step_claim...
                        let outcome = state.step_claim(message.clone(), stop);
                        emit_message_frames(state, &message, emitter); // ...before these frames.
                        if let ClaimOutcome::Dispatch(calls) = outcome {
                            for c in &calls {
                                emitter.emit(AgentEvent::ToolStart {
                                    id: c.call_id.clone(),
                                    name: c.name.clone(),
                                    args: c.args.clone(),
                                });
                            }
                        }
                    }
                    ProviderMsg::Failed { err, .. } => {
                        emitter.emit(AgentEvent::Error {
                            error: AgentError {
                                code: "provider-failed".into(),
                                message: err,
                            },
                        });
                    }
                }
            }
            // else: STALE GUARD, dropped without an event.
        }
        Some(tm) = tool_rx.recv() => {
            let shadow = tm.clone();
            if state.record_tool_result(tm) {
                // ToolResult appended above; terminal frame after.
                emitter.emit(AgentEvent::ToolEnd {
                    id: shadow.call_id,
                    result: Value::String(shadow.result.content),
                    is_error: shadow.result.is_error,
                });
            }
        }
        else => {}
    }
    let verdict = state.terminate();
    if state.turn != turn_before && matches!(verdict, PhaseVerdict::Continue) {
        emitter.emit(AgentEvent::TurnStart { turn: state.turn });
    }
    if let PhaseVerdict::Return(ref outcome) = verdict {
        let reason = outcome_log_reason(outcome, state.turn_reason.as_ref());
        state.stick_turn_reason(reason.clone());
        // Durable TurnEnd first, terminal frame second — never inverted.
        append_to(
            &mut state.items,
            ItemKind::TurnEnd {
                turn_id: turn_id(state.turn),
                reason: reason.clone(),
            },
        );
        emitter.emit(AgentEvent::TurnEnd {
            turn: state.turn,
            reason: turn_end_reason_to_event(&reason),
        });
    }
    verdict
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
            usage: None,
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
        assert_eq!(s.budget.counters().steps, 1);
        assert!(s.finish_provider_msg(settled(1)));
        assert!(s.start_provider_call(&root).is_some()); // slot freed
    }
    #[test]
    fn termination_order() {
        let mut s = LoopState::new(); // fresh parks, it does not exit
        assert!(matches!(s.terminate(), PhaseVerdict::Continue));
        let mut s = LoopState::new(); // cancel beats budget
        s.stop_hard = true;
        s.budget.counters_mut().steps = s.budget.config().max_steps.get();
        assert!(matches!(
            s.terminate(),
            PhaseVerdict::Return(Outcome::Cancelled)
        ));
        let mut s = LoopState::new(); // hard error beats budget
        s.fatal_error = Some("e".into());
        let max = s.budget.config().max_steps.get();
        s.budget.counters_mut().steps = max;
        assert!(matches!(
            s.terminate(),
            PhaseVerdict::Return(Outcome::Failed(_))
        ));
        let mut s = LoopState::new(); // budget beats done-shaped state
        s.stop_when_idle = true;
        let max = s.budget.config().max_steps.get();
        s.budget.counters_mut().steps = max;
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
        // guard halt labels surface verbatim
        let mut s = LoopState::new();
        let max = s.budget.config().max_steps.get();
        s.budget.counters_mut().steps = max;
        assert!(matches!(
            s.terminate(),
            PhaseVerdict::Return(Outcome::Halted(ref s)) if s == "steps"
        ));
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
        let mut emitter = Emitter::new();
        itx.send(Input::User("go".into())).await.unwrap();
        let verdict = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(verdict, PhaseVerdict::Continue));
        assert_eq!(s.turn, 1);
        itx.send(Input::StopHard).await.unwrap();
        let verdict = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(verdict, PhaseVerdict::Return(Outcome::Cancelled)));
    }

    #[tokio::test]
    async fn cancel_token_drives_stop() {
        let mut s = LoopState::new();
        let (_itx, mut irx) = mpsc::channel(8);
        let (_ptx, mut prx) = mpsc::channel(8);
        let (_ttx, mut trx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let mut emitter = Emitter::new();
        cancel.cancel();
        let verdict = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(verdict, PhaseVerdict::Return(Outcome::Cancelled)));
    }

    #[test]
    fn turn_end_reason_log_to_event_mapping() {
        use agent_event::TurnEndReason as EventReason;
        assert_eq!(
            turn_end_reason_to_event(&TurnEndReason::Completed),
            EventReason::Completed
        );
        assert_eq!(
            turn_end_reason_to_event(&TurnEndReason::Error("r".into())),
            EventReason::Error
        );
        assert_eq!(
            turn_end_reason_to_event(&TurnEndReason::Interrupted),
            EventReason::Aborted
        );
        assert_eq!(
            turn_end_reason_to_event(&TurnEndReason::Budget),
            EventReason::BudgetExceeded
        );
        assert_eq!(
            turn_end_reason_to_event(&TurnEndReason::MaxTokens),
            EventReason::MaxSteps
        );
    }

    #[test]
    fn crash_input_queues_like_user_with_crash_source() {
        let mut s = LoopState::new();
        s.apply_input(Input::Crash("boom state".into()));
        assert_eq!((s.turn, s.phase), (1, Phase::Running));
        assert_eq!(s.admit_steering(), 1);
        match &s.items[1].kind {
            ItemKind::Input { source, text, .. } => {
                assert_eq!(*source, InputSource::Crash);
                assert_eq!(text, "boom state");
            }
            other => panic!("expected Crash Input, got {other:?}"),
        }
        // Running-phase crash also just queues.
        s.apply_input(Input::Crash("again".into()));
        assert_eq!(s.steering.len(), 1);
        assert_eq!(s.steering[0].source, InputSource::Crash);
    }

    #[test]
    fn lessons_cap_at_three() {
        let mut s = LoopState::new();
        for i in 0..4 {
            s.push_lesson(format!("lesson-{i}"));
        }
        assert_eq!(s.lessons.len(), 3);
        assert!(!s.lessons.iter().any(|l| l == "lesson-0"));
        assert_eq!(s.lessons[2], "lesson-3");
    }

    #[test]
    fn budget_nudge_appends_to_tool_result_tail_once() {
        let mut s = LoopState::new();
        s.turn = 1;
        let outcome = s.step_claim(assistant(vec![call("a")]), StopReason::ToolUse);
        assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
        assert!(s.record_tool_result(ToolMsg {
            call_id: "a".into(),
            result: result(false),
        }));
        let warn = s.budget.config().warn_steps.get();
        s.budget.counters_mut().steps = warn;
        let text = s.apply_budget_nudge().expect("nudge due at warn");
        assert!(text.contains("wrap up"));
        match s.items.last().map(|i| &i.kind) {
            Some(ItemKind::ToolResult { content, .. }) => assert!(content.contains(&text)),
            other => panic!("nudge must land on the ToolResult tail, got {other:?}"),
        }
        assert!(s.apply_budget_nudge().is_none()); // latched
    }

    #[test]
    fn cancelled_provider_msg_refunds_bounded() {
        let mut s = LoopState::new();
        s.turn = 1;
        let root = CancellationToken::new();
        assert!(s.start_provider_call(&root).is_some());
        assert_eq!(s.budget.counters().steps, 1);
        assert!(s.finish_provider_msg(ProviderMsg::Failed {
            turn: 1,
            err: "cancelled".into(),
            cancelled: true,
        }));
        assert_eq!(s.budget.counters().steps, 0);
        assert_eq!(s.budget.counters().refunds, 1);
        // Floor holds: refunding an empty counter mints nothing.
        s.budget.refund_step();
        assert_eq!(
            (s.budget.counters().steps, s.budget.counters().refunds),
            (0, 1)
        );
    }

    struct FakeEdit;

    #[async_trait::async_trait]
    impl tool_core::Tool for FakeEdit {
        fn definition(&self) -> tool_core::ToolDefinition {
            tool_core::ToolDefinition {
                name: "edit".into(),
                description: "fake edit".into(),
                schema: serde_json::json!({}),
            }
        }
        fn prepare(&self, call: &ToolCall) -> tool_core::CallStatus {
            tool_core::CallStatus::Dispatch(tool_core::Invocation {
                call_id: call.call_id.clone(),
                name: call.name.clone(),
                args: call.args.clone(),
            })
        }
        async fn execute(
            &self,
            inv: tool_core::Invocation,
            _cancel: CancellationToken,
        ) -> Result<tool_core::ToolOutcome, tool_core::ToolError> {
            Ok(tool_core::ToolOutcome {
                content: format!("edited {}", inv.call_id),
                truncated: false,
            })
        }
    }

    fn edit_registry() -> tool_core::Registry {
        let gate = std::sync::Arc::new(tool_core::GrantGate::new(
            [("agent".to_string(), vec!["edit".to_string()])].into(),
        ));
        let mut r = tool_core::Registry::new(gate);
        r.register(std::sync::Arc::new(FakeEdit));
        r
    }

    fn tool_use_message() -> AssistantMessage {
        AssistantMessage {
            content: "fixing".into(),
            tool_calls: vec![provider_core::ToolCallRef {
                id: "c1".into(),
                name: "edit".into(),
                args: serde_json::json!({"path": "f"}),
            }],
            thinking: None,
        }
    }

    fn usage() -> Usage {
        Usage {
            input: 100,
            output: 50,
            cache_read: 0,
            cache_write: 0,
            reasoning: None,
            cost_usd: Some(0.02),
        }
    }

    fn event_order(history: &[AgentEvent]) -> Vec<&'static str> {
        history
            .iter()
            .map(|e| match e {
                AgentEvent::TurnStart { .. } => "TurnStart",
                AgentEvent::MessageStart { .. } => "MessageStart",
                AgentEvent::MessageUpdate { .. } => "MessageUpdate",
                AgentEvent::MessageEnd { .. } => "MessageEnd",
                AgentEvent::ToolStart { .. } => "ToolStart",
                AgentEvent::ToolEnd { .. } => "ToolEnd",
                AgentEvent::TurnEnd { .. } => "TurnEnd",
                AgentEvent::Control(_) => "Control",
                _ => "other",
            })
            .collect()
    }

    #[tokio::test]
    async fn e2e_full_turn_tool_call_orders_log_before_events() {
        use agent_event::check_pairing;
        let mut s = LoopState::new();
        let mut emitter = Emitter::new();
        let (itx, mut irx) = mpsc::channel(8);
        let (ptx, mut prx) = mpsc::channel(8);
        let (ttx, mut trx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let registry = edit_registry();

        itx.send(Input::User("build it".into())).await.unwrap();
        let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(v, PhaseVerdict::Continue));
        s.admit_steering(); // pre-boundary: the only place steering enters the log
        let root = CancellationToken::new();
        assert!(s.start_provider_call(&root).is_some()); // step head: may_step + record
        ptx.send(ProviderMsg::Partial {
            turn: 1,
            text: "thinking ".into(),
        })
        .await
        .unwrap();
        let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(v, PhaseVerdict::Continue));
        ptx.send(ProviderMsg::Settled {
            turn: 1,
            message: tool_use_message(),
            stop: StopReason::ToolUse,
            usage: Some(usage()),
        })
        .await
        .unwrap();
        let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(v, PhaseVerdict::Continue));
        // Pre-effect: the ToolCall row is durable before anything executes.
        let call_seq = s
            .items
            .iter()
            .position(|i| matches!(&i.kind, ItemKind::ToolCall { call_id, .. } if call_id == "c1"))
            .expect("ToolCall logged at claim");
        let prepared = registry.prepare(
            "agent",
            ToolCall {
                call_id: "c1".into(),
                name: "edit".into(),
                args: serde_json::json!({"path": "f"}),
            },
        );
        let inv = match prepared {
            tool_core::CallStatus::Dispatch(inv) => inv,
            tool_core::CallStatus::Result(r) => panic!("gate must allow the fake: {}", r.content),
        };
        let outcome = registry
            .resolve("edit")
            .unwrap()
            .execute(inv, CancellationToken::new())
            .await
            .expect("fake tool runs");
        ttx.send(ToolMsg {
            call_id: "c1".into(),
            result: ToolResult {
                content: outcome.content,
                is_error: false,
                terminate: true, // unanimous batch concludes the turn
            },
        })
        .await
        .unwrap();
        let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(v, PhaseVerdict::Return(Outcome::Done)));

        // Events arrive ordered; every Start pairs with its End.
        let order = event_order(emitter.history());
        for pair in [
            ("TurnStart", "MessageStart"),
            ("MessageStart", "MessageEnd"),
            ("MessageEnd", "ToolStart"),
            ("ToolStart", "ToolEnd"),
            ("ToolEnd", "TurnEnd"),
        ] {
            let (a, b) = (
                order.iter().position(|k| *k == pair.0),
                order.iter().position(|k| *k == pair.1),
            );
            assert!(a < b, "{pair:?} out of order in {order:?}");
        }
        assert!(order.contains(&"Control")); // the steer ack rides the stream
        assert!(check_pairing(emitter.history()));
        // Partial + settle share one message id: a single Start, two Updates.
        assert_eq!(order.iter().filter(|k| **k == "MessageStart").count(), 1);
        assert_eq!(order.iter().filter(|k| **k == "MessageUpdate").count(), 2);
        // Log rows precede their frames, in transcript order.
        let kinds: Vec<&str> = s
            .items
            .iter()
            .map(|i| match &i.kind {
                ItemKind::TurnStart { .. } => "TurnStart",
                ItemKind::Input { .. } => "Input",
                ItemKind::Assistant { .. } => "Assistant",
                ItemKind::ToolCall { .. } => "ToolCall",
                ItemKind::ToolResult { .. } => "ToolResult",
                ItemKind::TurnEnd { .. } => "TurnEnd",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "TurnStart",
                "Input",
                "Assistant",
                "ToolCall",
                "ToolResult",
                "TurnEnd"
            ]
        );
        let result_seq = s
            .items
            .iter()
            .position(|i| matches!(&i.kind, ItemKind::ToolResult { .. }))
            .unwrap();
        assert!(call_seq < result_seq); // pre-effect, never inverted
                                        // Budget consumed from the fake Usage.
        assert_eq!(s.budget.counters().steps, 1);
        assert_eq!(s.budget.counters().tokens, 150);
        assert_eq!(s.budget.counters().spent_cents, 2);
    }

    #[tokio::test]
    async fn e2e_truncation_halts_with_no_tool_start() {
        let mut s = LoopState::new();
        let mut emitter = Emitter::new();
        let (_itx, mut irx) = mpsc::channel(8);
        let (ptx, mut prx) = mpsc::channel(8);
        let (_ttx, mut trx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        s.apply_input(Input::User("go".into()));
        s.admit_steering();
        let root = CancellationToken::new();
        assert!(s.start_provider_call(&root).is_some());
        ptx.send(ProviderMsg::Settled {
            turn: 1,
            message: tool_use_message(),
            stop: StopReason::MaxTokens,
            usage: None,
        })
        .await
        .unwrap();
        let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(v, PhaseVerdict::Return(Outcome::Halted(ref s)) if s == "max-tokens"));
        // Truncated batch: answered with errors, never dispatched.
        let order = event_order(emitter.history());
        assert!(!order.contains(&"ToolStart"));
        assert!(order.contains(&"TurnEnd"));
        match emitter.history().last().unwrap() {
            AgentEvent::TurnEnd { reason, .. } => {
                assert_eq!(*reason, agent_event::TurnEndReason::MaxSteps)
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
        assert_eq!(s.turn_reason, Some(TurnEndReason::MaxTokens));
    }

    #[tokio::test]
    async fn e2e_budget_halt_after_grace_fires_once() {
        let mut s = LoopState::new();
        let mut emitter = Emitter::new();
        let (itx, mut irx) = mpsc::channel(8);
        let (_ptx, mut prx) = mpsc::channel(8);
        let (_ttx, mut trx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let max = s.budget.config().max_steps.get();
        s.budget.counters_mut().steps = max;
        // Grace fires exactly once through the loop's guard.
        assert!(!s.budget.grace_used());
        assert!(s.budget.may_step().is_ok());
        assert!(s.budget.grace_used());
        assert!(s.budget.may_step().is_err());
        // Step head now refuses; the tick halts with a BudgetExceeded frame.
        let root = CancellationToken::new();
        assert!(s.start_provider_call(&root).is_none());
        // A live inbox send drives the tick (a senders-dropped tick stalls here).
        itx.send(Input::User("too late".into())).await.unwrap();
        let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(v, PhaseVerdict::Return(Outcome::Halted(ref s)) if s == "steps"));
        match emitter.history().last().unwrap() {
            AgentEvent::TurnEnd { reason, .. } => {
                assert_eq!(*reason, agent_event::TurnEndReason::BudgetExceeded)
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
    }
}
