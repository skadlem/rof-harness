//! Turn/step driver. See research/crate-agent-loop.md.
//!
//! [`run`] is the headless multi-tick assembly: sequential `complete` +
//! inline tool execute over the same step-head and termination order as
//! [`drive_tick`]. Channel-driven traffic still enters as scripted
//! [`ProviderMsg`] / [`ToolMsg`] fakes into [`drive_tick`] for select!-shape
//! tests.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use agent_budget::{config_for, halt_name, BudgetGuard, BudgetHalt, Capability, Nudge};
use agent_event::{
    AgentError, AgentEvent, ControlAck, ControlKind, ControlStatus, DeltaKind, Emitter, Message,
    MessageDelta, Role, RunOutcome as EventRunOutcome, TurnEndReason as EventTurnEndReason,
    UsageReport,
};
use agent_log::{InputSource, Item, ItemKind, LogWriter, RecoveryCode, TurnEndReason, LOG_VERSION};
use provider_core::{
    AssistantMessage, LlmClient, LlmError, ProviderMessage, Request, StopReason, Thinking, Usage,
};
use serde_json::Value;
use tool_core::{ToolCall, ToolResult};

/// Queued directives kept before the oldest is dropped (drop-oldest: the
/// newest counter warning outranks stale advice, mirroring the lessons cap).
const DIRECTIVE_CAP: usize = 3;

/// Assistant rows whose echoed reasoning survives folding into the derived
/// transcript.
const THINKING_KEEP: usize = 2;

/// Model-facing notice when one failed tool reverts the whole batch.
const ROLLBACK_NOTICE: &str = "a tool in your last batch failed and the whole batch was reverted — your successful changes in it are gone; re-apply them";

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
        /// Usage the failed attempts carried (summed by the adapter). Metered
        /// by `finish_provider_msg`: a failed ladder is still billed spend.
        usage: Option<Usage>,
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
    /// File map frozen at run start (prefix-cache head): mid-run edits must
    /// not rotate the system-message bytes, so `build_request` never re-walks.
    pub file_map: Option<String>,
    /// Run-start pin contents (`path`, bytes) for `cfg.context_files` —
    /// `file_map`'s rule applied to named files. Re-reading a pin per request
    /// re-emits its bytes: after the model edits that file the bytes differ,
    /// rotating the cached prefix and restating a file the `edit` result
    /// already carried. Frozen once, compared per request: unchanged -> cached
    /// run-start bytes, changed -> dropped to the normal view/edit flow.
    pub pins: Option<Vec<(String, String)>>,
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
    /// Live running totals for `TurnEnd.usage_totals`, cumulative over the
    /// run; the durable log vocabulary is unchanged.
    pub usage_totals: UsageReport,
    pub emit_next: u64,
    pub open_msg: Option<u64>,
    pub step_retries: u32,
    pub fatal_error: Option<String>,
    pub last_sig: String,
    pub last_obs: u64,
    pub lessons: VecDeque<String>,
    /// Model-facing directives from the action counters, delivered onto the
    /// newest ToolResult tail; nothing is consumed until a tail exists.
    pub pending_directives: VecDeque<String>,
    /// Successful `edit`|`write` tool results so far (the zero-edit trigger).
    pub edits: u32,
    /// One-shot latches: the half-cap and near-cap directives fire once each.
    pub half_directive_sent: bool,
    pub late_directive_sent: bool,
    pub drain_timeout: Duration,
    pub drain_until: Option<Instant>,
}

impl LoopState {
    pub fn new() -> Self {
        Self {
            turn: 0,
            step: 0,
            phase: Phase::Idle,
            items: Vec::new(),
            file_map: None,
            pins: None,
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
            usage_totals: UsageReport::default(),
            emit_next: 0,
            open_msg: None,
            step_retries: 2,
            fatal_error: None,
            last_sig: String::new(),
            last_obs: 0,
            lessons: VecDeque::new(),
            pending_directives: VecDeque::new(),
            edits: 0,
            half_directive_sent: false,
            late_directive_sent: false,
            drain_timeout: Duration::from_secs(30),
            drain_until: None,
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
        // Prior batch results belong to older turns; drop them so the batch
        // shape only ever sees the current batch.
        self.tool_calls.retain(|_, c| c.result.is_none());
        Some(token)
    }

    /// Returns false when the message is stale (wrong turn) and was dropped.
    /// Spend/tokens land here from provider Usage: steps at the step head,
    /// tokens + spend on settle and on a metered failure. Tokens/spend are
    /// never refunded.
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
        if let ProviderMsg::Failed {
            err,
            cancelled,
            usage,
            ..
        } = msg
        {
            // Failed attempts are billed too: meter before the refund/retry
            // fork so an exhausted ladder's re-sends are never invisible.
            if let Some(u) = &usage {
                self.record_usage(u);
            }
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

    /// Tokens meter every settle and every metered failure; spend converts
    /// cost_usd to cents. The same reading folds into the live running totals
    /// (`TurnEnd.usage_totals`).
    pub fn record_usage(&mut self, usage: &Usage) {
        self.budget.record_tokens(usage.total_tokens());
        if let Some(cost) = usage.cost_usd {
            self.budget
                .record_spend_cents((cost * 100.0).round().max(0.0) as u64);
        }
        let totals = &mut self.usage_totals;
        totals.input_tokens = totals.input_tokens.saturating_add(usage.input);
        totals.output_tokens = totals.output_tokens.saturating_add(usage.output);
        totals.cache_read_tokens = totals.cache_read_tokens.saturating_add(usage.cache_read);
        if let Some(r) = usage.reasoning {
            totals.reasoning_tokens = Some(totals.reasoning_tokens.unwrap_or(0).saturating_add(r));
        }
        if let Some(c) = usage.cost_usd {
            totals.cost_usd = Some(totals.cost_usd.unwrap_or(0.0) + c);
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
                            }),
                        },
                    );
                }
                let n = message.tool_calls.len();
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
        if self.batch_complete() {
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

    /// One queued directive, capped at [`DIRECTIVE_CAP`] (drop-oldest).
    pub fn push_directive(&mut self, text: String) {
        if self.pending_directives.len() >= DIRECTIVE_CAP {
            self.pending_directives.pop_front();
        }
        self.pending_directives.push_back(text);
    }

    /// Action-counter directives, checked after every recorded action so the
    /// text rides that action's own not-yet-synced row. One-shot per run.
    pub fn queue_directives(&mut self) {
        let cap = self.budget.config().actions_per_trial;
        let actions = self.budget.counters().actions_this_trial;
        if cap == 0 {
            return;
        }
        if !self.half_directive_sent && actions >= cap / 2 && self.edits == 0 {
            self.half_directive_sent = true;
            self.push_directive(format!(
                "0 edits so far after {actions} actions. Stop reading. Apply your first edit with the edit tool NOW."
            ));
        }
        if !self.late_directive_sent && actions >= cap * 4 / 5 {
            self.late_directive_sent = true;
            self.push_directive(format!(
                "only {} actions remain before the run is stopped. Finish and submit your patch now.",
                cap - actions
            ));
        }
    }

    /// Deliver queued directives and each undelivered lesson onto the newest
    /// ToolResult tail, one text per line; nothing is consumed while no tail
    /// exists, so the next batch retries. The carried texts become part of
    /// that durable ToolResult row — no synthetic row, no new `ItemKind`.
    /// Returns how many texts landed.
    pub fn deliver_directives(&mut self) -> usize {
        if !self.has_tool_tail() {
            return 0;
        }
        let mut delivered = 0;
        while let Some(text) = self.pending_directives.pop_front() {
            self.append_to_tail(&text);
            delivered += 1;
        }
        while let Some(lesson) = self.lessons.pop_front() {
            self.append_to_tail(&lesson);
            delivered += 1;
        }
        delivered
    }

    /// Newest ToolResult row: the one mutable delivery surface. Appends must
    /// happen before that row is synced, so the durable log carries exactly
    /// the text the model saw and replay from the file stays faithful.
    fn has_tool_tail(&self) -> bool {
        self.items
            .iter()
            .rev()
            .any(|i| matches!(i.kind, ItemKind::ToolResult { .. }))
    }

    fn append_to_tail(&mut self, text: &str) {
        let tail = self
            .items
            .iter_mut()
            .rev()
            .find(|i| matches!(i.kind, ItemKind::ToolResult { .. }));
        if let Some(Item {
            kind: ItemKind::ToolResult { content, .. },
            ..
        }) = tail
        {
            content.push('\n');
            content.push_str(text);
        }
    }

    fn next_emit_id(&mut self) -> u64 {
        let id = self.emit_next;
        self.emit_next += 1;
        id
    }

    /// One-shot wrap-up notice. Caller-append contract: the text lands on the
    /// newest ToolResult tail in place, never as a synthetic user/system row.
    /// With no tail the nudge stays latched (not burned) and returns `None`,
    /// so a later batch can still deliver it.
    pub fn apply_budget_nudge(&mut self) -> Option<String> {
        if !self.has_tool_tail() {
            return None;
        }
        let Nudge::WrapUp(text) = self.budget.nudge_due()?;
        self.append_to_tail(&text);
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
            return PhaseVerdict::Return(Outcome::Halted(halt_name(&exceeded.halt).into()));
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
        if self.budget.counters().same_action_streak >= self.budget.config().same_action_cycles {
            self.push_lesson("same action repeated without progress; vary the approach".into());
        }
        // 5. Semantic termination: drained turn, empty queues, follow-up poll.
        // The old turn closes here (sticky reason or Completed) so the log
        // never holds two open turns; the new turn opens right after.
        if self.in_flight.is_none() && self.open_tools() == 0 && self.steering.is_empty() {
            if let Some(text) = self.followups.pop_front() {
                let reason = self.turn_reason.take().unwrap_or(TurnEndReason::Completed);
                append_to(
                    &mut self.items,
                    ItemKind::TurnEnd {
                        turn_id: turn_id(self.turn),
                        reason,
                    },
                );
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
    /// Collapse-5 over tool observations (context economics): the last
    /// `COLLAPSE_KEEP` tool results go verbatim, older ones shrink to a
    /// `[collapsed: Nb — re-open to edit]` stub plus their first 120 chars as
    /// folded. Stable order preserved, so prefix caches survive.
    ///
    /// Echoed reasoning is trimmed to the last [`THINKING_KEEP`] assistant
    /// rows: on a thinking-heavy trace it was 64% of input (measured: 128k of
    /// 224k tokens), and keeping the last two cuts that ~68%. Wire key
    /// presence is unaffected — the wire layer still emits
    /// `reasoning_content: ""` for these rows in a thinking-mode
    /// conversation. chosen-not-measured: dropping older reasoning may
    /// degrade cross-turn reasoning continuity; the A/B is pending.
    pub fn derived_messages(&self) -> Vec<ProviderMessage> {
        let mut out = Vec::new();
        for item in &self.items {
            match &item.kind {
                ItemKind::Input { text, .. } => out.push(ProviderMessage {
                    role: "user".into(),
                    content: text.clone(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    thinking: None,
                }),
                ItemKind::Assistant { message, .. } => {
                    // Stored as JSON: recover structured calls for strict providers.
                    let am: AssistantMessage =
                        serde_json::from_value(message.clone()).unwrap_or(AssistantMessage {
                            content: message_content(message),
                            tool_calls: Vec::new(),
                            thinking: None,
                        });
                    out.push(ProviderMessage {
                        role: "assistant".into(),
                        content: am.content,
                        tool_calls: am.tool_calls,
                        tool_call_id: None,
                        // Thinking rides the stored JSON; the trim below
                        // blanks all but the last 2, and the wire layer
                        // keeps the mandatory `reasoning_content` key present.
                        thinking: am.thinking,
                    })
                }
                ItemKind::ToolResult {
                    call_id, content, ..
                } => out.push(ProviderMessage {
                    role: "tool".into(),
                    content: content.clone(),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(call_id.clone()),
                    thinking: None,
                }),
                _ => {}
            }
        }
        // Echoed reasoning is trimmed here, before the wire layer sees it:
        // keep the last THINKING_KEEP assistant rows, blank the older ones.
        let keep_from = out
            .iter()
            .filter(|m| m.role == "assistant")
            .count()
            .saturating_sub(THINKING_KEEP);
        for (n, m) in out.iter_mut().filter(|m| m.role == "assistant").enumerate() {
            if n < keep_from {
                m.thinking = None;
            }
        }
        let tool_idx: Vec<usize> = out
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == "tool")
            .map(|(i, _)| i)
            .collect();
        if tool_idx.len() > context::COLLAPSE_KEEP {
            for &i in &tool_idx[..tool_idx.len() - context::COLLAPSE_KEEP] {
                // Bytes at fold time + a 120-char head: enough to tell the
                // observation apart from a re-read of the file.
                let bytes = out[i].content.len();
                let head: String = out[i].content.chars().take(120).collect();
                out[i].content = format!("[collapsed: {bytes}b — re-open to edit] {head}");
            }
        }
        out
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
    // 1-based: the file-backed run log validates gapless-from-1 under a Header.
    let seq = items.len() as u64 + 1;
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

/// Provider settle usage → the live-event payload. `cache_write` stays out:
/// the live vocabulary names only what consumers read.
fn usage_report(u: &Usage) -> UsageReport {
    UsageReport {
        input_tokens: u.input,
        output_tokens: u.output,
        cache_read_tokens: u.cache_read,
        reasoning_tokens: u.reasoning,
        cost_usd: u.cost_usd,
    }
}

/// Emit one assistant message frame set: Start, one full-content Update, End.
/// Reuses the open Partial id when a stream preceded the settle.
fn emit_message_frames(
    state: &mut LoopState,
    message: &AssistantMessage,
    usage: Option<&Usage>,
    emitter: &mut Emitter,
) {
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
        usage: usage.map(usage_report),
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
                    ProviderMsg::Settled {
                        message,
                        stop,
                        usage,
                        ..
                    } => {
                        // ...and Assistant + ToolCall appends inside step_claim...
                        let outcome = state.step_claim(message.clone(), stop);
                        emit_message_frames(state, &message, usage.as_ref(), emitter); // ...before these frames.
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
    // turn_before > 0: the 0 -> 1 opening has no prior turn to close.
    if state.turn != turn_before && turn_before > 0 && matches!(verdict, PhaseVerdict::Continue) {
        // Same hop as run(): closer is durable already, frames follow in order.
        emitter.emit(AgentEvent::TurnEnd {
            turn: turn_before,
            reason: hopped_turn_reason(state, turn_before),
            usage_totals: state.usage_totals.clone(),
        });
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
            usage_totals: state.usage_totals.clone(),
        });
    }
    verdict
}

/// Bets hook with a no-op default: loop compiles and ships with bets disabled.
///
/// Receivers are `&self` (the contract's `&mut self` does not fit `Run::bets`
/// staying `&dyn`, which the frozen `rof` call site requires); hooks that need
/// mutable state use interior mutability, as the test hook `RecBets` does.
pub trait BetsHook: Send + Sync {
    fn on_step(&self) -> PhaseVerdict {
        PhaseVerdict::Continue
    }
    /// Site 1 (step head): `step` is `LoopState::step` as the head is reached
    /// (steps started so far). Defaults to [`on_step`](Self::on_step), so hooks
    /// implementing only `on_step` keep firing at both sites.
    fn on_step_head(&self, _step: u64) -> PhaseVerdict {
        self.on_step()
    }
    /// Site 2 (post-batch): proof-gated commit over the batch's patch fragments
    /// (`(hunk, proof_passed)`; the proof flag is uniform today — the batch's
    /// tool results all passed — per-hunk incremental proof is the later
    /// ablation). Permissive default = feature off unless a hook opts in:
    /// `Committed` leaves the batch as the existing path left it, `Partial`
    /// restores `savepoint.kept_hunks`, `Aborted` rolls the batch back.
    fn on_post_batch(
        &self,
        _claim: &bets::Claim,
        _hunks: &[(String, bool)],
    ) -> bets::CommitVerdict {
        bets::CommitVerdict::Committed
    }
}

pub struct NoBets;

impl BetsHook for NoBets {}

// --- multi-tick run() assembly (headless; sequential, no channels) ---

/// Headless run knobs. `drain_timeout` defaults to 30s (unattended runs must
/// not wedge on a stuck tool); the context budget reuses v1's 24k evidence
/// window until the histogram retunes it.
#[derive(Debug, Clone)]
pub struct RunConfig {
    pub goal: String,
    pub model: String,
    pub max_tokens: usize,
    pub context_budget_chars: usize,
    pub context_files: Vec<String>,
    pub drain_timeout: Duration,
    pub log_path: Option<PathBuf>,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            goal: "run".into(),
            model: "run-model".into(),
            max_tokens: 2000,
            context_budget_chars: 24_000,
            context_files: Vec::new(),
            drain_timeout: Duration::from_secs(30),
            log_path: None,
        }
    }
}

/// Everything `run` borrows: real siblings except the provider (generic over
/// [`LlmClient` so tests use scripted fakes) and bets (local [`BetsHook`]
/// with [`NoBets`] as the green default).
pub struct Run<'a, P> {
    pub provider: &'a P,
    pub registry: &'a tool_core::Registry,
    pub agent: &'a str,
    pub workdir: &'a Path,
    pub emitter: &'a mut Emitter,
    pub bets: &'a dyn BetsHook,
    pub cfg: RunConfig,
}

/// Append-only tail sync: the vec is the mirror, the file is the record.
/// A failed pre-effect append is a hard failure (never run from
/// process-only state).
fn sync_log(
    items: &[Item],
    writer: Option<&mut LogWriter>,
    synced: &mut usize,
) -> std::io::Result<()> {
    let Some(w) = writer else { return Ok(()) };
    while *synced < items.len() {
        w.append(&items[*synced])?;
        *synced += 1;
    }
    Ok(())
}

fn event_outcome(outcome: &Outcome) -> EventRunOutcome {
    match outcome {
        Outcome::Done => EventRunOutcome::Passed,
        Outcome::Halted(s) | Outcome::Failed(s) => EventRunOutcome::Failed(s.clone()),
        Outcome::Cancelled => EventRunOutcome::Aborted,
    }
}

/// Durable TurnEnd first, terminal frames second — never inverted. The emit
/// still runs when the sync fails so live always sees a terminal frame.
fn finish_run(
    state: &mut LoopState,
    writer: Option<&mut LogWriter>,
    emitter: &mut Emitter,
    run_id: u64,
    synced: &mut usize,
    outcome: Outcome,
) -> Outcome {
    let reason = outcome_log_reason(&outcome, state.turn_reason.as_ref());
    state.stick_turn_reason(reason.clone());
    append_to(
        &mut state.items,
        ItemKind::TurnEnd {
            turn_id: turn_id(state.turn),
            reason: reason.clone(),
        },
    );
    let _ = sync_log(&state.items, writer, synced);
    emitter.emit(AgentEvent::TurnEnd {
        turn: state.turn,
        reason: turn_end_reason_to_event(&reason),
        usage_totals: state.usage_totals.clone(),
    });
    emitter.emit(AgentEvent::RunEnd {
        outcome: event_outcome(&outcome),
        messages: Vec::new(),
    });
    let _ = run_id;
    outcome
}

/// The durable closer for a hopped turn, read back from the log so live and
/// replay agree exactly.
fn hopped_turn_reason(state: &LoopState, before: u64) -> EventTurnEndReason {
    state
        .items
        .iter()
        .rev()
        .find_map(|i| match &i.kind {
            ItemKind::TurnEnd {
                turn_id: tid,
                reason,
            } if tid == &turn_id(before) => Some(turn_end_reason_to_event(reason)),
            _ => None,
        })
        .unwrap_or(EventTurnEndReason::Completed)
}

/// Follow-up opened a new turn mid-`terminate` (closer already appended
/// there): sync it, then emit its frame before the new TurnStart. False
/// means the durable append failed and the caller must fail closed.
fn close_hopped_turn(
    state: &mut LoopState,
    writer: Option<&mut LogWriter>,
    emitter: &mut Emitter,
    synced: &mut usize,
    before: u64,
) -> bool {
    if state.turn == before || before == 0 {
        return true;
    }
    if sync_log(&state.items, writer, synced).is_err() {
        return false;
    }
    emitter.emit(AgentEvent::TurnEnd {
        turn: before,
        reason: hopped_turn_reason(state, before),
        usage_totals: state.usage_totals.clone(),
    });
    emitter.emit(AgentEvent::TurnStart { turn: state.turn });
    true
}

/// Static workflow contract: the read→edit→finish obligations the transcript
/// alone does not state. Byte-identical for the whole run. Live budget digits
/// must not appear here: the system message is the cached-prefix head, and a
/// changing head invalidates the provider's prefix cache every request. The
/// counters ride the request tail instead (see [`build_request`]).
const WORKFLOW_CONTRACT: &str = "WORKFLOW CONTRACT\n\
    budgets remaining are printed on the last line of the newest message; read them there.\n\
    tool results older than the last 5 are collapsed to one line; re-open a file immediately before editing it.\n\
    work file-by-file: view -> edit immediately -> next file. `edit` and `write` are the ONLY patch mechanisms — never write files via exec; exec/test are for checks only.\n\
    when your patch is complete, reply with a text message and NO tool calls — that finishes the run.";

/// Live counters, request-scoped: appended to the final outgoing message after
/// the transcript is derived, so they are never persisted.
fn budget_line(state: &LoopState) -> String {
    let cfg = state.budget.config();
    let counters = state.budget.counters();
    format!(
        "budgets remaining: steps {}/{}; actions {}/{}; tokens {}/{}",
        cfg.max_steps.get().saturating_sub(counters.steps),
        cfg.max_steps,
        cfg.actions_per_trial
            .saturating_sub(counters.actions_this_trial),
        cfg.actions_per_trial,
        cfg.max_tokens.saturating_sub(counters.tokens),
        cfg.max_tokens,
    )
}

/// Named files are pinned verbatim up to this cap; snapshot and comparison
/// must read the same window or an unchanged pin would look edited.
const PIN_CAP_CHARS: usize = 8000;

/// Run-start pin contents: one read per named file, reused for the whole run
/// (see [`LoopState::pins`]). Unreadable files are simply not pinned.
fn pin_snapshot(files: &[String], workdir: &Path) -> Vec<(String, String)> {
    files
        .iter()
        .filter_map(|f| {
            context::named_file_contents(workdir, f, PIN_CAP_CHARS)
                .ok()
                .map(|c| (f.clone(), c))
        })
        .collect()
}

/// Prompt build: static workflow contract + run-start file map (cached-prefix
/// head) + named files as volatiles (delivered last), fitted to
/// `context_budget_chars` in the system string. Every byte of that head is
/// frozen for the run — DeepSeek-style prefix caching only fires on
/// byte-identical prefixes. Named files freeze the same way ([`LoopState::pins`])
/// and drop out of the request if their file changed since run start. The live
/// budget line is the sole per-request variation: it is appended to the last
/// outgoing message only and never to the transcript, so the durable log stays
/// exactly what was recorded. History is delivered exactly once, raw, as
/// messages (collapse-5 rides [`LoopState::derived_messages`]) — never fitted
/// into the system copy. Never summarizes.
fn build_request(
    state: &mut LoopState,
    registry: &tool_core::Registry,
    workdir: &Path,
    cfg: &RunConfig,
) -> Request {
    let mut asm = context::ContextAssembler::new(cfg.context_budget_chars);
    asm.add(context::ContextItem {
        key: context::ItemKey {
            path: "workflow-contract".into(),
            region: "contract".into(),
            role: "system".into(),
        },
        fidelity: context::Fidelity::Exact,
        must_include: true,
        text: WORKFLOW_CONTRACT.into(),
    });
    // Frozen by `run` at start; the lazy fallback keeps direct callers honest.
    let map_text = state
        .file_map
        .get_or_insert_with(|| context::file_map(workdir, 200).join("\n"))
        .clone();
    asm.add(context::ContextItem {
        key: context::ItemKey {
            path: "file-map".into(),
            region: "map".into(),
            role: "system".into(),
        },
        fidelity: context::Fidelity::Exact,
        must_include: true,
        text: map_text,
    });
    for (f, start) in state
        .pins
        .get_or_insert_with(|| pin_snapshot(&cfg.context_files, workdir))
        .iter()
    {
        // Dropped pins are not errors: `view`/`edit` carry the current bytes.
        let unchanged = context::named_file_contents(workdir, f, PIN_CAP_CHARS)
            .map(|c| c == *start)
            .unwrap_or(false);
        if unchanged {
            asm.add_volatile(context::ContextItem {
                key: context::ItemKey {
                    path: f.clone(),
                    region: "named".into(),
                    role: "system".into(),
                },
                fidelity: context::Fidelity::Exact,
                must_include: true,
                text: start.clone(),
            });
        }
    }
    let mut messages = vec![ProviderMessage {
        role: "system".into(),
        content: asm.assemble(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        thinking: None,
    }];
    messages.extend(state.derived_messages()); // once: raw history, collapse-5 intact

    // Prefix-cache tail: the only per-request bytes. Never persisted.
    if let Some(last) = messages[1..].last_mut() {
        last.content.push('\n');
        last.content.push_str(&budget_line(state));
    }
    Request {
        messages,
        tools: registry.definitions(),
        max_tokens: cfg.max_tokens,
        thinking: Thinking::Auto,
        extras: Value::Null,
    }
}

/// Split `TreeService::patch` text into the fragments
/// `TreeService::restore_hunks` accepts: one file header (`diff --git` /
/// `index` / `---` / `+++`) directly followed by exactly one `@@` hunk — the
/// same companion splitter the snapshot tests exercise. Untracked-file
/// evidence (appended without an `@@`) rides the trailing fragment's tail; a
/// restore that hits it fails closed to baseline. On a truncated patch the
/// final fragment may be incomplete, so it is dropped.
fn split_patch(patch: &snapshot::PatchText) -> Vec<String> {
    let mut out = Vec::new();
    let mut header = String::new();
    let mut cur: Option<String> = None;
    for line in patch.text.lines() {
        if line.starts_with("diff --git") {
            if let Some(done) = cur.take() {
                out.push(done);
            }
            header = line.to_string();
        } else if line.starts_with("@@") {
            if let Some(done) = cur.take() {
                out.push(done);
            }
            cur = Some(format!("{header}\n{line}"));
        } else if let Some(hunk) = cur.as_mut() {
            hunk.push('\n');
            hunk.push_str(line);
        } else {
            header.push('\n');
            header.push_str(line);
        }
    }
    if let Some(done) = cur {
        out.push(done);
    }
    if patch.truncated {
        out.pop(); // the marker landed in the last fragment: never restore it
    }
    out
}

/// The batch's patch fragments with the uniform proof flag attached to each.
fn batch_hunks(
    tree: &snapshot::TreeService,
    proof_passed: bool,
) -> Result<Vec<(String, bool)>, String> {
    let diff = tree.diff().map_err(|e| format!("snapshot diff: {e}"))?;
    let patch = tree
        .patch(&diff)
        .map_err(|e| format!("snapshot patch: {e}"))?;
    Ok(split_patch(&patch)
        .into_iter()
        .map(|h| (h, proof_passed))
        .collect())
}

/// Headless multi-tick run on real siblings: [`LlmClient`] provider,
/// [`tool_core::Registry`] tools, [`BudgetGuard`] step head,
/// [`snapshot::TreeService`] baseline-per-batch with batch-scope rollback
/// (proven prefix survives: the baseline commits it first), context prompt,
/// [`BetsHook`] step-head gate (`on_step_head`) + proof-gated post-batch gate
/// (`on_post_batch`: Committed stands, Partial restores the kept hunks,
/// Aborted rolls the batch back), [`Emitter`] frames, [`LogWriter`]
/// file record. Termination order is [`LoopState::terminate`] (§10); the
/// grace step runs inside `start_provider_call`, the halt lands after.
/// Inputs seed here (Crash queues like User, recorded with Crash source);
/// followups ride `state.followups`. Parked-idle with empty queues is Done.
pub async fn run<P: LlmClient>(
    state: &mut LoopState,
    r: Run<'_, P>,
    inputs: Vec<Input>,
    cancel: &CancellationToken,
) -> Outcome {
    let Run {
        provider,
        registry,
        agent,
        workdir,
        emitter,
        bets,
        cfg,
    } = r;
    state.drain_timeout = cfg.drain_timeout;
    let root = CancellationToken::new();
    let run_id = state.next_emit_id();
    emitter.emit(AgentEvent::RunStart {
        run_id,
        goal: cfg.goal.clone(),
    });
    let mut synced = 0usize;
    let mut writer: Option<LogWriter> = None;
    if let Some(p) = cfg.log_path.clone() {
        match LogWriter::open(&p) {
            Ok(w) => writer = Some(w),
            Err(e) => {
                let o = Outcome::Failed(format!("log open: {e}"));
                emitter.emit(AgentEvent::RunEnd {
                    outcome: event_outcome(&o),
                    messages: Vec::new(),
                });
                return o;
            }
        }
    }
    if state.items.is_empty() {
        append_to(
            &mut state.items,
            ItemKind::Header {
                version: LOG_VERSION,
                session_id: format!("run-{run_id}"),
                cwd: workdir.to_string_lossy().into_owned(),
                model: cfg.model.clone(),
            },
        );
    }
    let tree = snapshot::TreeService::new(workdir);
    if let Err(e) = tree.ensure() {
        return finish_run(
            state,
            writer.as_mut(),
            emitter,
            run_id,
            &mut synced,
            Outcome::Failed(format!("snapshot ensure: {e}")),
        );
    }
    if sync_log(&state.items, writer.as_mut(), &mut synced).is_err() {
        return finish_run(
            state,
            None,
            emitter,
            run_id,
            &mut synced,
            Outcome::Failed("log append failed".into()),
        );
    }
    for input in inputs {
        let before = state.turn;
        state.apply_input(input);
        if sync_log(&state.items, writer.as_mut(), &mut synced).is_err() {
            return finish_run(
                state,
                None,
                emitter,
                run_id,
                &mut synced,
                Outcome::Failed("log append failed".into()),
            );
        }
        if state.turn != before {
            emitter.emit(AgentEvent::TurnStart { turn: state.turn });
        }
    }
    if state.turn == 0 {
        let o = Outcome::Failed("run needs at least one input".into());
        emitter.emit(AgentEvent::RunEnd {
            outcome: event_outcome(&o),
            messages: Vec::new(),
        });
        return o;
    }
    // Prefix-cache head: freeze the file map once, before the first request;
    // mid-run edits must not rotate the system message. Named-file pins freeze
    // on the same beat (dropout rule in `build_request`).
    state.file_map = Some(context::file_map(workdir, 200).join("\n"));
    state.pins = Some(pin_snapshot(&cfg.context_files, workdir));
    loop {
        if cancel.is_cancelled() {
            state.stop_hard = true;
            state.gate.begin_abort();
        }
        state.admit_steering();
        if sync_log(&state.items, writer.as_mut(), &mut synced).is_err() {
            return finish_run(
                state,
                None,
                emitter,
                run_id,
                &mut synced,
                Outcome::Failed("log append failed".into()),
            );
        }
        // Bets site 1: step head.
        if let PhaseVerdict::Return(o) = bets.on_step_head(state.step as u64) {
            return finish_run(state, writer.as_mut(), emitter, run_id, &mut synced, o);
        }
        // Grace runs here (may_step inside); terminate halts after.
        let turn_token = match state.start_provider_call(&root) {
            Some(t) => t,
            None => match state.terminate() {
                PhaseVerdict::Return(o) => {
                    return finish_run(state, writer.as_mut(), emitter, run_id, &mut synced, o);
                }
                PhaseVerdict::Break => {
                    tokio::task::yield_now().await;
                    continue;
                }
                PhaseVerdict::Continue => {
                    if state.phase == Phase::Idle && state.is_idle() {
                        return finish_run(
                            state,
                            writer.as_mut(),
                            emitter,
                            run_id,
                            &mut synced,
                            Outcome::Done,
                        );
                    }
                    continue;
                }
            },
        };
        let req = build_request(state, registry, workdir, &cfg);
        match provider.complete(&cfg.model, &req).await {
            Err(e) => {
                let cancelled = cancel.is_cancelled() || root.is_cancelled();
                // Metered failures carry what the attempts were billed; every
                // other error shape has no usage to report.
                let (msg, usage) = match e {
                    LlmError::Metered { source, usage } => (source, usage),
                    other => (format!("{other:?}"), None),
                };
                let turn = state.turn;
                state.finish_provider_msg(ProviderMsg::Failed {
                    turn,
                    err: msg.clone(),
                    cancelled,
                    usage,
                });
                if sync_log(&state.items, writer.as_mut(), &mut synced).is_err() {
                    return finish_run(
                        state,
                        None,
                        emitter,
                        run_id,
                        &mut synced,
                        Outcome::Failed("log append failed".into()),
                    );
                }
                emitter.emit(AgentEvent::Error {
                    error: AgentError {
                        code: "provider-failed".into(),
                        message: msg,
                    },
                });
                if state.call_model {
                    continue; // in-step retry reuses the same assembly
                }
                match state.terminate() {
                    PhaseVerdict::Return(o) => {
                        return finish_run(state, writer.as_mut(), emitter, run_id, &mut synced, o);
                    }
                    _ => {
                        // Never spin: parked-idle without input is Done.
                        if state.phase == Phase::Idle && state.is_idle() {
                            return finish_run(
                                state,
                                writer.as_mut(),
                                emitter,
                                run_id,
                                &mut synced,
                                Outcome::Done,
                            );
                        }
                        continue;
                    }
                }
            }
            Ok(resp) => {
                let turn = state.turn;
                state.finish_provider_msg(ProviderMsg::Settled {
                    turn,
                    message: resp.message.clone(),
                    stop: resp.stop,
                    usage: Some(resp.usage.clone()),
                });
                let outcome = state.step_claim(resp.message.clone(), resp.stop);
                if sync_log(&state.items, writer.as_mut(), &mut synced).is_err() {
                    return finish_run(
                        state,
                        None,
                        emitter,
                        run_id,
                        &mut synced,
                        Outcome::Failed("log append failed".into()),
                    );
                }
                emit_message_frames(state, &resp.message, Some(&resp.usage), emitter);
                match outcome {
                    ClaimOutcome::Done | ClaimOutcome::Truncated(_) | ClaimOutcome::Refused => {
                        let before = state.turn;
                        match state.terminate() {
                            PhaseVerdict::Return(o) => {
                                return finish_run(
                                    state,
                                    writer.as_mut(),
                                    emitter,
                                    run_id,
                                    &mut synced,
                                    o,
                                );
                            }
                            _ => {
                                if !close_hopped_turn(
                                    state,
                                    writer.as_mut(),
                                    emitter,
                                    &mut synced,
                                    before,
                                ) {
                                    return finish_run(
                                        state,
                                        None,
                                        emitter,
                                        run_id,
                                        &mut synced,
                                        Outcome::Failed("log append failed".into()),
                                    );
                                }
                                if state.phase == Phase::Idle && state.is_idle() {
                                    return finish_run(
                                        state,
                                        writer.as_mut(),
                                        emitter,
                                        run_id,
                                        &mut synced,
                                        Outcome::Done,
                                    );
                                }
                                continue;
                            }
                        }
                    }
                    ClaimOutcome::HardExit(label) => {
                        state.fatal_error = Some(label.clone());
                        state.gate.close(label);
                        match state.terminate() {
                            PhaseVerdict::Return(o) => {
                                return finish_run(
                                    state,
                                    writer.as_mut(),
                                    emitter,
                                    run_id,
                                    &mut synced,
                                    o,
                                );
                            }
                            _ => continue,
                        }
                    }
                    ClaimOutcome::Dispatch(calls) => {
                        for c in &calls {
                            emitter.emit(AgentEvent::ToolStart {
                                id: c.call_id.clone(),
                                name: c.name.clone(),
                                args: c.args.clone(),
                            });
                        }
                        if let Err(e) = tree.baseline() {
                            return finish_run(
                                state,
                                writer.as_mut(),
                                emitter,
                                run_id,
                                &mut synced,
                                Outcome::Failed(format!("snapshot baseline: {e}")),
                            );
                        }
                        let mut batch_failed = false;
                        for c in &calls {
                            if cancel.is_cancelled() {
                                let content = "aborted before dispatch".to_owned();
                                state.record_tool_result(ToolMsg {
                                    call_id: c.call_id.clone(),
                                    result: ToolResult {
                                        content: content.clone(),
                                        is_error: true,
                                    },
                                });
                                batch_failed = true;
                            } else {
                                let inv = match registry.prepare(agent, c.clone()) {
                                    tool_core::CallStatus::Dispatch(inv) => Some(inv),
                                    tool_core::CallStatus::Result(res) => {
                                        batch_failed |= res.is_error;
                                        state.record_tool_result(ToolMsg {
                                            call_id: c.call_id.clone(),
                                            result: res,
                                        });
                                        None
                                    }
                                };
                                if let Some(inv) = inv {
                                    let name = inv.name.clone();
                                    let args = inv.args.to_string();
                                    let res = match registry.resolve(&inv.name) {
                                        Some(tool) => {
                                            match tool.execute(inv, turn_token.child_token()).await
                                            {
                                                Ok(o) => ToolResult {
                                                    content: if o.truncated {
                                                        format!("{}\n[truncated]", o.content)
                                                    } else {
                                                        o.content
                                                    },
                                                    is_error: false,
                                                },
                                                Err(e) => ToolResult::from(e),
                                            }
                                        }
                                        None => ToolResult {
                                            content: format!("unknown tool: {name}"),
                                            is_error: true,
                                        },
                                    };
                                    batch_failed |= res.is_error;
                                    state.observe_action(&format!("{name}:{args}"), &res.content);
                                    if !res.is_error && matches!(name.as_str(), "edit" | "write") {
                                        state.edits += 1;
                                    }
                                    state.record_tool_result(ToolMsg {
                                        call_id: c.call_id.clone(),
                                        result: res.clone(),
                                    });
                                }
                            }
                            // Directives and the nudge land on the fresh tail
                            // BEFORE the sync, so the file never holds a stale
                            // tail and the row keeps the exact model text.
                            state.queue_directives();
                            state.apply_budget_nudge();
                            state.deliver_directives();
                            if sync_log(&state.items, writer.as_mut(), &mut synced).is_err() {
                                return finish_run(
                                    state,
                                    None,
                                    emitter,
                                    run_id,
                                    &mut synced,
                                    Outcome::Failed("log append failed".into()),
                                );
                            }
                            if let Some(done) = state
                                .tool_calls
                                .get(&c.call_id)
                                .and_then(|s| s.result.clone())
                            {
                                emitter.emit(AgentEvent::ToolEnd {
                                    id: c.call_id.clone(),
                                    result: Value::String(done.content),
                                    is_error: done.is_error,
                                });
                            }
                        }
                        // Bets site 2: post-batch. The claim is built before
                        // the hunks are read (prediction precedes observation);
                        // no step-stored verdict exists yet, so the claim states
                        // the verifier verdict the gate checks. The batch's
                        // hunks are extracted BEFORE the tool-error rollback
                        // below — a failed batch would destroy its own evidence.
                        // Uniform proof flag: the batch's tool results all
                        // passed (test/check rides as any other result);
                        // per-hunk incremental proof is the later ablation.
                        let claim = bets::Claim {
                            predicted_verdict: "tool batch results all passed".to_owned(),
                            on_mismatch: "roll back the batch; keep only proven hunks".to_owned(),
                        };
                        let hunks = match batch_hunks(&tree, !batch_failed) {
                            Ok(h) => h,
                            Err(e) => {
                                return finish_run(
                                    state,
                                    writer.as_mut(),
                                    emitter,
                                    run_id,
                                    &mut synced,
                                    Outcome::Failed(e),
                                )
                            }
                        };
                        // Batch scope: only the failed batch rolls back; the
                        // baseline already committed the proven prefix.
                        if batch_failed {
                            // Pre-effect record: the rollback destroys the
                            // batch's successful edits, so the notice is
                            // durable before the tree moves. `Attempt` is the
                            // existing log-only vocabulary (no model row, no
                            // new event); the model-visible copy rides the
                            // directive channel on the next tool tail.
                            append_to(
                                &mut state.items,
                                ItemKind::Attempt {
                                    error: ROLLBACK_NOTICE.into(),
                                    will_retry: true,
                                },
                            );
                            state.push_directive(ROLLBACK_NOTICE.into());
                            if sync_log(&state.items, writer.as_mut(), &mut synced).is_err() {
                                return finish_run(
                                    state,
                                    None,
                                    emitter,
                                    run_id,
                                    &mut synced,
                                    Outcome::Failed("log append failed".into()),
                                );
                            }
                            if let Err(e) = tree.rollback() {
                                return finish_run(
                                    state,
                                    writer.as_mut(),
                                    emitter,
                                    run_id,
                                    &mut synced,
                                    Outcome::Failed(format!("snapshot rollback: {e}")),
                                );
                            }
                        }
                        // Verdict→tree mapping: Committed stands (the failed-
                        // batch rollback above is the existing path either way),
                        // Partial restores the kept prefix from baseline, Aborted
                        // runs the existing rollback path again (idempotent).
                        // A restore error fails closed: restore_hunks leaves the
                        // tree at baseline when any fragment does not apply.
                        match bets.on_post_batch(&claim, &hunks) {
                            bets::CommitVerdict::Committed => {}
                            bets::CommitVerdict::Partial { savepoint } => {
                                if let Err(e) = tree.restore_hunks(&savepoint.kept_hunks) {
                                    return finish_run(
                                        state,
                                        writer.as_mut(),
                                        emitter,
                                        run_id,
                                        &mut synced,
                                        Outcome::Failed(format!("snapshot restore: {e}")),
                                    );
                                }
                            }
                            bets::CommitVerdict::Aborted { .. } => {
                                if let Err(e) = tree.rollback() {
                                    return finish_run(
                                        state,
                                        writer.as_mut(),
                                        emitter,
                                        run_id,
                                        &mut synced,
                                        Outcome::Failed(format!("snapshot rollback: {e}")),
                                    );
                                }
                            }
                        }
                        // Bets site 2 step hook (unchanged): Return ends the run.
                        if let PhaseVerdict::Return(o) = bets.on_step() {
                            return finish_run(
                                state,
                                writer.as_mut(),
                                emitter,
                                run_id,
                                &mut synced,
                                o,
                            );
                        }
                        let before = state.turn;
                        match state.terminate() {
                            PhaseVerdict::Return(o) => {
                                return finish_run(
                                    state,
                                    writer.as_mut(),
                                    emitter,
                                    run_id,
                                    &mut synced,
                                    o,
                                );
                            }
                            _ => {
                                if !close_hopped_turn(
                                    state,
                                    writer.as_mut(),
                                    emitter,
                                    &mut synced,
                                    before,
                                ) {
                                    return finish_run(
                                        state,
                                        None,
                                        emitter,
                                        run_id,
                                        &mut synced,
                                        Outcome::Failed("log append failed".into()),
                                    );
                                }
                                continue;
                            }
                        }
                    }
                }
            }
        }
    }
}

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

    fn result() -> ToolResult {
        ToolResult {
            content: "ok".into(),
            is_error: false,
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
            usage: None,
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
            usage: None,
        }));
        assert_eq!(s.fatal_error.as_deref(), Some("dead"));
        assert!(matches!(
            s.terminate(),
            PhaseVerdict::Return(Outcome::Failed(_))
        ));
        assert!(!s.should_call_model()); // closed gate says nay
    }

    #[test]
    fn failed_attempt_usage_is_metered_and_still_retries() {
        let mut s = LoopState::new();
        s.turn = 1;
        let root = CancellationToken::new();
        assert!(s.start_provider_call(&root).is_some());
        let u = Usage {
            input: 100,
            output: 20,
            cache_read: 0,
            cache_write: 0,
            reasoning: Some(5),
            cost_usd: Some(0.02),
        };
        assert!(s.finish_provider_msg(ProviderMsg::Failed {
            turn: 1,
            err: "output truncated at 64 tokens".into(),
            cancelled: false,
            usage: Some(u),
        }));
        // Budget and run totals see the failed attempt's spend.
        assert_eq!(s.budget.counters().tokens, 120);
        assert_eq!(s.budget.counters().spent_cents, 2);
        assert_eq!(s.usage_totals.input_tokens, 100);
        assert_eq!(s.usage_totals.output_tokens, 20);
        assert_eq!(s.usage_totals.reasoning_tokens, Some(5));
        assert_eq!(s.usage_totals.cost_usd, Some(0.02));
        // The Attempt row still drives the in-step retry.
        assert!(s.call_model);
        assert!(matches!(
            s.items.last().map(|i| &i.kind),
            Some(ItemKind::Attempt {
                will_retry: true,
                ..
            })
        ));
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
    fn derived_messages_fold_history() {
        let mut s = LoopState::new();
        s.phase = Phase::Running;
        s.apply_input(Input::User("build it".into()));
        s.admit_steering();
        let msgs = s.derived_messages();
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            (msgs[0].role.as_str(), msgs[0].content.as_str()),
            ("user", "build it")
        );
    }

    #[test]
    fn derived_messages_collapse_5_and_ids() {
        let mut s = LoopState::new();
        for i in 0..7 {
            s.items.push(Item {
                seq: s.items.len() as u64,
                id: format!("t{i}"),
                parent_id: None,
                recorded_at: SystemTime::now(),
                kind: ItemKind::ToolResult {
                    call_id: format!("c{i}"),
                    content: format!("line{i}-head\nline{i}-tail"),
                    is_error: false,
                    recovery: None,
                },
            });
        }
        let msgs = s.derived_messages();
        assert_eq!(msgs.len(), 7);
        assert_eq!(
            msgs[0].content,
            "[collapsed: 21b — re-open to edit] line0-head\nline0-tail"
        );
        assert_eq!(
            msgs[1].content,
            "[collapsed: 21b — re-open to edit] line1-head\nline1-tail"
        );
        assert_eq!(msgs[2].content, "line2-head\nline2-tail");
        assert_eq!(msgs[2].tool_call_id.as_deref(), Some("c2"));
    }

    #[test]
    fn derived_messages_keeps_thinking_on_last_two_assistants() {
        let mut s = LoopState::new();
        for i in 0..4 {
            s.push_assistant(
                &AssistantMessage {
                    content: format!("step-{i}"),
                    tool_calls: Vec::new(),
                    thinking: Some(format!("reason-{i}")),
                },
                "Stop",
            );
        }
        let msgs = s.derived_messages();
        let thinking: Vec<Option<&str>> = msgs
            .iter()
            .filter(|m| m.role == "assistant")
            .map(|m| m.thinking.as_deref())
            .collect();
        assert_eq!(
            thinking,
            vec![None, None, Some("reason-2"), Some("reason-3")],
            "thinking survives on exactly the last 2 assistant rows"
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
            result: result(),
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
    fn no_tail_keeps_directive_queued_and_nudge_latch_unburned() {
        let mut s = LoopState::new();
        // Nudge due at warn steps, but tailless: the latch is not consumed.
        s.budget.counters_mut().steps = s.budget.config().warn_steps.get();
        assert!(s.apply_budget_nudge().is_none());
        // A queued directive with no tail stays queued (retry, not drop).
        s.budget.counters_mut().actions_this_trial = s.budget.config().actions_per_trial / 2;
        s.queue_directives();
        assert_eq!(s.pending_directives.len(), 1);
        assert_eq!(s.deliver_directives(), 0);
        assert_eq!(s.pending_directives.len(), 1);
        // First recorded result creates the tail: both land, once each.
        s.turn = 1;
        assert!(matches!(
            s.step_claim(assistant(vec![call("a")]), StopReason::ToolUse),
            ClaimOutcome::Dispatch(_)
        ));
        assert!(s.record_tool_result(ToolMsg {
            call_id: "a".into(),
            result: result(),
        }));
        assert_eq!(s.deliver_directives(), 1);
        let nudge = s
            .apply_budget_nudge()
            .expect("latch survived the tailless attempt");
        assert!(nudge.contains("wrap up"));
        match s.items.last().map(|i| &i.kind) {
            Some(ItemKind::ToolResult { content, .. }) => {
                assert!(content.contains("0 edits so far after 15 actions"));
                assert!(content.contains(&nudge));
            }
            other => panic!("expected ToolResult tail, got {other:?}"),
        }
        assert!(s.pending_directives.is_empty());
        assert_eq!(s.deliver_directives(), 0); // delivered once, never twice
    }

    #[test]
    fn directive_triggers_fire_once_at_half_and_late_cap() {
        let mut s = LoopState::new();
        let cap = s.budget.config().actions_per_trial;
        s.budget.counters_mut().actions_this_trial = cap / 2;
        s.queue_directives();
        assert_eq!(
            s.pending_directives.front().unwrap(),
            "0 edits so far after 15 actions. Stop reading. Apply your first edit with the edit tool NOW."
        );
        s.queue_directives();
        assert_eq!(s.pending_directives.len(), 1); // half-cap latch holds
        s.edits = 1; // an edit silences only the zero-edit rule
        s.budget.counters_mut().actions_this_trial = cap * 4 / 5;
        s.queue_directives();
        assert_eq!(
            s.pending_directives.back().unwrap(),
            "only 6 actions remain before the run is stopped. Finish and submit your patch now."
        );
        s.queue_directives();
        assert_eq!(s.pending_directives.len(), 2); // both one-shot
    }

    #[test]
    fn lessons_ride_the_tail_once_each() {
        let mut s = LoopState::new();
        s.push_lesson("vary the approach".into());
        s.turn = 1;
        assert!(matches!(
            s.step_claim(assistant(vec![call("a")]), StopReason::ToolUse),
            ClaimOutcome::Dispatch(_)
        ));
        assert!(s.record_tool_result(ToolMsg {
            call_id: "a".into(),
            result: result(),
        }));
        assert_eq!(s.deliver_directives(), 1);
        match s.items.last().map(|i| &i.kind) {
            Some(ItemKind::ToolResult { content, .. }) => {
                assert!(content.contains("vary the approach"))
            }
            other => panic!("expected ToolResult tail, got {other:?}"),
        }
        assert!(s.lessons.is_empty());
        assert_eq!(s.deliver_directives(), 0); // each lesson delivered once
    }

    #[test]
    fn collapse_stub_preview_caps_at_120_chars() {
        let mut s = LoopState::new();
        for i in 0..6 {
            s.items.push(Item {
                seq: s.items.len() as u64,
                id: format!("t{i}"),
                parent_id: None,
                recorded_at: SystemTime::now(),
                kind: ItemKind::ToolResult {
                    call_id: format!("c{i}"),
                    content: format!("{}-{i}", "x".repeat(200)),
                    is_error: false,
                    recovery: None,
                },
            });
        }
        let msgs = s.derived_messages();
        let head = &msgs[0].content;
        let prefix = "[collapsed: 202b — re-open to edit] ";
        assert!(head.starts_with(prefix), "{head}");
        assert_eq!(head.chars().count(), prefix.chars().count() + 120);
    }

    #[test]
    fn build_request_system_is_static_and_budgets_ride_the_tail() {
        let root = run_tmp("contract");
        std::fs::write(root.join("a.rs"), "v1\n").unwrap();
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        state.phase = Phase::Running;
        state.apply_input(Input::User("build it".into()));
        state.admit_steering();
        state.budget.counters_mut().steps = 3;
        state.budget.counters_mut().actions_this_trial = 5;
        state.budget.counters_mut().tokens = 100;
        let r1 = build_request(&mut state, &registry, &root, &RunConfig::default());
        // Mid-run edit + moved counters: neither may rotate the cached head.
        std::fs::write(root.join("b.rs"), "new\n").unwrap();
        state.budget.counters_mut().steps = 4;
        state.budget.counters_mut().actions_this_trial = 6;
        state.budget.counters_mut().tokens = 300;
        let r2 = build_request(&mut state, &registry, &root, &RunConfig::default());

        assert_eq!(r1.messages[0].role, "system");
        let sys = &r1.messages[0].content;
        assert!(sys.starts_with("--- workflow-contract"), "{sys}");
        for needle in [
            "budgets remaining are printed on the last line of the newest message; read them there.",
            "tool results older than the last 5 are collapsed to one line; re-open a file immediately before editing it.",
            "work file-by-file: view -> edit immediately -> next file.",
            "`edit` and `write` are the ONLY patch mechanisms — never write files via exec; exec/test are for checks only.",
            "when your patch is complete, reply with a text message and NO tool calls — that finishes the run.",
        ] {
            assert!(sys.contains(needle), "missing {needle:?} in {sys}");
        }
        assert!(
            !sys.contains("budgets remaining:"),
            "live digits in system: {sys}"
        );
        assert!(
            sys.contains("--- file-map") && sys.contains("a.rs"),
            "{sys}"
        );
        assert!(!sys.contains("b.rs"), "map froze at run start: {sys}");
        assert_eq!(sys, &r2.messages[0].content);
        assert_eq!(r1.messages.len(), 2); // system + the one input

        // Identical prefix up to the final (budget-carrying) message.
        for (m1, m2) in r1.messages[..r1.messages.len() - 1]
            .iter()
            .zip(&r2.messages[..r2.messages.len() - 1])
        {
            assert_eq!(format!("{m1:?}"), format!("{m2:?}"));
        }
        assert_eq!(
            r1.messages.last().unwrap().content,
            "build it\nbudgets remaining: steps 17/20; actions 25/30; tokens 49900/50000"
        );
        assert_eq!(
            r2.messages.last().unwrap().content,
            "build it\nbudgets remaining: steps 16/20; actions 24/30; tokens 49700/50000"
        );
        // Request-scoped only: the durable transcript never carries counters.
        assert!(!state
            .items
            .iter()
            .any(|i| format!("{:?}", i.kind).contains("budgets remaining")));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Pins follow the file-map rule: frozen at run start (byte-stable head)
    /// and dropped once their file changes, so the model never sees pre-edit
    /// bytes the `edit` result already superseded.
    #[test]
    fn pins_freeze_at_run_start_and_drop_when_edited() {
        let root = run_tmp("pins");
        std::fs::write(root.join("goal.md"), "keep me\n").unwrap();
        std::fs::write(root.join("note.md"), "v1\n").unwrap();
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        state.phase = Phase::Running;
        state.apply_input(Input::User("go".into()));
        state.admit_steering();
        let cfg = RunConfig {
            context_files: vec!["goal.md".into(), "note.md".into()],
            ..RunConfig::default()
        };
        // First request freezes the pins, as `run` does before its first call.
        let r1 = build_request(&mut state, &registry, &root, &cfg);
        assert!(r1.messages[0].content.contains("--- goal.md"));
        assert!(r1.messages[0].content.contains("v1"));

        // Unrelated write: pin bytes stay identical across requests.
        std::fs::write(root.join("other.txt"), "x\n").unwrap();
        let r2 = build_request(&mut state, &registry, &root, &cfg);
        assert_eq!(r2.messages[0].content, r1.messages[0].content);

        // Mid-run edit to one pinned file: that pin disappears, the other stays.
        std::fs::write(root.join("note.md"), "v2\n").unwrap();
        let r3 = build_request(&mut state, &registry, &root, &cfg);
        let sys = &r3.messages[0].content;
        assert!(
            sys.contains("--- goal.md") && sys.contains("keep me"),
            "{sys}"
        );
        assert!(!sys.contains("--- note.md"), "edited pin must drop: {sys}");
        assert!(
            !sys.contains("v1") && !sys.contains("v2"),
            "stale or fresh pin bytes leaked: {sys}"
        );
        let _ = std::fs::remove_dir_all(&root);
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
            usage: None,
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
            },
        })
        .await
        .unwrap();
        let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        // Batch done, results owed back to the model: the tick parks on.
        assert!(matches!(v, PhaseVerdict::Continue));
        // A hard stop drains the parked idle and closes the turn.
        itx.send(Input::StopHard).await.unwrap();
        let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(v, PhaseVerdict::Return(Outcome::Cancelled)));

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
        // F1a: the settle's usage rides MessageEnd; TurnEnd carries the totals.
        assert!(emitter.history().iter().any(|e| matches!(
            e,
            AgentEvent::MessageEnd { usage: Some(u), .. }
                if u.input_tokens == 100 && u.output_tokens == 50 && u.cost_usd == Some(0.02)
        )));
        assert!(emitter.history().iter().any(|e| matches!(
            e,
            AgentEvent::TurnEnd { usage_totals, .. }
                if usage_totals.input_tokens == 100 && usage_totals.cost_usd == Some(0.02)
        )));
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
        // F1a: no provider usage = None on MessageEnd, zero totals on TurnEnd.
        assert!(emitter
            .history()
            .iter()
            .any(|e| matches!(e, AgentEvent::MessageEnd { usage: None, .. })));
        assert!(emitter.history().iter().any(|e| matches!(
            e,
            AgentEvent::TurnEnd { usage_totals, .. }
                if *usage_totals == UsageReport::default()
        )));
    }

    // --- multi-tick run() assembly: fakes + tempdir git repo ---

    use provider_core::{Capabilities, Credentials, LlmClient, LlmError, Request, Response};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use tool_core::{
        CallStatus as CoreCallStatus, GrantGate, Invocation as CoreInvocation,
        Registry as CoreRegistry, Tool as CoreTool, ToolCall as CoreToolCall,
        ToolDefinition as CoreToolDef, ToolError as CoreToolError, ToolOutcome as CoreToolOutcome,
    };

    static RUN_N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn run_tmp(name: &str) -> std::path::PathBuf {
        let n = RUN_N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("rof-run-{name}-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct ScriptClient {
        order: Arc<Mutex<Vec<String>>>,
        queue: Mutex<VecDeque<Response>>,
        /// Every outgoing request, for shape assertions.
        requests: Arc<Mutex<Vec<Request>>>,
    }

    #[async_trait::async_trait]
    impl LlmClient for ScriptClient {
        async fn complete(&self, _model: &str, req: &Request) -> Result<Response, LlmError> {
            self.order.lock().unwrap().push("model".into());
            self.requests.lock().unwrap().push(req.clone());
            self.queue
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(LlmError::Transport("script empty".into()))
        }
        fn capabilities(&self, _model: &str) -> Capabilities {
            Capabilities {}
        }
        async fn resolve_key(&self, _provider: &str) -> Result<Credentials, LlmError> {
            Ok(Credentials {
                api_key: String::new(),
            })
        }
    }

    fn script_resp(calls: Vec<(&str, &str, serde_json::Value)>, stop: StopReason) -> Response {
        Response {
            message: AssistantMessage {
                content: "step".into(),
                tool_calls: calls
                    .into_iter()
                    .map(|(id, name, args)| provider_core::ToolCallRef {
                        id: id.into(),
                        name: name.into(),
                        args,
                    })
                    .collect(),
                thinking: None,
            },
            stop,
            usage: Usage {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: None,
                cost_usd: Some(0.01),
            },
            latency_ms: 0,
            attempts: 1,
            raw_stop_reason: None,
        }
    }

    fn text_resp(text: &str) -> Response {
        Response {
            message: AssistantMessage {
                content: text.into(),
                tool_calls: vec![],
                thinking: None,
            },
            stop: StopReason::Stop,
            usage: Usage {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: None,
                cost_usd: Some(0.01),
            },
            latency_ms: 0,
            attempts: 1,
            raw_stop_reason: None,
        }
    }

    struct WriteFile {
        root: std::path::PathBuf,
    }

    #[async_trait::async_trait]
    impl CoreTool for WriteFile {
        fn definition(&self) -> CoreToolDef {
            CoreToolDef {
                name: "write".into(),
                description: "write a file under the temp root".into(),
                schema: serde_json::json!({
                    "type": "object",
                    "required": ["path", "content"],
                    "additionalProperties": false,
                    "properties": {
                        "path": {"type": "string"},
                        "content": {"type": "string"}
                    }
                }),
            }
        }
        fn prepare(&self, call: &CoreToolCall) -> CoreCallStatus {
            CoreCallStatus::Dispatch(CoreInvocation {
                call_id: call.call_id.clone(),
                name: call.name.clone(),
                args: call.args.clone(),
            })
        }
        async fn execute(
            &self,
            inv: CoreInvocation,
            _cancel: CancellationToken,
        ) -> Result<CoreToolOutcome, CoreToolError> {
            let path = inv.args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let content = inv
                .args
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if path.is_empty() {
                return Err(CoreToolError::Failed("bad args".into()));
            }
            std::fs::write(self.root.join(path), content)
                .map_err(|e| CoreToolError::Failed(e.to_string()))?;
            Ok(CoreToolOutcome {
                content: format!("wrote {path}"),
                truncated: false,
            })
        }
    }

    struct Boom;

    #[async_trait::async_trait]
    impl CoreTool for Boom {
        fn definition(&self) -> CoreToolDef {
            CoreToolDef {
                name: "boom".into(),
                description: "always fails".into(),
                schema: serde_json::json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {}
                }),
            }
        }
        fn prepare(&self, call: &CoreToolCall) -> CoreCallStatus {
            CoreCallStatus::Dispatch(CoreInvocation {
                call_id: call.call_id.clone(),
                name: call.name.clone(),
                args: call.args.clone(),
            })
        }
        async fn execute(
            &self,
            _inv: CoreInvocation,
            _cancel: CancellationToken,
        ) -> Result<CoreToolOutcome, CoreToolError> {
            Err(CoreToolError::Failed("boom went off".into()))
        }
    }

    struct FakeRead;

    #[async_trait::async_trait]
    impl CoreTool for FakeRead {
        fn definition(&self) -> CoreToolDef {
            CoreToolDef {
                name: "read".into(),
                description: "read-only probe with a fresh observation each call".into(),
                schema: serde_json::json!({
                    "type": "object",
                    "required": ["n"],
                    "additionalProperties": false,
                    "properties": {"n": {"type": "integer"}}
                }),
            }
        }
        fn prepare(&self, call: &CoreToolCall) -> CoreCallStatus {
            CoreCallStatus::Dispatch(CoreInvocation {
                call_id: call.call_id.clone(),
                name: call.name.clone(),
                args: call.args.clone(),
            })
        }
        async fn execute(
            &self,
            inv: CoreInvocation,
            _cancel: CancellationToken,
        ) -> Result<CoreToolOutcome, CoreToolError> {
            let n = inv.args.get("n").and_then(|v| v.as_i64()).unwrap_or(-1);
            Ok(CoreToolOutcome {
                content: format!("read {n}"),
                truncated: false,
            })
        }
    }

    fn run_registry(root: &std::path::Path) -> CoreRegistry {
        let mut r = CoreRegistry::new(Arc::new(GrantGate::new(
            [(
                "agent".to_string(),
                vec!["write".to_string(), "boom".to_string(), "read".to_string()],
            )]
            .into(),
        )));
        r.register(Arc::new(WriteFile {
            root: root.to_path_buf(),
        }));
        r.register(Arc::new(Boom));
        r.register(Arc::new(FakeRead));
        r
    }

    fn run_kinds(items: &[Item]) -> Vec<&'static str> {
        items
            .iter()
            .map(|i| match &i.kind {
                ItemKind::Header { .. } => "Header",
                ItemKind::TurnStart { .. } => "TurnStart",
                ItemKind::Input { .. } => "Input",
                ItemKind::Assistant { .. } => "Assistant",
                ItemKind::Attempt { .. } => "Attempt",
                ItemKind::ToolCall { .. } => "ToolCall",
                ItemKind::ToolResult { .. } => "ToolResult",
                ItemKind::TurnEnd { .. } => "TurnEnd",
            })
            .collect()
    }

    struct RecBets {
        order: Arc<Mutex<Vec<String>>>,
    }

    impl BetsHook for RecBets {
        fn on_step(&self) -> PhaseVerdict {
            self.order.lock().unwrap().push("bets".into());
            PhaseVerdict::Continue
        }
    }

    #[tokio::test]
    async fn run_e2e_multi_turn_proven_prefix_kept_failed_batch_rolled_back() {
        use agent_event::check_pairing;
        use agent_log::read_log;
        let root = run_tmp("e2e");
        std::fs::write(root.join("a.rs"), "v1\n").unwrap();
        std::fs::write(root.join("goal.md"), "ship it\n").unwrap();
        let log_path = std::env::temp_dir().join(format!(
            "rof-run-e2e-log-{}-{}.jsonl",
            std::process::id(),
            RUN_N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let order = Arc::new(Mutex::new(Vec::new()));
        let client = ScriptClient {
            order: order.clone(),
            requests: Default::default(),
            queue: Mutex::new(VecDeque::from([
                script_resp(
                    vec![(
                        "c1",
                        "write",
                        serde_json::json!({"path": "a.rs", "content": "v2\n"}),
                    )],
                    StopReason::ToolUse,
                ),
                script_resp(
                    vec![
                        (
                            "c2",
                            "write",
                            serde_json::json!({"path": "b.rs", "content": "new\n"}),
                        ),
                        ("c3", "boom", serde_json::json!({})),
                    ],
                    StopReason::ToolUse,
                ),
                text_resp("batch rounds done"),
                text_resp("second done"),
            ])),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        state.followups.push_back("second task".into());
        let mut emitter = Emitter::new();
        let cancel = CancellationToken::new();
        let cfg = RunConfig {
            log_path: Some(log_path.clone()),
            context_files: vec!["goal.md".into()],
            ..RunConfig::default()
        };
        let outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &NoBets,
                cfg,
            },
            vec![Input::User("build it".into())],
            &cancel,
        )
        .await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        // Proven prefix kept on disk, failed batch rolled back.
        assert_eq!(std::fs::read_to_string(root.join("a.rs")).unwrap(), "v2\n");
        assert!(!root.join("b.rs").exists());
        // Budget consumed: one step per model call, tokens + spend metered.
        assert_eq!(state.budget.counters().steps, 4);
        assert_eq!(state.budget.counters().tokens, 60);
        assert_eq!(state.budget.counters().spent_cents, 4);
        // No cap tripped: the one grace step stayed untouched.
        assert!(state.budget.exceeded().is_none());
        // Events ordered and paired across two turns plus the run pair.
        let history = emitter.history();
        assert!(check_pairing(history), "unpaired: {history:?}");
        let kinds = event_order(history);
        assert!(matches!(
            history.first().unwrap(),
            AgentEvent::RunStart { .. }
        ));
        assert!(matches!(history.last().unwrap(), AgentEvent::RunEnd { .. }));
        assert_eq!(kinds.iter().filter(|k| **k == "TurnStart").count(), 2);
        assert_eq!(kinds.iter().filter(|k| **k == "TurnEnd").count(), 2);
        assert_eq!(kinds.iter().filter(|k| **k == "MessageStart").count(), 4);
        assert_eq!(kinds.iter().filter(|k| **k == "MessageEnd").count(), 4);
        assert_eq!(kinds.iter().filter(|k| **k == "ToolStart").count(), 3);
        assert_eq!(kinds.iter().filter(|k| **k == "ToolEnd").count(), 3);
        // File log validates and is balanced: header first.
        let file_items = read_log(&log_path).unwrap();
        assert!(matches!(file_items[0].kind, ItemKind::Header { .. }));
        assert_eq!(file_items.len(), state.items.len());
        let log_kinds = run_kinds(&file_items);
        assert_eq!(
            log_kinds,
            vec![
                "Header",
                "TurnStart",
                "Input",
                "Assistant",
                "ToolCall",
                "ToolResult",
                "Assistant",
                "ToolCall",
                "ToolCall",
                "ToolResult",
                "ToolResult",
                "Attempt",
                "Assistant",
                "TurnEnd",
                "TurnStart",
                "Input",
                "Assistant",
                "TurnEnd",
            ]
        );
        // The failed call is a durable error result, not a throw.
        let boom = file_items
            .iter()
            .find(|i| matches!(&i.kind, ItemKind::ToolResult { call_id, .. } if call_id == "c3"))
            .unwrap();
        assert!(matches!(
            &boom.kind,
            ItemKind::ToolResult { is_error: true, .. }
        ));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&log_path);
    }

    #[tokio::test]
    async fn settle_usage_rides_message_end_and_cumulates_on_turn_end() {
        let root = run_tmp("usage");
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        let mut settled = text_resp("done");
        settled.usage.reasoning = Some(3);
        let client = ScriptClient {
            order: Default::default(),
            requests: Default::default(),
            queue: Mutex::new(VecDeque::from([
                script_resp(
                    vec![(
                        "c1",
                        "write",
                        serde_json::json!({"path": "a.txt", "content": "bye\n"}),
                    )],
                    StopReason::ToolUse,
                ),
                settled,
            ])),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let mut emitter = Emitter::new();
        let outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &NoBets,
                cfg: RunConfig::default(),
            },
            vec![Input::User("edit the note".into())],
            &CancellationToken::new(),
        )
        .await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        let history = emitter.history();
        // Every settle reports its own usage on MessageEnd.
        let ends: Vec<&UsageReport> = history
            .iter()
            .filter_map(|e| match e {
                AgentEvent::MessageEnd { usage, .. } => {
                    Some(usage.as_ref().expect("settle must carry usage"))
                }
                _ => None,
            })
            .collect();
        assert_eq!(ends.len(), 2, "one MessageEnd per settle");
        assert_eq!(
            (
                ends[0].input_tokens,
                ends[0].output_tokens,
                ends[0].cost_usd
            ),
            (10, 5, Some(0.01))
        );
        assert_eq!(ends[0].reasoning_tokens, None); // provider reported none
        assert_eq!(ends[1].reasoning_tokens, Some(3));
        // TurnEnd totals sum both settles (10+10 in, 5+5 out, 0.01+0.01).
        let totals = history
            .iter()
            .find_map(|e| match e {
                AgentEvent::TurnEnd { usage_totals, .. } => Some(usage_totals),
                _ => None,
            })
            .expect("turn closes with totals");
        assert_eq!((totals.input_tokens, totals.output_tokens), (20, 10));
        assert_eq!(totals.reasoning_tokens, Some(3)); // reported-only sum, not erased by None
        assert_eq!(totals.cost_usd, Some(0.02));
        assert_eq!(state.usage_totals.input_tokens, 20);
        assert_eq!(state.budget.counters().tokens, 30);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn run_half_cap_zero_edits_directive_lands_on_a_tool_tail() {
        use agent_log::read_log;
        let root = run_tmp("directive");
        let log_path = std::env::temp_dir().join(format!(
            "rof-directive-log-{}-{}.jsonl",
            std::process::id(),
            RUN_N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let requests = Arc::new(Mutex::new(Vec::new()));
        // 15 read-only actions (= cap/2) with zero edits: the directive fires.
        let mut queue: VecDeque<Response> = VecDeque::new();
        for i in 1..=15 {
            let id = format!("r{i}");
            queue.push_back(script_resp(
                vec![(id.as_str(), "read", serde_json::json!({"n": i}))],
                StopReason::ToolUse,
            ));
        }
        queue.push_back(text_resp("done"));
        let client = ScriptClient {
            order: Arc::new(Mutex::new(Vec::new())),
            requests: requests.clone(),
            queue: Mutex::new(queue),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let mut emitter = Emitter::new();
        let cancel = CancellationToken::new();
        let outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &NoBets,
                cfg: RunConfig {
                    log_path: Some(log_path.clone()),
                    ..RunConfig::default()
                },
            },
            vec![Input::User("probe it".into())],
            &cancel,
        )
        .await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        assert_eq!(state.edits, 0);
        assert_eq!(state.budget.counters().actions_this_trial, 15);
        let hits: Vec<&str> = state
            .items
            .iter()
            .filter_map(|i| match &i.kind {
                ItemKind::ToolResult { content, .. }
                    if content.contains("0 edits so far after 15 actions") =>
                {
                    Some(content.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(hits[0].contains("Stop reading. Apply your first edit with the edit tool NOW."));
        assert!(state.pending_directives.is_empty());
        // The next model call sees it, on the 15th action's own tail.
        let reqs = requests.lock().unwrap();
        assert_eq!(reqs.len(), 16);
        assert!(
            reqs[15]
                .messages
                .iter()
                .any(|m| m.content.contains("0 edits so far after 15 actions")),
            "{:?}",
            reqs[15].messages
        );
        // Durability: the file row is exactly the durable row the model saw
        // (the request-scoped budget line rides the wire copy only).
        let file_items = read_log(&log_path).unwrap();
        assert!(file_items.iter().any(|i| matches!(
            &i.kind,
            ItemKind::ToolResult { content, .. }
                if content.contains("0 edits so far after 15 actions")
        )));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&log_path);
    }

    #[tokio::test]
    async fn run_failed_batch_records_rollback_notice_and_queues_it_for_the_model() {
        use agent_log::read_log;
        let root = run_tmp("rollback-notice");
        let log_path = std::env::temp_dir().join(format!(
            "rof-rollback-log-{}-{}.jsonl",
            std::process::id(),
            RUN_N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::write(root.join("a.rs"), "v1\n").unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let client = ScriptClient {
            order: Arc::new(Mutex::new(Vec::new())),
            requests: requests.clone(),
            queue: Mutex::new(VecDeque::from([
                script_resp(
                    vec![
                        (
                            "c1",
                            "write",
                            serde_json::json!({"path": "a.rs", "content": "v2\n"}),
                        ),
                        ("c2", "boom", serde_json::json!({})),
                    ],
                    StopReason::ToolUse,
                ),
                script_resp(
                    vec![(
                        "c3",
                        "write",
                        serde_json::json!({"path": "a.rs", "content": "v3\n"}),
                    )],
                    StopReason::ToolUse,
                ),
                text_resp("done"),
            ])),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let mut emitter = Emitter::new();
        let cancel = CancellationToken::new();
        let outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &NoBets,
                cfg: RunConfig {
                    log_path: Some(log_path.clone()),
                    ..RunConfig::default()
                },
            },
            vec![Input::User("go".into())],
            &cancel,
        )
        .await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        // The successful edit counted, was rolled back, then re-applied.
        assert_eq!(state.edits, 2);
        assert_eq!(std::fs::read_to_string(root.join("a.rs")).unwrap(), "v3\n");
        // Durable log-only row, never a model-message row.
        let notices: Vec<&str> = state
            .items
            .iter()
            .filter_map(|i| match &i.kind {
                ItemKind::Attempt {
                    error,
                    will_retry: true,
                } if error.contains("whole batch was reverted") => Some(error.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(notices.len(), 1, "{notices:?}");
        // Durable: the rollback record is in the log file before the model's
        // next request, and only once.
        let file_items = read_log(&log_path).unwrap();
        let durable = file_items
            .iter()
            .filter(|i| {
                matches!(&i.kind, ItemKind::Attempt { error, .. } if error.contains("whole batch was reverted"))
            })
            .count();
        assert_eq!(durable, 1, "{file_items:?}");
        // The model sees it on the next tool tail, one batch later.
        let reqs = requests.lock().unwrap();
        let seen = |i: usize| {
            reqs[i]
                .messages
                .iter()
                .any(|m| m.content.contains("whole batch was reverted"))
        };
        assert!(!seen(1), "not with the failed batch's own results");
        assert!(seen(2), "next tool tail must carry the notice");
        assert!(state.pending_directives.is_empty());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&log_path);
    }

    #[tokio::test]
    async fn run_bets_hook_fires_at_step_head_and_post_batch_in_order() {
        let root = run_tmp("bets");
        std::fs::write(root.join("f.txt"), "old\n").unwrap();
        let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let client = ScriptClient {
            order: order.clone(),
            requests: Default::default(),
            queue: Mutex::new(VecDeque::from([
                script_resp(
                    vec![(
                        "c1",
                        "write",
                        serde_json::json!({"path": "f.txt", "content": "new\n"}),
                    )],
                    StopReason::ToolUse,
                ),
                text_resp("done"),
            ])),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let mut emitter = Emitter::new();
        let cancel = CancellationToken::new();
        let outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &RecBets {
                    order: order.clone(),
                },
                cfg: RunConfig::default(),
            },
            vec![Input::User("go".into())],
            &cancel,
        )
        .await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        // Step-head, post-batch, step-head interleave with the two model calls.
        assert_eq!(
            *order.lock().unwrap(),
            vec!["bets", "model", "bets", "bets", "model"],
        );
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "new\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn run_halt_budget_steps_after_grace_with_budget_frame() {
        assert_eq!(RunConfig::default().drain_timeout, Duration::from_secs(30));
        assert_eq!(LoopState::new().drain_timeout, Duration::from_secs(30));
        let root = run_tmp("halt");
        let order = Arc::new(Mutex::new(Vec::new()));
        let client = ScriptClient {
            order,
            requests: Default::default(),
            queue: Mutex::new(VecDeque::new()),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let max = state.budget.config().max_steps.get();
        state.budget.counters_mut().steps = max;
        let mut emitter = Emitter::new();
        let cancel = CancellationToken::new();
        // Configurable drain: 5s here proves the knob, 30s is the default.
        let cfg = RunConfig {
            drain_timeout: Duration::from_secs(5),
            ..RunConfig::default()
        };
        let outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &NoBets,
                cfg,
            },
            vec![Input::User("too late".into())],
            &cancel,
        )
        .await;
        assert!(
            matches!(outcome, Outcome::Halted(ref s) if s == "steps"),
            "got {outcome:?}"
        );
        assert!(state.budget.may_step().is_err()); // grace ran first, halt after
        assert_eq!(state.drain_timeout, Duration::from_secs(5));
        match emitter.history().last().unwrap() {
            AgentEvent::RunEnd { outcome, .. } => {
                assert!(matches!(outcome, agent_event::RunOutcome::Failed(_)))
            }
            other => panic!("expected RunEnd, got {other:?}"),
        }
        let turn_end = emitter
            .history()
            .iter()
            .find(|e| matches!(e, AgentEvent::TurnEnd { .. }))
            .unwrap();
        assert!(matches!(
            turn_end,
            AgentEvent::TurnEnd {
                reason: agent_event::TurnEndReason::BudgetExceeded,
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn run_halt_same_action_and_crash_input_source() {
        // Same action + same observation x4 trips the tripwire; lessons stay capped.
        let root = run_tmp("trip");
        std::fs::write(root.join("f.txt"), "x\n").unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let same = || {
            script_resp(
                vec![(
                    "c",
                    "write",
                    serde_json::json!({"path": "f.txt", "content": "x\n"}),
                )],
                StopReason::ToolUse,
            )
        };
        let client = ScriptClient {
            order,
            requests: Default::default(),
            queue: Mutex::new(VecDeque::from([same(), same(), same(), same()])),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let mut emitter = Emitter::new();
        let cancel = CancellationToken::new();
        let outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &NoBets,
                cfg: RunConfig::default(),
            },
            vec![Input::User("again".into())],
            &cancel,
        )
        .await;
        assert!(
            matches!(outcome, Outcome::Halted(ref s) if s == "same-action"),
            "got {outcome:?}"
        );
        assert!(state.lessons.len() <= 3);
        let _ = std::fs::remove_dir_all(&root);
        // Crash queues like User but keeps the Crash source in the log.
        let root = run_tmp("crash");
        let order = Arc::new(Mutex::new(Vec::new()));
        let client = ScriptClient {
            order,
            requests: Default::default(),
            queue: Mutex::new(VecDeque::from([text_resp("recovered")])),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let mut emitter = Emitter::new();
        let outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &NoBets,
                cfg: RunConfig::default(),
            },
            vec![Input::Crash("boom state".into())],
            &cancel,
        )
        .await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        // items[0] is the run Header, [1] the TurnStart, [2] the admitted Crash.
        match &state.items[2].kind {
            ItemKind::Input { source, text, .. } => {
                assert_eq!(*source, InputSource::Crash);
                assert_eq!(text, "boom state");
            }
            other => panic!("expected Crash Input, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
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
        assert!(s.budget.may_step().is_ok());
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

    #[tokio::test]
    async fn run_request_shape_history_delivered_once_collapse_5() {
        use agent_event::check_pairing;
        let root = run_tmp("shape");
        let order = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        // 7 write rounds, then a finishing text: the 8th request carries 7 tool
        // results, so collapse-5 has older material to shrink.
        let mut queue: VecDeque<Response> = VecDeque::new();
        for i in 1..=7 {
            let id = format!("c{i}");
            let path = format!("f{i}.txt");
            // Unique marker: the generic "step" would substring-match the
            // "steps N/M" budget tail on every request.
            let mut resp = script_resp(
                vec![(
                    id.as_str(),
                    "write",
                    serde_json::json!({"path": path, "content": "x\n"}),
                )],
                StopReason::ToolUse,
            );
            resp.message.content = format!("script-step-{i}");
            resp.message.thinking = Some(format!("reason-{i}"));
            queue.push_back(resp);
        }
        queue.push_back(text_resp("all done"));
        let client = ScriptClient {
            order,
            requests: requests.clone(),
            queue: Mutex::new(queue),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let mut emitter = Emitter::new();
        let cancel = CancellationToken::new();
        let outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &NoBets,
                cfg: RunConfig::default(),
            },
            vec![Input::User("shape-pin-goal".into())],
            &cancel,
        )
        .await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        let reqs = requests.lock().unwrap();
        assert_eq!(reqs.len(), 8); // 7 tool rounds + the finishing text call
        for (i, req) in reqs.iter().enumerate() {
            assert_eq!(req.messages[0].role, "system");
            let wire = serde_json::to_string(req).unwrap();
            // Unique input appears exactly once in the whole wire request:
            // a fitted system copy would make it twice.
            assert_eq!(
                wire.matches("shape-pin-goal").count(),
                1,
                "history duplicated in request {i}"
            );
            // No history message is echoed into the system string.
            let sys = &req.messages[0].content;
            for m in &req.messages[1..] {
                assert!(
                    m.content.is_empty() || !sys.contains(&m.content),
                    "request {i}: {:?} also fitted into system",
                    m.content
                );
            }
        }
        // Collapse-5 rides the raw path: oldest 2 of 7 tool results arrive
        // collapsed, each result delivered exactly once overall.
        let wire_last = serde_json::to_string(&reqs[7]).unwrap();
        for n in 1..=7 {
            assert_eq!(
                wire_last.matches(&format!("wrote f{n}.txt")).count(),
                1,
                "tool result f{n} must appear exactly once"
            );
        }
        let content_of = |id: &str| {
            reqs[7]
                .messages
                .iter()
                .find(|m| m.tool_call_id.as_deref() == Some(id))
                .unwrap_or_else(|| panic!("missing tool message {id}"))
                .content
                .clone()
        };
        assert_eq!(
            content_of("c1"),
            "[collapsed: 12b — re-open to edit] wrote f1.txt"
        );
        assert_eq!(
            content_of("c2"),
            "[collapsed: 12b — re-open to edit] wrote f2.txt"
        );
        assert_eq!(content_of("c3"), "wrote f3.txt");
        // c7 is the request's final message: the request-scoped budget line
        // rides its tail, so only the durable part matches exactly.
        assert!(
            content_of("c7").starts_with("wrote f7.txt\nbudgets remaining:"),
            "{:?}",
            content_of("c7")
        );
        // Collapse-5 collapses tool observations only; assistant reasoning
        // follows the keep-last-2 policy, so only steps 6-7 still carry it
        // into the 8th request.
        for i in 1..=7 {
            let want = format!("reason-{i}");
            let step = format!("script-step-{i}");
            let got = reqs[7]
                .messages
                .iter()
                .find(|m| m.role == "assistant" && m.content == step)
                .and_then(|m| m.thinking.as_deref());
            let expect = if i >= 6 { Some(want.as_str()) } else { None };
            assert_eq!(got, expect, "reasoning for step {i}");
        }
        assert!(check_pairing(emitter.history()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Prefix-cache stability end to end: the system head is byte-identical
    /// across requests and frozen at run start, and the only per-request bytes
    /// are the live budgets on the final message.
    #[tokio::test]
    async fn run_system_prefix_is_static_and_file_map_frozen() {
        let root = run_tmp("prefix");
        std::fs::write(root.join("a.rs"), "v1\n").unwrap();
        let client = ScriptClient {
            order: Arc::new(Mutex::new(Vec::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            queue: Mutex::new(VecDeque::from([
                script_resp(
                    vec![(
                        "w1",
                        "write",
                        serde_json::json!({"path": "b.rs", "content": "new\n"}),
                    )],
                    StopReason::ToolUse,
                ),
                script_resp(
                    vec![("r1", "read", serde_json::json!({"n": 1}))],
                    StopReason::ToolUse,
                ),
                text_resp("done"),
            ])),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let mut emitter = Emitter::new();
        let outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &NoBets,
                cfg: RunConfig::default(),
            },
            vec![Input::User("go".into())],
            &CancellationToken::new(),
        )
        .await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        assert!(root.join("b.rs").exists(), "mid-run write happened");
        let reqs = client.requests.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        // File map frozen at run start: b.rs was written mid-run and never
        // shows up; the head bytes match across all requests.
        let sys = &reqs[0].messages[0].content;
        assert!(sys.contains("a.rs") && !sys.contains("b.rs"), "{sys}");
        assert_eq!(sys, &reqs[1].messages[0].content);
        assert_eq!(sys, &reqs[2].messages[0].content);
        // Identical prefix up to each request's final message, the only one
        // the budget line touches.
        for pair in reqs.windows(2) {
            let head = &pair[0].messages[..pair[0].messages.len() - 1];
            assert!(pair[1].messages.len() > head.len(), "request lost history");
            for (m1, m2) in head.iter().zip(&pair[1].messages) {
                assert_eq!(format!("{m1:?}"), format!("{m2:?}"), "prefix rotated");
            }
        }
        // Fresh counters on the tail of every request: one step per call, one
        // action per executed tool, 15 tokens metered per settle.
        for (req, tail) in reqs.iter().zip([
            "budgets remaining: steps 19/20; actions 30/30; tokens 50000/50000",
            "budgets remaining: steps 18/20; actions 29/30; tokens 49985/50000",
            "budgets remaining: steps 17/20; actions 28/30; tokens 49970/50000",
        ]) {
            let last = &req.messages.last().unwrap().content;
            assert!(last.ends_with(tail), "{last:?} must end with {tail:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// DeepSeek thinking mode: turn 2's request must echo turn 1's assistant
    /// `thinking`, or the API 400s mid-run (measured, api.deepseek.com).
    #[tokio::test]
    async fn run_multi_turn_request_echoes_assistant_thinking() {
        let root = run_tmp("think");
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut first = script_resp(
            vec![(
                "c1",
                "write",
                serde_json::json!({"path": "a.txt", "content": "bye\n"}),
            )],
            StopReason::ToolUse,
        );
        first.message.thinking = Some("must edit a.txt".into());
        let client = ScriptClient {
            order: Arc::new(Mutex::new(Vec::new())),
            requests: requests.clone(),
            queue: Mutex::new(VecDeque::from([first, text_resp("done")])),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let mut emitter = Emitter::new();
        let outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &NoBets,
                cfg: RunConfig::default(),
            },
            vec![Input::User("edit the note".into())],
            &CancellationToken::new(),
        )
        .await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        // The stored assistant row keeps the thinking for replay.
        let stored = state
            .items
            .iter()
            .find_map(|i| match &i.kind {
                ItemKind::Assistant { message, .. } => Some(message),
                _ => None,
            })
            .expect("assistant row stored");
        assert_eq!(stored["thinking"], "must edit a.txt");
        let reqs = requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        // Turn 1 has no assistant history: nothing to echo there.
        assert!(reqs[0].messages.iter().all(|m| m.thinking.is_none()));
        let echoed = reqs[1]
            .messages
            .iter()
            .find(|m| m.role == "assistant")
            .expect("assistant history in turn 2");
        assert_eq!(echoed.thinking.as_deref(), Some("must edit a.txt"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn run_provider_failure_emits_provider_failed_then_fails() {
        use agent_event::check_pairing;
        let root = run_tmp("provfail");
        // Empty script: every complete() call returns Err(Transport(...)).
        let client = ScriptClient {
            order: Arc::new(Mutex::new(Vec::new())),
            requests: Default::default(),
            queue: Mutex::new(VecDeque::new()),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let mut emitter = Emitter::new();
        let cancel = CancellationToken::new();
        let outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &NoBets,
                cfg: RunConfig::default(),
            },
            vec![Input::User("go".into())],
            &cancel,
        )
        .await;
        // Production outcome path: retries exhaust, the gate closes, the
        // fatal error surfaces as Outcome::Failed.
        match &outcome {
            Outcome::Failed(msg) => assert!(msg.contains("script empty"), "got {msg}"),
            other => panic!("expected Failed, got {other:?}"),
        }
        let history = emitter.history();
        let errors = history
            .iter()
            .filter(|e| matches!(e, AgentEvent::Error { error } if error.code == "provider-failed"))
            .count();
        assert_eq!(errors, 3, "1 initial + 2 in-step retries: {history:?}");
        // Durable attempt trail: will_retry, will_retry, exhausted.
        let attempts: Vec<bool> = state
            .items
            .iter()
            .filter_map(|i| match &i.kind {
                ItemKind::Attempt { will_retry, .. } => Some(*will_retry),
                _ => None,
            })
            .collect();
        assert_eq!(attempts, vec![true, true, false]);
        assert!(check_pairing(history));
        match history.last().unwrap() {
            AgentEvent::RunEnd {
                outcome: agent_event::RunOutcome::Failed(msg),
                ..
            } => assert!(msg.contains("script empty"), "got {msg}"),
            other => panic!("expected failed RunEnd, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// First call fails with a metered truncation, then succeeds: F1b through
    /// the real `run()` path (extraction + budget + retry).
    struct MeteredFailThenOk {
        calls: Mutex<u32>,
        usage: Usage,
    }

    #[async_trait::async_trait]
    impl LlmClient for MeteredFailThenOk {
        async fn complete(&self, _model: &str, _req: &Request) -> Result<Response, LlmError> {
            let mut n = self.calls.lock().unwrap();
            *n += 1;
            if *n == 1 {
                return Err(LlmError::Metered {
                    source: "output truncated at 64 tokens".into(),
                    usage: Some(self.usage.clone()),
                });
            }
            drop(n);
            Ok(text_resp("done"))
        }
        fn capabilities(&self, _model: &str) -> Capabilities {
            Capabilities {}
        }
        async fn resolve_key(&self, _provider: &str) -> Result<Credentials, LlmError> {
            Ok(Credentials {
                api_key: String::new(),
            })
        }
    }

    #[tokio::test]
    async fn run_meters_failed_attempt_usage_then_retries() {
        let root = run_tmp("meteredfail");
        let client = MeteredFailThenOk {
            calls: Mutex::new(0),
            usage: Usage {
                input: 100,
                output: 20,
                cache_read: 0,
                cache_write: 0,
                reasoning: None,
                cost_usd: Some(0.02),
            },
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let mut emitter = Emitter::new();
        let outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &NoBets,
                cfg: RunConfig::default(),
            },
            vec![Input::User("go".into())],
            &CancellationToken::new(),
        )
        .await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        // Failed attempt (120) + settled attempt (15) both metered.
        assert_eq!(state.budget.counters().tokens, 135);
        assert_eq!(state.usage_totals.input_tokens, 110);
        assert_eq!(state.usage_totals.output_tokens, 25);
        assert_eq!(state.usage_totals.cost_usd, Some(0.03));
        let attempts: Vec<bool> = state
            .items
            .iter()
            .filter_map(|i| match &i.kind {
                ItemKind::Attempt { will_retry, .. } => Some(*will_retry),
                _ => None,
            })
            .collect();
        assert_eq!(attempts, vec![true], "one failed attempt, retried");
        let errors = emitter
            .history()
            .iter()
            .filter(|e| matches!(e, AgentEvent::Error { error } if error.code == "provider-failed"))
            .count();
        assert_eq!(errors, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    // --- bets site 2: proof-gated post-batch verdicts ---

    /// Verdict hook: keeps only the batch's first hunk, reverts the rest.
    struct PartialFirst;

    impl BetsHook for PartialFirst {
        fn on_post_batch(
            &self,
            claim: &bets::Claim,
            hunks: &[(String, bool)],
        ) -> bets::CommitVerdict {
            assert!(!claim.predicted_verdict.trim().is_empty());
            assert!(hunks.len() >= 2, "need a multi-hunk batch, got {hunks:?}");
            // Uniform proof: this batch's tool results all passed.
            assert!(hunks.iter().all(|(_, ok)| *ok), "{hunks:?}");
            bets::CommitVerdict::Partial {
                savepoint: bets::Savepoint {
                    kept_hunks: hunks[..1].iter().map(|(h, _)| h.clone()).collect(),
                    reverted_hunks: hunks[1..].iter().map(|(h, _)| h.clone()).collect(),
                },
            }
        }
    }

    /// Verdict hook: rejects every batch outright.
    struct AbortBatch;

    impl BetsHook for AbortBatch {
        fn on_post_batch(
            &self,
            _claim: &bets::Claim,
            hunks: &[(String, bool)],
        ) -> bets::CommitVerdict {
            assert!(!hunks.is_empty(), "gate sees the batch's hunks");
            bets::CommitVerdict::Aborted {
                reason: "stub rejects the batch".into(),
            }
        }
    }

    #[test]
    fn no_bets_defaults_keep_the_run_ungated_and_step_head_delegates() {
        // Site 2 default = permissive Committed: feature off unless a hook opts in.
        let claim = bets::Claim {
            predicted_verdict: "all green".into(),
            on_mismatch: "rollback".into(),
        };
        assert!(matches!(
            NoBets.on_post_batch(&claim, &[]),
            bets::CommitVerdict::Committed
        ));
        assert!(matches!(NoBets.on_step_head(3), PhaseVerdict::Continue));
        // Site 1 default delegates to on_step: an on_step-only hook still gates.
        struct StopOnStep;
        impl BetsHook for StopOnStep {
            fn on_step(&self) -> PhaseVerdict {
                PhaseVerdict::Return(Outcome::Done)
            }
        }
        assert!(matches!(
            StopOnStep.on_step_head(0),
            PhaseVerdict::Return(Outcome::Done)
        ));
    }

    #[tokio::test]
    async fn run_post_batch_partial_keeps_first_hunk_only_and_continues() {
        let root = run_tmp("partial");
        std::fs::write(root.join("a.rs"), "one\n").unwrap();
        std::fs::write(root.join("b.rs"), "two\n").unwrap();
        let client = ScriptClient {
            order: Arc::new(Mutex::new(Vec::new())),
            requests: Default::default(),
            queue: Mutex::new(VecDeque::from([
                script_resp(
                    vec![
                        (
                            "c1",
                            "write",
                            serde_json::json!({"path": "a.rs", "content": "A2\n"}),
                        ),
                        (
                            "c2",
                            "write",
                            serde_json::json!({"path": "b.rs", "content": "B2\n"}),
                        ),
                    ],
                    StopReason::ToolUse,
                ),
                text_resp("done"),
            ])),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let mut emitter = Emitter::new();
        let cancel = CancellationToken::new();
        let outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &PartialFirst,
                cfg: RunConfig::default(),
            },
            vec![Input::User("go".into())],
            &cancel,
        )
        .await;
        // Documented semantics: Partial rewrites the tree to the kept prefix,
        // then the run continues to its normal Done.
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        assert_eq!(std::fs::read_to_string(root.join("a.rs")).unwrap(), "A2\n");
        assert_eq!(std::fs::read_to_string(root.join("b.rs")).unwrap(), "two\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn run_post_batch_aborted_rolls_back_the_whole_batch() {
        let root = run_tmp("aborted");
        std::fs::write(root.join("a.rs"), "one\n").unwrap();
        let client = ScriptClient {
            order: Arc::new(Mutex::new(Vec::new())),
            requests: Default::default(),
            queue: Mutex::new(VecDeque::from([
                script_resp(
                    vec![(
                        "c1",
                        "write",
                        serde_json::json!({"path": "a.rs", "content": "A2\n"}),
                    )],
                    StopReason::ToolUse,
                ),
                text_resp("done"),
            ])),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let mut emitter = Emitter::new();
        let cancel = CancellationToken::new();
        let outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &AbortBatch,
                cfg: RunConfig::default(),
            },
            vec![Input::User("go".into())],
            &cancel,
        )
        .await;
        // Documented semantics: Aborted runs the full rollback path, the run
        // itself continues and finishes normally.
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        assert_eq!(std::fs::read_to_string(root.join("a.rs")).unwrap(), "one\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    // --- shared-scenario drift guard: both drivers, one script, one ordering ---

    /// Frame vocabulary in emission order (driver chrome filtered out).
    fn frame_order(history: &[AgentEvent]) -> Vec<&'static str> {
        history
            .iter()
            .filter_map(|e| match e {
                AgentEvent::TurnStart { .. } => Some("TurnStart"),
                AgentEvent::MessageStart { .. } => Some("MessageStart"),
                AgentEvent::MessageUpdate { .. } => Some("MessageUpdate"),
                AgentEvent::MessageEnd { .. } => Some("MessageEnd"),
                AgentEvent::ToolStart { .. } => Some("ToolStart"),
                AgentEvent::ToolEnd { .. } => Some("ToolEnd"),
                AgentEvent::TurnEnd { .. } => Some("TurnEnd"),
                _ => None,
            })
            .collect()
    }

    /// The frame order the durable rows demand: every terminal frame follows
    /// the append of its fact.
    fn project_durable(rows: &[&'static str]) -> Vec<&'static str> {
        rows.iter()
            .flat_map(|k| match *k {
                "TurnStart" => vec!["TurnStart"],
                "Assistant" => vec!["MessageStart", "MessageUpdate", "MessageEnd"],
                "ToolCall" => vec!["ToolStart"],
                "ToolResult" => vec!["ToolEnd"],
                "TurnEnd" => vec!["TurnEnd"],
                _ => vec![],
            })
            .collect()
    }

    #[tokio::test]
    async fn shared_scenario_both_drivers_agree_on_durable_before_emit_order() {
        use agent_event::check_pairing;

        // The shared script: user turn, one write tool call, its result, final text.
        fn write_call(id: &str) -> (&str, &str, serde_json::Value) {
            (
                id,
                "write",
                serde_json::json!({"path": "w.txt", "content": "x\n"}),
            )
        }

        // --- leg 1: run() on real siblings ---
        let run_root = run_tmp("shared-run");
        let run_client = ScriptClient {
            order: Arc::new(Mutex::new(Vec::new())),
            requests: Default::default(),
            queue: Mutex::new(VecDeque::from([
                script_resp(vec![write_call("c1")], StopReason::ToolUse),
                text_resp("done"),
            ])),
        };
        let registry = run_registry(&run_root);
        let mut run_state = LoopState::new();
        let mut run_emitter = Emitter::new();
        let cancel = CancellationToken::new();
        let outcome = run(
            &mut run_state,
            Run {
                provider: &run_client,
                registry: &registry,
                agent: "agent",
                workdir: &run_root,
                emitter: &mut run_emitter,
                bets: &NoBets,
                cfg: RunConfig::default(),
            },
            vec![Input::User("go".into())],
            &cancel,
        )
        .await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        let _ = std::fs::remove_dir_all(&run_root);

        // --- leg 2: drive_tick on the same script ---
        let tick_root = run_tmp("shared-tick");
        let registry = run_registry(&tick_root);
        let mut s = LoopState::new();
        s.stop_when_idle = true; // the scripted run is done after the final text
        let mut emitter = Emitter::new();
        let (itx, mut irx) = mpsc::channel(8);
        let (ptx, mut prx) = mpsc::channel(8);
        let (ttx, mut trx) = mpsc::channel(8);
        let cancel = CancellationToken::new();

        itx.send(Input::User("go".into())).await.unwrap();
        let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(v, PhaseVerdict::Continue));
        s.admit_steering(); // pre-boundary: the only place steering enters the log
        let root = CancellationToken::new();
        assert!(s.start_provider_call(&root).is_some());
        ptx.send(ProviderMsg::Settled {
            turn: 1,
            message: script_resp(vec![write_call("c1")], StopReason::ToolUse).message,
            stop: StopReason::ToolUse,
            usage: None,
        })
        .await
        .unwrap();
        let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(v, PhaseVerdict::Continue));
        // The ToolCall row is durable (at claim) before the tool runs.
        let prepared = registry.prepare(
            "agent",
            ToolCall {
                call_id: "c1".into(),
                name: "write".into(),
                args: serde_json::json!({"path": "w.txt", "content": "x\n"}),
            },
        );
        let inv = match prepared {
            tool_core::CallStatus::Dispatch(inv) => inv,
            tool_core::CallStatus::Result(r) => panic!("gate must allow the fake: {}", r.content),
        };
        let out = registry
            .resolve("write")
            .unwrap()
            .execute(inv, CancellationToken::new())
            .await
            .expect("write tool runs");
        ttx.send(ToolMsg {
            call_id: "c1".into(),
            result: ToolResult {
                content: out.content,
                is_error: false,
            },
        })
        .await
        .unwrap();
        let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(v, PhaseVerdict::Continue));
        assert!(s.start_provider_call(&root).is_some());
        ptx.send(ProviderMsg::Settled {
            turn: 1,
            message: text_resp("done").message,
            stop: StopReason::Stop,
            usage: None,
        })
        .await
        .unwrap();
        let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(v, PhaseVerdict::Return(Outcome::Done)));
        let _ = std::fs::remove_dir_all(&tick_root);

        // --- drift guard: same durable order, same frame order, frames follow rows ---
        let rows_of = |items: &[Item]| -> Vec<&'static str> {
            run_kinds(items)
                .into_iter()
                .filter(|k| *k != "Header")
                .collect()
        };
        let run_rows = rows_of(&run_state.items);
        let tick_rows = rows_of(&s.items);
        assert_eq!(run_rows, tick_rows, "durable rows drifted between drivers");
        assert_eq!(
            run_rows,
            vec![
                "TurnStart",
                "Input",
                "Assistant",
                "ToolCall",
                "ToolResult",
                "Assistant",
                "TurnEnd"
            ]
        );
        let run_frames = frame_order(run_emitter.history());
        let tick_frames = frame_order(emitter.history());
        assert_eq!(
            run_frames, tick_frames,
            "frame order drifted between drivers"
        );
        assert_eq!(
            project_durable(&run_rows),
            run_frames,
            "run(): frames must follow the durable rows"
        );
        assert_eq!(
            project_durable(&tick_rows),
            tick_frames,
            "drive_tick(): frames must follow the durable rows"
        );
        assert!(check_pairing(run_emitter.history()));
        assert!(check_pairing(emitter.history()));
    }
}
