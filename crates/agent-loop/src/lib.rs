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
use tool_core::{ToolCall, ToolOutcome, ToolResult};

/// Queued directives kept before the oldest is dropped (drop-oldest: the
/// newest counter warning outranks stale advice, mirroring the lessons cap).
const DIRECTIVE_CAP: usize = 3;

/// Assistant rows whose echoed reasoning survives folding into the derived
/// transcript.
const THINKING_KEEP: usize = 2;

/// Echoed-reasoning trim knob: env `THINKING_KEEP` overrides the keep-count
/// (A/B arm B = a large value echoes everything). Default stays
/// [`THINKING_KEEP`] (chosen-not-measured; the K=2 A/B validates). Not unit
/// tested for the env leg (env mutation races under parallel tests); the live
/// A/B measures the arms directly.
fn thinking_keep() -> usize {
    std::env::var("THINKING_KEEP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(THINKING_KEEP)
}

/// Model-facing notice when one failed tool reverts the whole batch.
const ROLLBACK_NOTICE: &str = "a tool in your last batch failed and the whole batch was reverted — your successful changes in it are gone; re-apply them";

/// Mid-run verification nudge: held-declare directive (spec draft verbatim)
/// plus the per-run budget. Full incentives only; Base/Contract never see it.
const VERIFY_NUDGE: &str = "Unverified declare held: no passing verification run since your last write. Either run the task's verification now, or state exactly what blocks it (missing tool/package/file) and what you verified instead. Declaring without one of those two is not completing.";
const VERIFY_NUDGE_CAP: u32 = 2;

/// Per-step provider-failure budget (init + per-step reset value).
/// Nesting: the provider adapter retries INSIDE each `complete` (cold-start
/// 503s up to 8 attempts with 60s sleeps, other retryables 5, plus the
/// truncation ladder), and this budget nests OUTSIDE it — each in-step retry
/// re-runs the full adapter ladder. Provider retry counts are unchanged here.
const STEP_RETRY_BUDGET: u32 = 2;

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
/// `VerifyHold` (Full incentives only) holds the turn alive on an unverified
/// declare: the run continues and the directive rides the next request tail
/// (request-scoped, never persisted), never an old ToolResult row.
#[derive(Debug, Clone)]
pub enum ClaimOutcome {
    Dispatch(Vec<ToolCall>),
    Truncated(usize),
    Done,
    VerifyHold,
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
    /// Args JSON at claim time, for the shared `observe_action` sig
    /// (`"{name}:{args}"`): `run` and `drive_tick` reconstruct the identical
    /// sig from this row, so the same-action tripwire sees the same bytes.
    pub args: String,
    /// True when this call is a verification run (`test`, or `exec` with
    /// pytest in args): an ok result marks verification since the last write.
    pub is_verification: bool,
    pub result: Option<ToolResult>,
}

/// One applied compaction checkpoint: the request fold replaces raw history
/// `[..keep_from]` with `summary`. The durable items are untouched — the log
/// keeps the raw transcript; only what the model sees is folded.
#[derive(Debug, Clone)]
pub struct Checkpoint {
    /// Index into the RAW history fold. Raw history only appends, so the
    /// index never shifts; a later checkpoint composes on top of it.
    pub keep_from: usize,
    pub summary: String,
}

/// Shared turn/step state for both drivers. `run` is the canonical shipped
/// driver; every field below is live in `run` (single-flight `in_flight` via
/// `start_provider_call`/`finish_provider_msg`, `steering`/`followups` via
/// `admit_steering`/`terminate`, `wake_requested`+`Phase::Maintenance` via the
/// same admit path). No field is dead machinery: nothing is annotated dead
/// and nothing may be removed under the parent-frozen ADD-only contract.
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
    /// Per-step provider-failure budget: reset to [`STEP_RETRY_BUDGET`] at
    /// each fresh step head (`start_provider_call` with no `call_model`
    /// retry latched), so a new step after success never inherits the
    /// previous step's consumed retries. In-step retry continuations keep
    /// their remaining budget. See the `STEP_RETRY_BUDGET` nesting note for
    /// the adapter 8x60s relationship.
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
    /// Mid-run verification nudge (Full only): nudges used (cap
    /// [`VERIFY_NUDGE_CAP`]), edits at the last nudge (second declare without
    /// an intervening write stays silent), passing-verify-since-write flag,
    /// and the request-scoped hold text for the next request tail.
    pub verify_nudges_used: u32,
    pub edits_at_last_nudge: u32,
    pub verified_since_write: bool,
    pub verify_hold: Option<String>,
    /// One-shot latches: the half-cap and near-cap directives fire once each.
    pub half_directive_sent: bool,
    pub late_directive_sent: bool,
    /// Ablation observability (Bet B→+A→+C): verdict/assessment counters for
    /// the run-end report. Never drives behavior.
    pub ablation: bets::AblationMetrics,
    /// Incentive scaffold level (B→+A→+C ablation): gates the workflow
    /// contract and the directive channel. Default Full = current behavior.
    pub incentives: IncentivesLevel,
    pub drain_timeout: Duration,
    pub drain_until: Option<Instant>,
    /// Compaction checkpoint (default off): the request fold replaces the raw
    /// prefix with one summary. `None` = no checkpoint applied.
    pub checkpoint: Option<Checkpoint>,
    /// Checkpoint estimate anchor: history length + provider-reported prompt
    /// tokens of the last settled request.
    pub anchor: Option<context::UsageAnchor>,
    /// Turn of the last checkpoint attempt: never twice in a row without new
    /// turns between (a refused summary is not retried every step).
    pub compacted_turn: Option<u64>,
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
            step_retries: STEP_RETRY_BUDGET,
            fatal_error: None,
            last_sig: String::new(),
            last_obs: 0,
            lessons: VecDeque::new(),
            pending_directives: VecDeque::new(),
            edits: 0,
            verify_nudges_used: 0,
            edits_at_last_nudge: 0,
            verified_since_write: false,
            verify_hold: None,
            half_directive_sent: false,
            late_directive_sent: false,
            ablation: bets::AblationMetrics::default(),
            incentives: IncentivesLevel::Full,
            drain_timeout: Duration::from_secs(30),
            drain_until: None,
            checkpoint: None,
            anchor: None,
            compacted_turn: None,
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
        // Per-step budget as named: a fresh step head resets, an in-step retry
        // continuation (`call_model` latched by the failed attempt) keeps its
        // remaining budget so 3 consecutive fails still exhaust (run-wide
        // would exhaust after any 2 fails across successes; unconditional
        // reset would never exhaust since every retry is a new step head).
        if !self.call_model {
            self.step_retries = STEP_RETRY_BUDGET;
        }
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
            // Attempts that returned usage are billed: meter before the
            // refund/retry fork so an exhausted ladder's re-sends are never
            // invisible (pre-generation failures carry no usage to meter).
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
        // Fail-closed serialization: a serialization failure must never store
        // a silent Null (which would later fold to "null" text). The marker
        // keeps the row parseable and names the failure explicitly.
        let value = serde_json::to_value(message).unwrap_or_else(|e| {
            serde_json::json!({
                "content": format!("[assistant serialization failed: {e}]"),
                "tool_calls": [],
                "thinking": null
            })
        });
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
        let stop_label = stop_label(stop);
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
                            args: tc.args.to_string(),
                            is_verification: false, // never dispatched: never verification
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
                    if let Some(text) = self.verify_nudge_due() {
                        self.verify_hold = Some(text.clone());
                        // Durable hold record: the request tail never reaches the
                        // log (derived folds items only) while directives ride a
                        // ToolResult tail, so the hold needs its own log row for
                        // replay to reproduce the fire. `Attempt` is log-only
                        // (never folded into `derived_messages`); the
                        // model-visible copy rides the next request as its own
                        // user-role row, so no old row is ever mutated.
                        append_to(
                            &mut self.items,
                            ItemKind::Attempt {
                                error: text,
                                will_retry: true,
                            },
                        );
                        // Hold the turn alive: the next request carries the
                        // directive on its own tail row.
                        self.call_model = true;
                        return ClaimOutcome::VerifyHold;
                    }
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
                            args: tc.args.to_string(),
                            is_verification: is_verification_call(&tc.name, &tc.args),
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

    /// Mid-run verification nudge gate (Full incentives only): fires when a
    /// declare would land Done while writes since the last nudge are
    /// unverified. Latches like the half/late one-shots: at most
    /// [`VERIFY_NUDGE_CAP`] per run, never twice without an intervening
    /// write, never inside the action cap's last 10%, never without headroom
    /// for a test run plus a re-declare (2 steps), and never when any budget
    /// cap already tripped (the same AND-gate [`Self::terminate`] halts on,
    /// so an exhausted budget gets no grace hold via `VerifyHold => continue`).
    /// Returns the directive text and burns one nudge when due.
    fn verify_nudge_due(&mut self) -> Option<String> {
        if self.incentives < IncentivesLevel::Full {
            return None;
        }
        if self.verify_nudges_used >= VERIFY_NUDGE_CAP {
            return None;
        }
        if self.edits <= self.edits_at_last_nudge {
            return None;
        }
        if self.verified_since_write {
            return None;
        }
        let cap = self.budget.config().actions_per_trial;
        let actions = self.budget.counters().actions_this_trial;
        if cap > 0 && actions.saturating_mul(10) >= cap.saturating_mul(9) {
            return None;
        }
        // Exhausted budget gets no grace hold: consult the guard `terminate`
        // halts on, covering steps/tokens/spend/wallclock/trials/refines and
        // both action caps in one AND-gate read.
        if self.budget.exceeded().is_some() {
            return None;
        }
        // Reserve test-then-declare: the hold buys two more model calls.
        let cfg = self.budget.config();
        let counters = self.budget.counters();
        if cfg.max_steps.get().saturating_sub(counters.steps) < 2 {
            return None;
        }
        // Explicit per-cap reads (duplicating `exceeded` for provenance):
        // tokens/spend/wallclock each veto the hold when already tripped.
        if cfg.max_tokens > 0 && counters.tokens >= cfg.max_tokens {
            return None;
        }
        if let Some(limit) = cfg.max_spend_cents {
            if counters.spent_cents >= limit {
                return None;
            }
        }
        if !cfg.max_wallclock.is_zero() && self.budget.elapsed() >= cfg.max_wallclock {
            return None;
        }
        self.verify_nudges_used += 1;
        self.edits_at_last_nudge = self.edits;
        Some(VERIFY_NUDGE.into())
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
        let probe = self
            .tool_calls
            .get(&msg.call_id)
            .map(|c| (c.name.clone(), c.is_verification));
        let ok = !msg.result.is_error;
        if let Some(c) = self.tool_calls.get_mut(&msg.call_id) {
            c.result = Some(msg.result);
        }
        if ok {
            if let Some((name, is_verification)) = probe {
                // A successful write invalidates earlier verification; a
                // passing verification run covers writes since the last one.
                if name == "edit" || name == "write" {
                    self.verified_since_write = false;
                } else if is_verification {
                    self.verified_since_write = true;
                }
            }
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
        if self.incentives < IncentivesLevel::Full {
            return; // ablation arm: the directive channel is off entirely
        }
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
        if self.budget.config().same_action_cycles > 0
            && self.budget.counters().same_action_streak >= self.budget.config().same_action_cycles
        {
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
    /// `COLLAPSE_KEEP` tool results go verbatim (up to `COLLAPSE_KEEP + H`
    /// under hysteresis H; see [`Self::raw_messages_with`]), older ones shrink
    /// to a `[collapsed: Nb — re-open to edit]` stub plus their first 120 chars
    /// as folded. Stable order preserved, so prefix caches survive.
    ///
    /// Echoed reasoning is trimmed to the last [`THINKING_KEEP`] assistant
    /// rows: on a thinking-heavy trace it was 64% of input (measured: 128k of
    /// 224k tokens), and keeping the last two cuts that ~68%. Wire key
    /// presence is unaffected — the wire layer still emits
    /// `reasoning_content: ""` for these rows in a thinking-mode
    /// conversation. chosen-not-measured: dropping older reasoning may
    /// degrade cross-turn reasoning continuity; the A/B is pending.
    ///
    /// An active [`Checkpoint`] replaces raw history `[..keep_from]` with its
    /// summary row; with no checkpoint this is exactly [`Self::raw_messages`],
    /// so the disabled path is byte-identical to collapse-5.
    pub fn derived_messages(&self) -> Vec<ProviderMessage> {
        let raw = self.raw_messages();
        let Some(cp) = &self.checkpoint else {
            return raw;
        };
        let mut out = Vec::with_capacity(raw.len() - cp.keep_from + 1);
        out.push(summary_message(&cp.summary));
        out.extend_from_slice(&raw[cp.keep_from..]);
        out
    }

    /// The raw log fold: every message, collapse + thinking trim applied, with
    /// the collapse hysteresis read from [`context::collapse_hysteresis`].
    fn raw_messages(&self) -> Vec<ProviderMessage> {
        self.raw_messages_with(context::collapse_hysteresis())
    }

    /// [`Self::raw_messages`] with the collapse hysteresis taken as a plain
    /// parameter: tests drive H directly instead of mutating the environment.
    fn raw_messages_with(&self, hysteresis: usize) -> Vec<ProviderMessage> {
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
                    // Fail-closed: a corrupt row must surface an explicit marker,
                    // never a silent default or raw-JSON content.
                    let am: AssistantMessage = match serde_json::from_value(message.clone()) {
                        Ok(am) => am,
                        Err(e) => AssistantMessage {
                            content: format!("[corrupt assistant row: {e}]"),
                            tool_calls: Vec::new(),
                            thinking: None,
                        },
                    };
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
        // keep the last thinking_keep() assistant rows, blank the older ones.
        let keep_from = out
            .iter()
            .filter(|m| m.role == "assistant")
            .count()
            .saturating_sub(thinking_keep());
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
        // Collapse boundary: rows before it stub, the tail stays verbatim.
        // H=0 moves it one row per fold (collapse-5); hysteresis H batches the
        // move to once per H rows so the untouched prefix stays cache-hittable.
        let boundary =
            context::collapse_boundary(tool_idx.len(), context::COLLAPSE_KEEP, hysteresis);
        for &i in &tool_idx[..boundary] {
            // Bytes at fold time + a 120-char head: enough to tell the
            // observation apart from a re-read of the file.
            let bytes = out[i].content.len();
            let head: String = out[i].content.chars().take(120).collect();
            out[i].content = format!("[collapsed: {bytes}b — re-open to edit] {head}");
        }
        out
    }
}

impl Default for LoopState {
    fn default() -> Self {
        Self::new()
    }
}

/// A passing result on one of these counts as a verification run since the
/// last write: the `test` tool, or `exec` whose `cmd` arg runs a test
/// command (`pytest`, `cargo test`, `go test`, `npm test`, `npm run test`).
/// Only the command position counts: `pip install pytest` and `grep pytest`
/// name pytest without running it, so neither verifies.
fn is_verification_call(name: &str, args: &Value) -> bool {
    if name == "test" {
        return true;
    }
    if name != "exec" {
        return false;
    }
    let cmd = args.get("cmd").and_then(Value::as_str).unwrap_or("");
    // Shell chains run left to right; any segment may be the verify step.
    cmd.to_lowercase()
        .replace("&&", ";")
        .replace("||", ";")
        .split([';', '|'])
        .any(|segment| {
            let tokens: Vec<&str> = segment.split_whitespace().collect();
            match tokens.as_slice() {
                [first, ..] if *first == "pytest" || first.ends_with("/pytest") => true,
                ["cargo", "test", ..] | ["go", "test", ..] | ["npm", "test", ..] => true,
                ["npm", "run", "test", ..] => true,
                _ => false,
            }
        })
}

/// Stable stop-reason label for the durable log: explicit match, never
/// `Debug`, so a variant rename cannot silently rotate stored bytes.
fn stop_label(stop: StopReason) -> String {
    match stop {
        StopReason::Pending => "Pending",
        StopReason::Stop => "Stop",
        StopReason::ToolUse => "ToolUse",
        StopReason::MaxTokens => "MaxTokens",
        StopReason::Refused => "Refused",
        StopReason::Error => "Error",
        StopReason::Aborted => "Aborted",
        StopReason::Deferred => "Deferred",
    }
    .to_owned()
}

/// Header of the one row a checkpoint injects: the model must read it as
/// context state, not as a new user instruction.
const CHECKPOINT_PREFIX: &str =
    "[context checkpoint: earlier turns were summarized to save tokens; the durable log still holds them]";

fn summary_message(summary: &str) -> ProviderMessage {
    ProviderMessage {
        role: "user".into(),
        content: format!("{CHECKPOINT_PREFIX}\n{summary}"),
        tool_calls: Vec::new(),
        tool_call_id: None,
        thinking: None,
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

/// Channel test harness over the same step helpers `run` ships.
///
/// `run` is the canonical shipped driver (headless multi-tick assembly on
/// real siblings); `drive_tick` is a thin test harness for select!-shape
/// tests, driving one inbox/provider/tool/cancel tick through the shared
/// helpers (`step_claim` + `emit_message_frames`, `settle_tool_msg` =
/// `note_tool_execution` + `record_tool_result` + `settle_tool_tail`). No
/// harness-local copies: `VerifyHold`, `record_tool_result`,
/// `observe_action`, and the `edits`/`actions` increments behave identically
/// because both drivers call the same fns.
///
/// `LoopState` field note: every field is live in `run` — single-flight
/// `in_flight` (`start_provider_call`/`finish_provider_msg`) and `steering`
/// (`admit_steering`) ARE used and kept; there is no dead machinery to
/// annotate, and none may be removed under the frozen contract.
///
/// Ordering rule: the durable log append always
/// precedes its terminal frame in code order — `step_claim` /
/// `record_tool_result` / the TurnEnd append below run before the matching
/// `emit`, so replay from the log agrees with replay from events.
/// Starting the next provider call (should_call_model -> start_provider_call)
/// stays the caller's job; the multi-tick run() is the shipped driver.
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
                        // Shared with `run`: the same `step_claim` + frames order.
                        let outcome = state.step_claim(message.clone(), stop);
                        emit_message_frames(state, &message, usage.as_ref(), emitter); // ...before these frames.
                        match outcome {
                            ClaimOutcome::Dispatch(calls) => {
                                for c in &calls {
                                    emitter.emit(AgentEvent::ToolStart {
                                        id: c.call_id.clone(),
                                        name: c.name.clone(),
                                        args: c.args.clone(),
                                    });
                                }
                            }
                            ClaimOutcome::VerifyHold => {
                                // Same as `run`'s `VerifyHold => continue`: the
                                // turn stays alive (`call_model` latched in
                                // `step_claim`) and the next request carries
                                // `verify_hold` on its tail. No `ToolStart`.
                            }
                            ClaimOutcome::Done
                            | ClaimOutcome::Truncated(_)
                            | ClaimOutcome::Refused
                            | ClaimOutcome::HardExit(_) => {}
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
            // Shared with `run`: `settle_tool_msg` = `note_tool_execution`
            // (`observe_action` + `edits += 1` on ok `edit`|`write`) +
            // `record_tool_result` + `settle_tool_tail`, the same order `run`
            // uses per executed call.
            let shadow = tm.clone();
            if settle_tool_msg(state, tm) {
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

/// Incentive scaffold level for the B→+A→+C ablation. `Base` ships no
/// workflow contract and drops the directive channel; `Contract` adds the
/// static workflow contract; `Full` (default = current behavior) adds the
/// model-facing directives (cap notices, lessons, rollback notices).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum IncentivesLevel {
    Base,
    Contract,
    #[default]
    Full,
}

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
    /// Ablation knob: incentive scaffold level (default [`IncentivesLevel::Full`]).
    pub incentives: IncentivesLevel,
    /// Per-hunk incremental proof probe (Bet A): whitespace-argv, no shell,
    /// run against each leading-prefix tree in `workdir`. `None` = the uniform
    /// proof flag (the batch's tool results all passed).
    pub proof_cmd: Option<String>,
    /// Compaction checkpoint knobs (default OFF: no summary call, no fold).
    pub compaction: context::CompactionConfig,
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
            incentives: IncentivesLevel::Full,
            proof_cmd: None,
            compaction: context::CompactionConfig::default(),
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
    if cfg.incentives >= IncentivesLevel::Contract {
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
    }
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
    // The held nudge rides its own user-role row, never merged into the
    // model's declare text (the last derived row after an unverified declare
    // is the model's own assistant row). Peek only: `run` takes on
    // successful send, so a provider Err+retry re-arms instead of losing it.
    let budget = budget_line(state);
    if let Some(last) = messages[1..].last_mut() {
        last.content.push('\n');
        last.content.push_str(&budget);
    } else {
        messages.push(ProviderMessage {
            role: "user".into(),
            content: budget,
            tool_calls: Vec::new(),
            tool_call_id: None,
            thinking: None,
        });
    }
    // Held verification nudge (Full only; None elsewhere, so other arms stay
    // byte-identical): peeked here, taken by `run` only on successful send.
    if let Some(nudge) = state.verify_hold.as_ref().cloned() {
        messages.push(ProviderMessage {
            role: "user".into(),
            content: nudge,
            tool_calls: Vec::new(),
            tool_call_id: None,
            thinking: None,
        });
    }
    Request {
        messages,
        tools: registry.definitions(),
        max_tokens: cfg.max_tokens,
        thinking: Thinking::Auto,
        extras: Value::Null,
    }
}

/// The compactor's view of one message: assistant thinking and tool calls are
/// folded into the text (the summarizer must see what was done), everything
/// else keeps its content. This same view feeds the token estimate, so an
/// appended assistant row is charged for its thinking and arguments.
fn compact_view(messages: &[ProviderMessage]) -> Vec<context::CompactMessage> {
    messages
        .iter()
        .map(|m| {
            let content = if m.role == "assistant" {
                let mut parts: Vec<String> = Vec::new();
                if let Some(t) = &m.thinking {
                    if !t.is_empty() {
                        parts.push(format!("[thinking] {t}"));
                    }
                }
                if !m.content.is_empty() {
                    parts.push(m.content.clone());
                }
                for c in &m.tool_calls {
                    parts.push(format!("{}({})", c.name, c.args));
                }
                parts.join("\n")
            } else {
                m.content.clone()
            };
            context::CompactMessage {
                role: m.role.clone(),
                content,
            }
        })
        .collect()
}

/// Compaction checkpoint at the step head: estimate the folded context from
/// the last settled request's usage plus chars/4 for what followed, and when
/// it crosses `budget_tokens * frac` replace the older prefix with ONE
/// summarizer call's output. Returns true when a checkpoint was applied.
///
/// The summary call is metered like any other request (tokens, spend, run
/// totals); a summary that errored, hit the length stop, or came back empty
/// is refused and the window is left unchanged. Either way the turn is
/// latched: never compact twice in a row without new turns between.
async fn checkpoint<P: LlmClient>(
    state: &mut LoopState,
    provider: &P,
    cfg: &RunConfig,
    emitter: &mut Emitter,
) -> bool {
    let cc = &cfg.compaction;
    if !cc.enabled || state.compacted_turn == Some(state.turn) {
        return false;
    }
    let folded = state.derived_messages();
    let view = compact_view(&folded);
    let estimate = context::estimate_tokens(&view, state.anchor.as_ref());
    if !context::compaction_due(estimate, state.budget.config().max_tokens, cc) {
        return false;
    }
    let Some(cut) = context::cut_point(&view, cc.keep_tokens) else {
        return false;
    };
    state.compacted_turn = Some(state.turn); // attempt latch: no retry storm
    let req = Request {
        messages: vec![
            ProviderMessage {
                role: "system".into(),
                content: context::SUMMARY_SYSTEM.into(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                thinking: None,
            },
            ProviderMessage {
                role: "user".into(),
                content: format!(
                    "<conversation>\n{}\n</conversation>\n\n{}",
                    context::summary_payload(&view[..cut]),
                    context::SUMMARY_PROMPT
                ),
                tool_calls: Vec::new(),
                tool_call_id: None,
                thinking: None,
            },
        ],
        tools: Vec::new(), // a summary must not act
        max_tokens: cfg.max_tokens,
        thinking: Thinking::Off,
        extras: Value::Null,
    };
    let summary = match provider.complete(&cfg.model, &req).await {
        Ok(resp) => {
            state.record_usage(&resp.billed_usage());
            match resp.stop {
                StopReason::Stop if !resp.message.content.trim().is_empty() => {
                    Ok(resp.message.content.trim().to_owned())
                }
                stop => Err(format!("summary stop {stop:?}")),
            }
        }
        Err(e) => {
            if let LlmError::Metered { usage: Some(u), .. } = &e {
                state.record_usage(u);
            }
            Err(format!("summary call failed: {e:?}"))
        }
    };
    let summary = match summary {
        Ok(s) => s,
        Err(why) => {
            emitter.emit(AgentEvent::Error {
                error: AgentError {
                    code: "compaction-refused".into(),
                    message: why,
                },
            });
            return false;
        }
    };
    // Compose with an earlier checkpoint: the new summary absorbs the old one
    // (folded[0] is the previous summary), so the cut maps back to raw history.
    let keep_from = match &state.checkpoint {
        Some(prev) => prev.keep_from + cut - 1,
        None => cut,
    };
    state.checkpoint = Some(Checkpoint { keep_from, summary });
    state.anchor = None; // the compacted request re-anchors on its own usage
    true
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

/// Per-hunk incremental proof (Bet A): probe every leading prefix against the
/// tree it actually produces — restore baseline + hunks[0..=k], run `proof_cmd`
/// (whitespace argv, no shell, 60s kill; timeout/failure = unproven), record
/// ok_k. The bet gate keeps the proven LEADING prefix
/// ([`bets::split_savepoint`]); a later hunk that passes after a failing
/// prefix is not proven against committed state and reverts with the rest.
/// Leaves the tree at baseline + all hunks (the last prefix); the verdict
/// mapping below performs the final restore.
async fn incremental_hunks(
    tree: &snapshot::TreeService,
    workdir: &Path,
    proof_cmd: &str,
) -> Result<Vec<(String, bool)>, String> {
    let diff = tree.diff().map_err(|e| format!("snapshot diff: {e}"))?;
    let patch = tree
        .patch(&diff)
        .map_err(|e| format!("snapshot patch: {e}"))?;
    let hunks = split_patch(&patch);
    let mut flags = Vec::with_capacity(hunks.len());
    for k in 0..hunks.len() {
        tree.restore_hunks(&hunks[..=k])
            .map_err(|e| format!("snapshot restore: {e}"))?;
        let mut argv = proof_cmd.split_whitespace();
        let bin = argv.next().ok_or_else(|| "proof-cmd empty".to_string())?;
        let ok = tokio::time::timeout(
            Duration::from_secs(60),
            tokio::process::Command::new(bin)
                .args(argv)
                .current_dir(workdir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                // kill_on_drop (tokio default false): a timed-out proof child
                // must not keep running and mutating the workdir after we
                // report timeout, mirroring tools-std's exec guard.
                .kill_on_drop(true)
                .status(),
        )
        .await
        .map(|s| s.is_ok_and(|s| s.success()))
        .unwrap_or(false);
        flags.push((hunks[k].clone(), ok));
    }
    Ok(flags)
}

/// [`run`] tool mapping: the tools lane reports its verdict in
/// [`ToolOutcome::success`] (serde default false), so a failing check is an
/// error result even though its content is ordinary output. `Err` never
/// reaches here: the call site maps it to `is_error: true`.
fn outcome_to_result(outcome: ToolOutcome) -> ToolResult {
    ToolResult {
        content: if outcome.truncated {
            format!("{}\n[truncated]", outcome.content)
        } else {
            outcome.content
        },
        is_error: !outcome.success,
    }
}

/// Shared step helpers: `run` (the shipped driver) and `drive_tick` (the
/// channel test harness) call the same fns, so `VerifyHold`,
/// `record_tool_result`, `observe_action`, and the `edits`/`actions`
/// increments behave identically. No driver inlines its own copy.
///
/// Per-tool execution effects: the `observe_action` tripwire input plus the
/// `edits += 1` on a successful `edit`|`write`. Callers run this BEFORE
/// `record_tool_result` (the `run` order), so the counters describe the
/// execution the durable `ToolResult` row then records. Returns the
/// tripwire halt, if any (callers let `terminate` own the halt; the return
/// is observed, never branched on inline).
fn note_tool_execution(
    state: &mut LoopState,
    name: &str,
    args_str: &str,
    result: &ToolResult,
) -> Option<BudgetHalt> {
    let halt = state.observe_action(&format!("{name}:{args_str}"), &result.content);
    if !result.is_error && matches!(name, "edit" | "write") {
        state.edits += 1;
    }
    halt
}

/// Shared tool-tail settlement: counter directives, the budget wrap-up nudge,
/// and delivery onto the newest `ToolResult` tail. Must run BEFORE the log
/// sync, so the file never holds a stale tail and the row keeps the exact
/// model-visible text. Both drivers call this after every recorded tool
/// result.
fn settle_tool_tail(state: &mut LoopState) {
    state.queue_directives();
    state.apply_budget_nudge();
    state.deliver_directives();
}

/// Shared single-result settlement for the channel harness: the same
/// observe/`edits`/`record`/tail order `run` uses per executed call. Looks
/// the `name`/`args` sig up from the claim row, so the tripwire sees the
/// identical bytes. Returns false when the call id was unknown or already
/// answered (no effects applied).
fn settle_tool_msg(state: &mut LoopState, msg: ToolMsg) -> bool {
    let open = matches!(
        state.tool_calls.get(&msg.call_id),
        Some(c) if c.result.is_none()
    );
    if !open {
        return false;
    }
    let (name, args_str) = state
        .tool_calls
        .get(&msg.call_id)
        .map(|c| (c.name.clone(), c.args.clone()))
        .unwrap_or_default();
    let _ = note_tool_execution(state, &name, &args_str, &msg.result);
    let recorded = state.record_tool_result(msg);
    debug_assert!(recorded);
    settle_tool_tail(state);
    true
}

/// Batch-scope counter snapshot: the rows `tree.rollback` (or a gate
/// `Aborted`) destroys must not linger in the counters. `edits`,
/// `actions_this_trial` (+ the tripwire streak/sig that describes those
/// actions), and `verified_since_write` are refunded to this snapshot on a
/// full-batch rollback. Directive one-shot latches are NOT refunded: fired
/// text is already durable on a `ToolResult` tail and must not repeat.
#[derive(Debug, Clone)]
struct BatchSnapshot {
    edits: u32,
    actions_this_trial: u32,
    same_action_streak: u32,
    last_sig: String,
    last_obs: u64,
    verified_since_write: bool,
}

/// Capture the pre-batch counters. Call once per `Dispatch` batch, before
/// the first tool executes.
fn snapshot_batch(state: &LoopState) -> BatchSnapshot {
    BatchSnapshot {
        edits: state.edits,
        actions_this_trial: state.budget.counters().actions_this_trial,
        same_action_streak: state.budget.counters().same_action_streak,
        last_sig: state.last_sig.clone(),
        last_obs: state.last_obs,
        verified_since_write: state.verified_since_write,
    }
}

/// Refund a fully rolled-back batch to its pre-batch snapshot. Call after
/// `tree.rollback` (tool-error path) and after a gate `Aborted` restore.
/// A gate `Partial` keeps its prefix on disk, so its counters stand (the
/// kept hunks still describe edits; per-hunk counter attribution is
/// deferred).
fn refund_batch(state: &mut LoopState, snap: &BatchSnapshot) {
    state.edits = snap.edits;
    state.budget.counters_mut().actions_this_trial = snap.actions_this_trial;
    state.budget.counters_mut().same_action_streak = snap.same_action_streak;
    state.last_sig = snap.last_sig.clone();
    state.last_obs = snap.last_obs;
    state.verified_since_write = snap.verified_since_write;
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
    state.incentives = cfg.incentives;
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
        // Checkpoint before the real request, after the budget admitted the
        // step: the summary request is real spend and must never be paid for
        // a step the guard would have refused. Raced against cancel so Ctrl-C
        // during the summary call returns promptly (parent-side only; the
        // race drops the summary future and cancels the per-turn child token).
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {}
            _ = checkpoint(state, provider, &cfg, emitter) => {}
        }
        if cancel.is_cancelled() {
            turn_token.cancel();
            state.stop_hard = true;
            state.gate.begin_abort();
        }
        let req = build_request(state, registry, workdir, &cfg);
        // Parent-side cancel race: Ctrl-C during a model call returns promptly
        // instead of waiting out the adapter's 8x60s ladder. The `LlmClient`
        // trait is frozen (no token param), so the cancel branch drops the
        // `complete` future and cancels the per-turn child token.
        let completed = tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            res = provider.complete(&cfg.model, &req) => Some(res),
        };
        let completed = match completed {
            None => {
                turn_token.cancel();
                state.stop_hard = true;
                state.gate.begin_abort();
                Err(LlmError::Metered {
                    source: "cancelled".into(),
                    usage: None,
                })
            }
            Some(res) => res,
        };
        match completed {
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
                // The request (including any peeked hold row) reached the
                // provider: consume the hold now. A provider Err below keeps
                // it armed for the in-step retry.
                state.verify_hold.take();
                let turn = state.turn;
                // Checkpoint anchor: this request's history length and its
                // provider-reported prompt tokens (the final attempt's, not
                // the retry ladder's sum). History only (`len() - 1`: the one
                // system head), so the next estimate walks exactly the
                // messages appended after it.
                state.anchor = Some(context::UsageAnchor {
                    messages: req.messages.len().saturating_sub(1),
                    input_tokens: resp.usage.input,
                });
                // Meter the BILL, not just the final attempt: a recovered
                // retry ladder was billed every failed re-send too.
                let billed = resp.billed_usage();
                state.finish_provider_msg(ProviderMsg::Settled {
                    turn,
                    message: resp.message.clone(),
                    stop: resp.stop,
                    usage: Some(billed.clone()),
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
                emit_message_frames(state, &resp.message, Some(&billed), emitter);
                match outcome {
                    ClaimOutcome::VerifyHold => {
                        // Unverified declare held: the turn stays alive and
                        // the next request carries the directive on its own row.
                        // Durable hold record syncs at the next loop head before
                        // that request, so the file never lags the wire.
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
                        continue;
                    }
                    ClaimOutcome::Done => {
                        // Already-started final step: `record_step` at the head
                        // may have hit max, so `terminate`'s `exceeded` would
                        // preempt this Done with Halted(steps). The admitted
                        // step's declare wins when drained with no followups.
                        if state.in_flight.is_none()
                            && state.open_tools() == 0
                            && !state.call_model
                            && state.steering.is_empty()
                            && state.followups.is_empty()
                            && matches!(
                                state.budget.exceeded().as_ref().map(|e| &e.halt),
                                Some(BudgetHalt::Steps)
                            )
                        {
                            return finish_run(
                                state,
                                writer.as_mut(),
                                emitter,
                                run_id,
                                &mut synced,
                                Outcome::Done,
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
                    ClaimOutcome::Truncated(_) | ClaimOutcome::Refused => {
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
                        // Pre-batch counters: a full rollback refunds `edits` /
                        // `actions` / `verified_since_write` past the verdict.
                        let batch_snap = snapshot_batch(state);
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
                                    // Parent-side cancel race: Ctrl-C during a tool
                                    // returns promptly instead of waiting it out.
                                    // The race drops the `execute` future and
                                    // cancels the per-turn child token (which
                                    // owns each tool's child token) when it loses.
                                    let res = match registry.resolve(&inv.name) {
                                        Some(tool) => {
                                            let tool_token = turn_token.child_token();
                                            let raced = tokio::select! {
                                                biased;
                                                _ = cancel.cancelled() => None,
                                                r = tool.execute(inv, tool_token) => Some(r),
                                            };
                                            match raced {
                                                None => {
                                                    turn_token.cancel();
                                                    state.stop_hard = true;
                                                    state.gate.begin_abort();
                                                    ToolResult {
                                                        content: "cancelled".into(),
                                                        is_error: true,
                                                    }
                                                }
                                                Some(Ok(o)) => outcome_to_result(o),
                                                Some(Err(e)) => ToolResult::from(e),
                                            }
                                        }
                                        None => ToolResult {
                                            content: format!("unknown tool: {name}"),
                                            is_error: true,
                                        },
                                    };
                                    batch_failed |= res.is_error;
                                    // Shared with `drive_tick`: the same
                                    // `note_tool_execution` + `record` order.
                                    let _ = note_tool_execution(state, &name, &args, &res);
                                    state.record_tool_result(ToolMsg {
                                        call_id: c.call_id.clone(),
                                        result: res.clone(),
                                    });
                                }
                            }
                            // Directives and the nudge land on the fresh tail
                            // BEFORE the sync, so the file never holds a stale
                            // tail and the row keeps the exact model text.
                            // Shared with `drive_tick` (`settle_tool_tail`).
                            settle_tool_tail(state);
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
                        let hunks = {
                            let probe = match &cfg.proof_cmd {
                                Some(cmd) => incremental_hunks(&tree, workdir, cmd).await,
                                None => batch_hunks(&tree, !batch_failed),
                            };
                            match probe {
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
                            // Rolled-back edits must not linger in the
                            // counters: refund to the pre-batch snapshot.
                            refund_batch(state, &batch_snap);
                            // Not counted in AblationMetrics.rollbacks: this
                            // legacy rollback is pre-gate and identical in all
                            // ablation arms. rollbacks = GATE interventions
                            // (Partial/Aborted verdicts) only.
                        }
                        // Verdict→tree mapping: Committed stands (the failed-
                        // batch rollback above is the existing path either way),
                        // Partial restores the kept prefix from baseline, Aborted
                        // runs the existing rollback path again (idempotent).
                        // A restore error fails closed: restore_hunks leaves the
                        // tree at baseline when any fragment does not apply.
                        let observation = if batch_failed {
                            "tool batch results not all passed"
                        } else {
                            "tool batch results all passed"
                        };
                        state
                            .ablation
                            .note_assessment(bets::assess_claim(&claim, observation));
                        let verdict = bets.on_post_batch(&claim, &hunks);
                        if !hunks.is_empty() {
                            // The gate's domain is patch batches: an empty
                            // batch has nothing proven, so its Committed must
                            // not inflate proven_hunks. rollbacks counts gate
                            // interventions (Partial/Aborted) only — the
                            // legacy failed-batch rollback above is pre-gate
                            // and identical across ablation arms.
                            state.ablation.note_commit(&verdict);
                        }
                        match verdict {
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
                                // Gate-aborted batch is a full rollback: the
                                // counters refund like the tool-error path.
                                // (`Partial` keeps its prefix, so its
                                // counters stand.)
                                refund_batch(state, &batch_snap);
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
                args: "null".into(),
                is_verification: false,
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
                args: "null".into(),
                is_verification: false,
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

    /// Tool-only history of `n` rows, content `obs-{i}` (5 bytes each).
    fn tool_history(n: usize) -> LoopState {
        let mut s = LoopState::new();
        for i in 0..n {
            s.items.push(Item {
                seq: s.items.len() as u64,
                id: format!("t{i}"),
                parent_id: None,
                recorded_at: SystemTime::now(),
                kind: ItemKind::ToolResult {
                    call_id: format!("c{i}"),
                    content: format!("obs-{i}"),
                    is_error: false,
                    recovery: None,
                },
            });
        }
        s
    }

    /// The boundary a fold used: the contiguous leading stub prefix (fixtures
    /// are tool-only, so it is also the stub count). A stub behind a verbatim
    /// row would mean the fold moved the boundary one row at a time.
    fn stub_boundary(msgs: &[ProviderMessage]) -> usize {
        const STUB: &str = "[collapsed: ";
        let b = msgs.iter().filter(|m| m.content.starts_with(STUB)).count();
        assert!(
            msgs[..b].iter().all(|m| m.content.starts_with(STUB)),
            "stubs must be the leading prefix"
        );
        assert!(
            msgs[b..].iter().all(|m| !m.content.starts_with(STUB)),
            "stub behind a verbatim row: boundary moved one row at a time"
        );
        b
    }

    /// H=0 is today's collapse-5 fold, byte for byte: 3 of 8 tool rows stub,
    /// the last 5 stay verbatim; the env-less default folds the same bytes.
    #[test]
    fn collapse_hysteresis_zero_is_byte_identical_to_collapse_5() {
        let s = tool_history(8);
        let msgs = s.raw_messages_with(0);
        let got: Vec<(&str, String, Option<&str>)> = msgs
            .iter()
            .map(|m| {
                (
                    m.role.as_str(),
                    m.content.clone(),
                    m.tool_call_id.as_deref(),
                )
            })
            .collect();
        let stub = |i: usize| format!("[collapsed: 5b — re-open to edit] obs-{i}");
        let want = vec![
            ("tool", stub(0), Some("c0")),
            ("tool", stub(1), Some("c1")),
            ("tool", stub(2), Some("c2")),
            ("tool", "obs-3".into(), Some("c3")),
            ("tool", "obs-4".into(), Some("c4")),
            ("tool", "obs-5".into(), Some("c5")),
            ("tool", "obs-6".into(), Some("c6")),
            ("tool", "obs-7".into(), Some("c7")),
        ];
        assert_eq!(got, want);
        // Default knob (env unset) takes the byte-identical path.
        assert_eq!(
            serde_json::to_string(&s.raw_messages()).unwrap(),
            serde_json::to_string(&msgs).unwrap()
        );
    }

    /// H=5 over a growing history: one boundary move per 5 added rows, each
    /// moving a 5-row batch (H=0 moves every row: 25 moves vs 5). The verbatim
    /// window stays inside [COLLAPSE_KEEP, COLLAPSE_KEEP + H] throughout.
    #[test]
    fn collapse_hysteresis_moves_boundary_once_per_h_rows_in_one_batch() {
        let folds: Vec<(usize, usize)> = (1..=30)
            .map(|t| (t, stub_boundary(&tool_history(t).raw_messages_with(5))))
            .collect();
        let mut moves = Vec::new();
        let mut prev = 0;
        for &(t, b) in &folds {
            assert_eq!(b, 5 * (t.saturating_sub(5) / 5), "boundary at t={t}");
            let window = t - b;
            assert!(
                (5.min(t)..=10).contains(&window),
                "t={t}: verbatim window {window} outside [KEEP, KEEP+H]"
            );
            if b != prev {
                moves.push(t);
                prev = b;
            }
        }
        // Exact move turns; between moves the stub set never grows.
        assert_eq!(moves, vec![10, 15, 20, 25, 30]);
        for w in folds.windows(2) {
            let delta = w[1].1 - w[0].1;
            assert!(
                delta == 0 || delta == 5,
                "t={}: boundary moved {delta} rows, not one H-batch",
                w[1].0
            );
        }
        // H=0 control: the boundary tracks the tail, one row per fold.
        let h0: Vec<usize> = (1..=30)
            .map(|t| stub_boundary(&tool_history(t).raw_messages_with(0)))
            .collect();
        assert_eq!(
            h0,
            (1..=30usize)
                .map(|t| t.saturating_sub(5))
                .collect::<Vec<_>>()
        );
        assert_eq!(h0.iter().filter(|&&b| b != 0).count(), 25);
        assert_eq!(moves.len(), 5);
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

    /// One successful tool round through the claim seam, mirroring run():
    /// Dispatch registers name+args, the ok result lands, edits count here
    /// (run() owns the counter in production).
    fn ok_round(s: &mut LoopState, id: &str, name: &str, args: Value) {
        let outcome = s.step_claim(
            assistant(vec![ToolCallRef {
                id: id.into(),
                name: name.into(),
                args,
            }]),
            StopReason::ToolUse,
        );
        assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
        assert!(s.record_tool_result(ToolMsg {
            call_id: id.into(),
            result: result(),
        }));
        if name == "edit" || name == "write" {
            s.edits += 1;
        }
    }

    fn declare(s: &mut LoopState) -> ClaimOutcome {
        s.step_claim(assistant(vec![]), StopReason::Stop)
    }

    #[test]
    fn verify_nudge_fires_on_done_with_unverified_edits() {
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        assert_eq!(s.edits, 1);
        let outcome = declare(&mut s);
        assert!(matches!(outcome, ClaimOutcome::VerifyHold));
        assert_eq!(s.verify_hold.as_deref(), Some(VERIFY_NUDGE));
        assert!(s
            .verify_hold
            .unwrap()
            .starts_with("Unverified declare held:"));
        assert_eq!(s.verify_nudges_used, 1);
        assert_eq!(s.edits_at_last_nudge, 1);
        assert!(s.call_model); // turn held alive for the next request
        assert!(s.pending_directives.is_empty()); // request tail only: no double delivery
    }

    #[test]
    fn verify_nudge_needs_edits_before_first_fire() {
        let mut s = LoopState::new();
        assert!(matches!(declare(&mut s), ClaimOutcome::Done));
        assert_eq!(s.verify_nudges_used, 0);
        assert!(s.verify_hold.is_none());
    }

    #[test]
    fn verify_nudge_silent_when_test_passed_since_write() {
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        ok_round(&mut s, "t1", "test", Value::Null);
        assert!(matches!(declare(&mut s), ClaimOutcome::Done));
        assert_eq!(s.verify_nudges_used, 0);
        assert!(s.verify_hold.is_none());
    }

    #[test]
    fn verify_nudge_exec_pytest_counts_but_plain_exec_does_not() {
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        ok_round(&mut s, "x1", "exec", serde_json::json!({"cmd": "ls"}));
        assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        ok_round(
            &mut s,
            "x1",
            "exec",
            serde_json::json!({"cmd": "pytest -q"}),
        );
        assert!(matches!(declare(&mut s), ClaimOutcome::Done));
    }

    #[test]
    fn verify_nudge_failed_test_is_not_passing() {
        let mut s = LoopState::new();
        let outcome = s.step_claim(
            assistant(vec![ToolCallRef {
                id: "e1".into(),
                name: "edit".into(),
                args: Value::Null,
            }]),
            StopReason::ToolUse,
        );
        assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
        assert!(s.record_tool_result(ToolMsg {
            call_id: "e1".into(),
            result: result(),
        }));
        s.edits += 1;
        let outcome = s.step_claim(
            assistant(vec![ToolCallRef {
                id: "t1".into(),
                name: "test".into(),
                args: Value::Null,
            }]),
            StopReason::ToolUse,
        );
        assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
        assert!(s.record_tool_result(ToolMsg {
            call_id: "t1".into(),
            result: ToolResult {
                content: "1 failed".into(),
                is_error: true,
            },
        }));
        assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
    }

    #[test]
    fn verify_nudge_failed_test_outcome_does_not_verify() {
        // FIX 1 shape: the tools lane reports a failing `test` run as
        // `ToolOutcome { success: false, .. }` with FAIL content; run()
        // maps it to an error ToolResult, which must not verify the write.
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        let outcome = s.step_claim(
            assistant(vec![ToolCallRef {
                id: "t1".into(),
                name: "test".into(),
                args: serde_json::json!({"cmd": "pytest -q"}),
            }]),
            StopReason::ToolUse,
        );
        assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
        let failing = outcome_to_result(ToolOutcome {
            content: "FAIL: pytest -q\n1 failed".into(),
            truncated: false,
            success: false,
        });
        assert!(failing.is_error);
        assert!(s.record_tool_result(ToolMsg {
            call_id: "t1".into(),
            result: failing,
        }));
        assert!(!s.verified_since_write);
        assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
    }

    #[test]
    fn is_verification_call_command_table() {
        // Only the command position counts: naming pytest as an argument
        // (`pip install pytest`, `grep pytest`) is not a verification run.
        let cases = [
            ("test", None, true),
            ("exec", Some("pytest"), true),
            ("exec", Some("pytest -q"), true),
            ("exec", Some(".venv/bin/pytest -q"), true),
            ("exec", Some("cargo test"), true),
            ("exec", Some("go test ./..."), true),
            ("exec", Some("npm test"), true),
            ("exec", Some("npm run test"), true),
            ("exec", Some("pip install pytest"), false),
            ("exec", Some("grep pytest"), false),
            ("exec", Some("ls"), false),
            ("exec", None, false),
            ("edit", Some("pytest"), false),
        ];
        for (name, cmd, expected) in cases {
            let args = match cmd {
                Some(c) => serde_json::json!({"cmd": c}),
                None => Value::Null,
            };
            assert_eq!(
                is_verification_call(name, &args),
                expected,
                "name={name} cmd={cmd:?}"
            );
        }
    }

    #[test]
    fn verify_nudge_silent_on_second_declare_without_write() {
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
        // "Blocked" answer: text again, no intervening write -> Done.
        assert!(matches!(declare(&mut s), ClaimOutcome::Done));
        assert_eq!(s.verify_nudges_used, 1);
    }

    #[test]
    fn verify_nudge_silent_in_base_and_contract() {
        for level in [IncentivesLevel::Base, IncentivesLevel::Contract] {
            let mut s = LoopState::new();
            s.incentives = level;
            ok_round(&mut s, "e1", "edit", Value::Null);
            assert!(matches!(declare(&mut s), ClaimOutcome::Done));
            assert_eq!(s.verify_nudges_used, 0);
            assert!(s.verify_hold.is_none());
        }
    }

    #[test]
    fn verify_nudge_silent_inside_cap_tail_tenth() {
        let mut s = LoopState::new();
        let cap = s.budget.config().actions_per_trial;
        assert!(cap >= 10, "tail math needs a non-trivial cap");
        // Just below the last 10%: still fires.
        ok_round(&mut s, "e1", "edit", Value::Null);
        s.budget.counters_mut().actions_this_trial = cap * 9 / 10 - 1;
        assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
        // Inside the last 10%: silent.
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        s.budget.counters_mut().actions_this_trial = cap * 9 / 10;
        assert!(matches!(declare(&mut s), ClaimOutcome::Done));
        assert!(s.verify_hold.is_none());
    }

    #[test]
    fn verify_nudge_caps_at_two_per_run() {
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
        ok_round(&mut s, "e2", "edit", Value::Null); // intervening write
        assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
        assert_eq!(s.verify_nudges_used, 2);
        ok_round(&mut s, "e3", "edit", Value::Null); // budget spent
        assert!(matches!(declare(&mut s), ClaimOutcome::Done));
        assert_eq!(s.verify_nudges_used, 2);
    }

    #[test]
    fn verify_hold_rides_next_request_tail_once_and_leaves_log() {
        let root = run_tmp("verify-hold");
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        state.phase = Phase::Running;
        state.apply_input(Input::User("build it".into()));
        state.admit_steering();
        ok_round(&mut state, "e1", "edit", Value::Null);
        assert!(matches!(declare(&mut state), ClaimOutcome::VerifyHold));
        // Durable hold record for replay: the declare's Attempt row.
        let rows = state.items.len();
        assert!(state.items.iter().any(|i| matches!(
            &i.kind,
            ItemKind::Attempt { error, will_retry: true } if error == VERIFY_NUDGE
        )));
        let cfg = RunConfig::default();
        let r1 = build_request(&mut state, &registry, &root, &cfg);
        // Attribution: the hold is its own user-role row, never merged into
        // the model's assistant declare text. Budgets stay on the prior tail.
        assert!(r1.messages.len() >= 3);
        let last = r1.messages.last().unwrap();
        let prev = &r1.messages[r1.messages.len() - 2];
        assert_eq!(last.role, "user");
        assert_eq!(last.content, VERIFY_NUDGE);
        assert!(
            prev.content.contains("budgets remaining:"),
            "{}",
            prev.content
        );
        assert!(!prev.content.contains(VERIFY_NUDGE));
        // Peek, not consume: a provider Err+retry must re-arm.
        assert!(state.verify_hold.is_some());
        assert_eq!(state.items.len(), rows); // request rows never persisted
                                             // Simulate `run`'s take-on-successful-send.
        state.verify_hold.take();
        let r2 = build_request(&mut state, &registry, &root, &cfg);
        assert!(r2
            .messages
            .iter()
            .all(|m| !m.content.contains(VERIFY_NUDGE)));
        let _ = std::fs::remove_dir_all(&root);
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
                success: true,
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
            retry_usage: None,
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
            retry_usage: None,
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
                success: true,
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
                success: true,
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
                // Held-declare round trip: the first done is held (unverified
                // writes), the second lands Done, the third closes turn 2.
                text_resp("third done"),
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
        // Budget consumed: one step per model call, tokens + spend metered
        // (five calls: the held declare adds one round trip).
        assert_eq!(state.budget.counters().steps, 5);
        assert_eq!(state.budget.counters().tokens, 75);
        assert_eq!(state.budget.counters().spent_cents, 5);
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
        assert_eq!(kinds.iter().filter(|k| **k == "MessageStart").count(), 5);
        assert_eq!(kinds.iter().filter(|k| **k == "MessageEnd").count(), 5);
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
                "Attempt",
                "Assistant",
                "TurnEnd",
                "TurnStart",
                "Input",
                "Assistant",
                "TurnEnd",
            ]
        );
        // Durable hold record: the unverified declare's Attempt carries the
        // nudge text for replay; the model-visible copy rode the next
        // request as its own user-role row.
        assert!(file_items.iter().any(|i| matches!(
            &i.kind,
            ItemKind::Attempt { error, will_retry: true } if error.contains("Unverified declare held")
        )));
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
                // Held-declare round trip: the first done is held (unverified
                // write), the second lands Done.
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
        assert_eq!(ends.len(), 3, "one MessageEnd per settle");
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
        assert_eq!(ends[2].reasoning_tokens, None); // post-hold declare
                                                    // TurnEnd totals sum all three settles (10 each in, 5 each out).
        let totals = history
            .iter()
            .find_map(|e| match e {
                AgentEvent::TurnEnd { usage_totals, .. } => Some(usage_totals),
                _ => None,
            })
            .expect("turn closes with totals");
        assert_eq!((totals.input_tokens, totals.output_tokens), (30, 15));
        assert_eq!(totals.reasoning_tokens, Some(3)); // reported-only sum, not erased by None
        assert_eq!(totals.cost_usd, Some(0.03));
        assert_eq!(state.usage_totals.input_tokens, 30);
        assert_eq!(state.budget.counters().tokens, 45);
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
                // Held-declare round trip: the first done is held (unverified
                // writes), the second lands Done.
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
        // The failed batch's edit refunded past the rollback, then the
        // retry re-applied: exactly one live edit remains.
        assert_eq!(state.edits, 1);
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
                // Held-declare round trip: the first done is held (unverified
                // write), the second lands Done.
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
        // Step-head, post-batch, step-head interleave with the three model
        // calls (the held declare adds one round trip, no batch hooks).
        assert_eq!(
            *order.lock().unwrap(),
            vec!["bets", "model", "bets", "bets", "model", "bets", "model"],
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
        // Held-declare round trip: the first done is held (unverified
        // writes), the second lands Done.
        queue.push_back(text_resp("done again"));
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
        assert_eq!(reqs.len(), 9); // 7 tool rounds + held declare + final text
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

    // --- compaction checkpoint (default off) ---

    use agent_budget::BudgetConfig;

    fn compaction(frac: f64, keep_tokens: usize) -> context::CompactionConfig {
        context::CompactionConfig {
            enabled: true,
            frac,
            keep_tokens,
        }
    }

    /// A scripted summary response: `input` is what the totals read.
    fn summary_resp(text: &str, stop: StopReason, input: u64) -> Response {
        let mut r = text_resp(text);
        r.stop = stop;
        r.usage.input = input;
        r.usage.cost_usd = Some(0.02);
        r
    }

    /// One scripted run; hands back everything the checkpoint assertions need.
    /// `followups` open later turns (a checkpoint is per turn).
    async fn run_script(
        name: &str,
        queue: Vec<Response>,
        cfg: RunConfig,
        budget_tokens: u64,
        followups: Vec<&str>,
        goal: &str,
    ) -> (Outcome, Arc<Mutex<Vec<Request>>>, LoopState, Emitter) {
        let root = run_tmp(name);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let client = ScriptClient {
            order: Arc::new(Mutex::new(Vec::new())),
            requests: requests.clone(),
            queue: Mutex::new(VecDeque::from(queue)),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        state.budget = BudgetGuard::new(
            BudgetConfig {
                max_tokens: budget_tokens,
                ..config_for(Capability::UnattendedBatch)
            },
            Instant::now(),
        );
        for f in followups {
            state.followups.push_back(f.into());
        }
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
                cfg,
            },
            vec![Input::User(goal.into())],
            &cancel,
        )
        .await;
        let _ = std::fs::remove_dir_all(&root);
        (outcome, requests, state, emitter)
    }

    fn last_totals(emitter: &Emitter) -> UsageReport {
        emitter
            .history()
            .iter()
            .rev()
            .find_map(|e| match e {
                AgentEvent::TurnEnd { usage_totals, .. } => Some(usage_totals.clone()),
                _ => None,
            })
            .expect("a TurnEnd frame")
    }

    /// One write round billed `input` prompt tokens: the estimate (anchor +
    /// tail chars/4) crosses `budget_tokens * frac` at the next step head.
    fn write_round(
        calls: Vec<(&str, &str, serde_json::Value)>,
        content: &str,
        input: u64,
    ) -> Response {
        let mut r = script_resp(calls, StopReason::ToolUse);
        r.message.content = content.into();
        r.usage.input = input;
        r
    }

    #[tokio::test]
    async fn run_checkpoint_fires_once_keeps_the_tail_and_meters_the_summary() {
        use agent_event::check_pairing;
        // Round 1 is below the trigger (100 + tail), round 2 crosses it (1000 +
        // tail > 2000 * 0.3): the checkpoint summarizes round 1 and keeps
        // round 2 verbatim.
        let queue = vec![
            write_round(
                vec![(
                    "c1",
                    "write",
                    serde_json::json!({"path": "f1.txt", "content": "x\n"}),
                )],
                "write-f1",
                100,
            ),
            write_round(
                vec![(
                    "c2",
                    "write",
                    serde_json::json!({"path": "f2.txt", "content": "y\n"}),
                )],
                "write-f2",
                1_000,
            ),
            summary_resp("## Goal\nfinish the task", StopReason::Stop, 100),
            text_resp("all done"),
            // Held-declare round trip: the first done is held (unverified
            // writes), the second lands Done.
            text_resp("done again"),
        ];
        let cfg = RunConfig {
            compaction: compaction(0.3, 15),
            ..RunConfig::default()
        };
        let (outcome, requests, state, emitter) =
            run_script("compact-on", queue, cfg, 2_000, Vec::new(), "goal-1").await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        let reqs = requests.lock().unwrap();
        assert_eq!(
            reqs.len(),
            5,
            "two agent calls + summary + held + finishing call"
        );
        // The summary call happens exactly once: no tools, summarizer system
        // row, the older round inside the payload, the kept tail outside it.
        let summary_calls: Vec<&Request> = reqs.iter().filter(|r| r.tools.is_empty()).collect();
        assert_eq!(summary_calls.len(), 1);
        let sreq = summary_calls[0];
        assert_eq!(sreq.messages[0].content, context::SUMMARY_SYSTEM);
        let payload = &sreq.messages[1].content;
        assert!(
            payload.starts_with("<conversation>\n[user]: goal-1"),
            "{payload}"
        );
        assert!(payload.contains("write(") && payload.contains("f1.txt"));
        assert!(payload.contains("[tool]: wrote f1.txt"));
        assert!(
            !payload.contains("wrote f2.txt"),
            "kept tail is not summarized"
        );
        assert!(payload.ends_with(context::SUMMARY_PROMPT));
        // Post-checkpoint request: one summary row replaces the old prefix,
        // the newest assistant call + its result ride verbatim.
        let post = &reqs[3];
        assert!(post.messages[1].content.starts_with(CHECKPOINT_PREFIX));
        assert!(post.messages[1]
            .content
            .contains("## Goal\nfinish the task"));
        let wire = serde_json::to_string(post).unwrap();
        assert!(
            !wire.contains("goal-1"),
            "the summarized prefix was dropped"
        );
        assert!(!wire.contains("wrote f1.txt"), "replaced, not duplicated");
        assert_eq!(post.messages[2].role, "assistant");
        assert_eq!(post.messages[2].tool_calls[0].id, "c2");
        assert_eq!(post.messages[3].tool_call_id.as_deref(), Some("c2"));
        assert!(post.messages[3].content.starts_with("wrote f2.txt"));
        // Real spend, priced like any other request: run totals (and so every
        // TurnEnd frame) carry the summary call.
        let totals = last_totals(&emitter);
        assert_eq!(totals.input_tokens, 100 + 1_000 + 100 + 10 + 10);
        assert_eq!(totals.cost_usd, Some(0.01 + 0.01 + 0.02 + 0.01 + 0.01));
        assert!(state.checkpoint.is_some());
        assert_eq!(state.compacted_turn, Some(1));
        assert!(check_pairing(emitter.history()));
    }

    #[tokio::test]
    async fn run_checkpoint_cut_never_splits_a_call_from_its_results() {
        // Two results in one batch, each 3 estimated tokens: with keep_tokens
        // 3 the crossing lands ON the newest tool result, so the cut has to
        // back up to the assistant call that produced the group.
        let queue = vec![
            write_round(
                vec![
                    (
                        "c1",
                        "write",
                        serde_json::json!({"path": "f1.txt", "content": "x\n"}),
                    ),
                    (
                        "c2",
                        "write",
                        serde_json::json!({"path": "f2.txt", "content": "y\n"}),
                    ),
                ],
                "batch",
                1_000,
            ),
            summary_resp("## Goal\nfinish the task", StopReason::Stop, 100),
            text_resp("all done"),
            // Held-declare round trip: the first done is held (unverified
            // writes), the second lands Done.
            text_resp("done again"),
        ];
        let cfg = RunConfig {
            compaction: compaction(0.5, 3),
            ..RunConfig::default()
        };
        let (outcome, requests, state, _emitter) =
            run_script("compact-cut", queue, cfg, 1_200, Vec::new(), "goal-1").await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        assert!(state.checkpoint.is_some());
        let reqs = requests.lock().unwrap();
        let post = &reqs[2];
        // [system, summary, assistant(c1,c2), tool c1, tool c2]
        assert_eq!(post.messages[2].role, "assistant");
        assert_eq!(post.messages[2].tool_calls.len(), 2);
        assert_eq!(post.messages[3].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(post.messages[4].tool_call_id.as_deref(), Some("c2"));
        for (i, m) in post.messages.iter().enumerate() {
            if m.role == "tool" {
                let id = m.tool_call_id.as_deref().unwrap();
                assert!(
                    post.messages[..i]
                        .iter()
                        .any(|p| p.tool_calls.iter().any(|c| c.id == id)),
                    "tool result {id} kept without its call"
                );
            }
        }
    }

    #[tokio::test]
    async fn run_checkpoint_refuses_a_length_stopped_summary_and_keeps_the_window() {
        use agent_event::check_pairing;
        let queue = vec![
            write_round(
                vec![(
                    "c1",
                    "write",
                    serde_json::json!({"path": "f1.txt", "content": "x\n"}),
                )],
                "write-f1",
                1_000,
            ),
            summary_resp("## Goal\npartial", StopReason::MaxTokens, 200),
            text_resp("all done"),
            // Held-declare round trip: the first done is held (unverified
            // write), the second lands Done.
            text_resp("done again"),
        ];
        let cfg = RunConfig {
            compaction: compaction(0.2, 15),
            ..RunConfig::default()
        };
        let (outcome, requests, state, emitter) =
            run_script("compact-refuse", queue, cfg, 1_500, Vec::new(), "goal-1").await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        let reqs = requests.lock().unwrap();
        assert_eq!(reqs.len(), 4);
        assert_eq!(
            reqs.iter().filter(|r| r.tools.is_empty()).count(),
            1,
            "a refused summary is not retried every step"
        );
        // Window unchanged: the original prefix, no checkpoint row.
        let wire = serde_json::to_string(&reqs[2]).unwrap();
        assert!(wire.contains("goal-1"));
        assert!(!wire.contains("## Goal\npartial"));
        assert!(state.checkpoint.is_none());
        // Refused, but still billed: the spend is in the totals and the
        // refusal is visible in the event stream.
        assert_eq!(last_totals(&emitter).input_tokens, 1_000 + 200 + 10 + 10);
        assert!(emitter.history().iter().any(|e| matches!(
            e,
            AgentEvent::Error { error } if error.code == "compaction-refused"
        )));
        assert!(check_pairing(emitter.history()));
    }

    #[tokio::test]
    async fn run_checkpoint_never_twice_without_a_new_turn() {
        // Round 1 triggers the checkpoint. Round 2's settle re-anchors the
        // estimate above the threshold again, still inside turn 1: the latch
        // is the only thing stopping a second summary call.
        let queue = vec![
            write_round(
                vec![(
                    "c1",
                    "write",
                    serde_json::json!({"path": "f1.txt", "content": "x\n"}),
                )],
                "write-f1",
                1_000,
            ),
            summary_resp("## Goal\nfinish the task", StopReason::Stop, 100),
            write_round(
                vec![(
                    "c2",
                    "write",
                    serde_json::json!({"path": "f2.txt", "content": "y\n"}),
                )],
                "write-f2",
                1_000,
            ),
            text_resp("all done"),
            // Held-declare round trip: the first done is held (unverified
            // writes), the second lands Done.
            text_resp("done again"),
        ];
        let cfg = RunConfig {
            compaction: compaction(0.2, 15),
            ..RunConfig::default()
        };
        let (outcome, requests, state, _emitter) =
            run_script("compact-latch", queue, cfg, 3_000, Vec::new(), "goal-1").await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        let reqs = requests.lock().unwrap();
        assert_eq!(
            reqs.len(),
            5,
            "two agent calls + summary + held + finishing call"
        );
        assert_eq!(
            reqs.iter().filter(|r| r.tools.is_empty()).count(),
            1,
            "one checkpoint per turn"
        );
        assert_eq!(state.compacted_turn, Some(1));
        let post = &reqs[3];
        assert!(post.messages[1].content.starts_with(CHECKPOINT_PREFIX));
        assert_eq!(
            serde_json::to_string(post)
                .unwrap()
                .matches(CHECKPOINT_PREFIX)
                .count(),
            1,
            "one checkpoint row, not a chain"
        );
    }

    #[tokio::test]
    async fn run_checkpoint_composes_on_a_new_turn() {
        use agent_event::check_pairing;
        // Turn 1 checkpoints rounds 1-2 (keep_from 3). The followup opens turn
        // 2, where a second checkpoint absorbs the first summary plus round 3:
        // `keep_from` must map the folded cut back onto raw history.
        let mut turn_one_done = text_resp("turn one done");
        turn_one_done.usage.input = 1_000; // arms turn 2's estimate
                                           // Held-declare round trip: turn one done is held (unverified
                                           // writes); the repeat lands Done. input 1000 re-arms turn 2's
                                           // checkpoint estimate exactly like the held response did; the short
                                           // text keeps the keep_tokens cut on the same row.
        let mut turn_one_again = text_resp("done again");
        turn_one_again.usage.input = 1_000;
        let queue = vec![
            write_round(
                vec![(
                    "c1",
                    "write",
                    serde_json::json!({"path": "f1.txt", "content": "x\n"}),
                )],
                "write-f1",
                100,
            ),
            write_round(
                vec![(
                    "c2",
                    "write",
                    serde_json::json!({"path": "f2.txt", "content": "y\n"}),
                )],
                "write-f2",
                1_000,
            ),
            summary_resp("## Goal\ncheckpoint one", StopReason::Stop, 100),
            write_round(
                vec![(
                    "c3",
                    "write",
                    serde_json::json!({"path": "f3.txt", "content": "z\n"}),
                )],
                "write-f3",
                1_000,
            ),
            turn_one_done,
            turn_one_again,
            summary_resp("## Goal\ncheckpoint two", StopReason::Stop, 100),
            text_resp("turn two done"),
        ];
        let cfg = RunConfig {
            compaction: compaction(0.2, 15),
            ..RunConfig::default()
        };
        let (outcome, requests, state, emitter) = run_script(
            "compact-turn2",
            queue,
            cfg,
            5_000,
            vec!["keep going"],
            "goal-1",
        )
        .await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        let reqs = requests.lock().unwrap();
        assert_eq!(
            reqs.len(),
            8,
            "two checkpoints + five agent calls + one finish"
        );
        assert_eq!(reqs.iter().filter(|r| r.tools.is_empty()).count(), 2);
        // The second summary is an update: the first summary is inside its
        // payload, not lost.
        let payload2 = &reqs[6].messages[1].content;
        assert!(payload2.contains("checkpoint one"), "{payload2}");
        assert!(payload2.contains("[tool]: wrote f2.txt"));
        // Final request: [system, summary2, round 3, turn-1 finishes, followup].
        let post = &reqs[7];
        assert_eq!(post.messages.len(), 7);
        assert!(post.messages[1].content.contains("checkpoint two"));
        assert_eq!(post.messages[2].role, "assistant");
        assert_eq!(post.messages[3].tool_call_id.as_deref(), Some("c3"));
        assert_eq!(post.messages[4].content, "turn one done");
        assert_eq!(post.messages[5].content, "done again");
        assert!(post.messages[6].content.starts_with("keep going"));
        let wire = serde_json::to_string(post).unwrap();
        assert!(!wire.contains("goal-1"));
        assert!(!wire.contains("wrote f1.txt") && !wire.contains("wrote f2.txt"));
        assert!(wire.contains("wrote f3.txt"), "round 3 stayed verbatim");
        assert!(
            !wire.contains("checkpoint one"),
            "absorbed by checkpoint two"
        );
        assert_eq!(state.checkpoint.as_ref().unwrap().keep_from, 5);
        assert_eq!(state.compacted_turn, Some(2));
        assert!(check_pairing(emitter.history()));
    }

    #[tokio::test]
    async fn run_compaction_off_is_byte_identical_to_collapse_5() {
        use agent_event::check_pairing;
        // Trigger-ready: budget 8000, frac 0.1 (threshold 800), keep_tokens 3
        // and every round billed 1000 prompt tokens — every knob but `enabled`
        // is set to fire. Default `enabled: false` is the only thing holding.
        let mut queue: VecDeque<Response> = VecDeque::new();
        for i in 1..=7 {
            let id = format!("c{i}");
            let path = format!("f{i}.txt");
            queue.push_back(write_round(
                vec![(
                    id.as_str(),
                    "write",
                    serde_json::json!({"path": path, "content": "x\n"}),
                )],
                &format!("script-step-{i}"),
                1_000,
            ));
        }
        queue.push_back(text_resp("all done"));
        // Held-declare round trip: the first done is held (unverified
        // writes), the second lands Done.
        queue.push_back(text_resp("done again"));
        let cfg = RunConfig {
            compaction: context::CompactionConfig {
                frac: 0.1,
                keep_tokens: 3,
                ..context::CompactionConfig::default()
            },
            ..RunConfig::default()
        };
        let (outcome, requests, state, emitter) = run_script(
            "compact-off",
            queue.into(),
            cfg,
            8_000,
            Vec::new(),
            "goal-1",
        )
        .await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        let reqs = requests.lock().unwrap();
        assert_eq!(reqs.len(), 9, "no summary call on the off path");
        assert!(reqs.iter().all(|r| !r.tools.is_empty()));
        assert!(state.checkpoint.is_none() && state.compacted_turn.is_none());
        assert!(state.anchor.is_some(), "the trigger was armed and held");
        // Collapse-5 bytes, exactly as the disabled path always produced them.
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
        assert!(!serde_json::to_string(&reqs[7])
            .unwrap()
            .contains(CHECKPOINT_PREFIX));
        assert!(check_pairing(emitter.history()));
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
                // Held-declare round trip: the first done is held (unverified
                // write), the second lands Done.
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
        assert_eq!(reqs.len(), 4);
        // File map frozen at run start: b.rs was written mid-run and never
        // shows up; the head bytes match across all requests.
        let sys = &reqs[0].messages[0].content;
        assert!(sys.contains("a.rs") && !sys.contains("b.rs"), "{sys}");
        assert_eq!(sys, &reqs[1].messages[0].content);
        assert_eq!(sys, &reqs[2].messages[0].content);
        assert_eq!(sys, &reqs[3].messages[0].content); // Identical prefix up to each request's final message, the only one
                                                       // the budget line touches.
        for pair in reqs.windows(2) {
            let head = &pair[0].messages[..pair[0].messages.len() - 1];
            assert!(pair[1].messages.len() > head.len(), "request lost history");
            for (m1, m2) in head.iter().zip(&pair[1].messages) {
                assert_eq!(format!("{m1:?}"), format!("{m2:?}"), "prefix rotated");
            }
        }
        // Fresh counters on the tail of every request: one step per call, one
        // action per executed tool, 15 tokens metered per settle. The fourth
        // request carries the held nudge as its own user-role row past the
        // budgets (never merged into the assistant declare text).
        for (req, tail) in reqs.iter().take(3).zip([
            "budgets remaining: steps 19/20; actions 30/30; tokens 50000/50000",
            "budgets remaining: steps 18/20; actions 29/30; tokens 49985/50000",
            "budgets remaining: steps 17/20; actions 28/30; tokens 49970/50000",
        ]) {
            let last = &req.messages.last().unwrap().content;
            assert!(last.contains(tail), "{last:?} must carry {tail:?}");
            assert!(
                last.ends_with(tail),
                "{last:?} must end with the budget tail"
            );
            assert!(!last.contains(VERIFY_NUDGE));
        }
        let hold_req = &reqs[3];
        let tail = "budgets remaining: steps 16/20; actions 28/30; tokens 49955/50000";
        let last = hold_req.messages.last().unwrap();
        let prev = &hold_req.messages[hold_req.messages.len() - 2];
        assert_eq!(last.role, "user");
        assert_eq!(last.content, VERIFY_NUDGE);
        assert!(
            prev.content.contains(tail),
            "{} must carry {tail:?}",
            prev.content
        );
        assert!(prev.content.ends_with(tail));
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
            queue: Mutex::new(VecDeque::from([
                first,
                text_resp("done"),
                // Held-declare round trip: the first done is held (unverified
                // write), the second lands Done.
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
        assert_eq!(reqs.len(), 3);
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

    #[test]
    fn incentives_levels_gate_contract_and_directive_channel() {
        let root = run_tmp("incentives");
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        // Base: no workflow contract in the system string, directives dropped.
        let cfg = RunConfig {
            incentives: IncentivesLevel::Base,
            ..RunConfig::default()
        };
        let r = build_request(&mut state, &registry, &root, &cfg);
        assert!(!r.messages[0].content.contains("WORKFLOW CONTRACT"));
        state.incentives = IncentivesLevel::Base;
        state.push_directive("go".into());
        assert!(state.pending_directives.is_empty(), "Base drops directives");
        // Contract: contract on, directives still off.
        let cfg = RunConfig {
            incentives: IncentivesLevel::Contract,
            ..RunConfig::default()
        };
        let r = build_request(&mut state, &registry, &root, &cfg);
        assert!(r.messages[0].content.contains("WORKFLOW CONTRACT"));
        state.incentives = IncentivesLevel::Contract;
        state.push_directive("go".into());
        assert!(
            state.pending_directives.is_empty(),
            "Contract drops directives"
        );
        // Full (default = current behavior): both live.
        state.incentives = IncentivesLevel::Full;
        state.push_directive("go".into());
        assert_eq!(state.pending_directives.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Bet A gate (mirror of rof's `Gate`): keep the proven leading prefix.
    struct GateBatch;

    impl BetsHook for GateBatch {
        fn on_post_batch(
            &self,
            claim: &bets::Claim,
            hunks: &[(String, bool)],
        ) -> bets::CommitVerdict {
            bets::gate_batch_commit(claim, hunks)
        }
    }

    #[tokio::test]
    async fn incremental_proof_keeps_the_proven_leading_prefix() {
        let root = run_tmp("incremental");
        std::fs::write(root.join("a.rs"), "one\n").unwrap();
        std::fs::write(root.join("b.rs"), "two\n").unwrap();
        // Probe: a b.rs containing "BAD" fails. Prefix 1 (a.rs only) passes,
        // prefix 2 (both) fails -> flags [true, false] -> Partial keeps a.rs.
        std::fs::write(
            root.join("probe.py"),
            "import pathlib, sys\nsys.exit(1 if 'BAD' in pathlib.Path('b.rs').read_text() else 0)\n",
        )
        .unwrap();
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
                            serde_json::json!({"path": "b.rs", "content": "BAD\n"}),
                        ),
                    ],
                    StopReason::ToolUse,
                ),
                text_resp("done"),
                // Held-declare round trip: the first done is held (unverified
                // writes), the second lands Done.
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
                bets: &GateBatch,
                cfg: RunConfig {
                    proof_cmd: Some("python3 probe.py".into()),
                    ..RunConfig::default()
                },
            },
            vec![Input::User("go".into())],
            &cancel,
        )
        .await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        assert_eq!(
            std::fs::read_to_string(root.join("a.rs")).unwrap(),
            "A2\n",
            "proven hunk stays"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("b.rs")).unwrap(),
            "two\n",
            "refuted hunk reverts"
        );
        assert_eq!(state.ablation.proven_hunks, 1);
        assert_eq!(state.ablation.rollbacks, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn empty_hunk_batches_never_inflate_proven_hunks() {
        let root = run_tmp("nohunks");
        std::fs::write(root.join("a.rs"), "one\n").unwrap();
        let client = ScriptClient {
            order: Arc::new(Mutex::new(Vec::new())),
            requests: Default::default(),
            queue: Mutex::new(VecDeque::from([
                script_resp(
                    vec![("c1", "view", serde_json::json!({"path": "a.rs"}))],
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
                bets: &GateBatch,
                cfg: RunConfig::default(),
            },
            vec![Input::User("go".into())],
            &cancel,
        )
        .await;
        assert!(matches!(outcome, Outcome::Done), "got {outcome:?}");
        assert_eq!(
            state.ablation.proven_hunks, 0,
            "read-only batch: nothing proven, nothing counted"
        );
        assert_eq!(state.ablation.rollbacks, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

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
                // Held-declare round trip: the first done is held (unverified
                // writes), the second lands Done.
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
                // Held-declare round trip: the first done is held (unverified
                // write), the second lands Done.
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
                // Held-declare round trip: the first done is held (unverified
                // write), the second lands Done.
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
        // Shared `settle_tool_msg` already counted the write (`edits += 1`)
        // like `run`'s `note_tool_execution`: no manual increment here.
        assert!(s.start_provider_call(&root).is_some());
        ptx.send(ProviderMsg::Settled {
            turn: 1,
            message: text_resp("done").message,
            stop: StopReason::Stop,
            usage: None,
        })
        .await
        .unwrap();
        // Unverified declare held: the turn stays alive, no Done yet.
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
                "Attempt",
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

    /// `drive_tick` VerifyHold parity: an unverified declare holds the turn
    /// alive through the shared `step_claim` path, exactly like `run`'s
    /// `VerifyHold => continue` (no `ToolStart`, `call_model` latched,
    /// `verify_hold` set for the next request tail).
    #[tokio::test]
    async fn drive_tick_verify_hold_matches_run() {
        let mut s = LoopState::new();
        s.stop_when_idle = true;
        let mut emitter = Emitter::new();
        let (_itx, mut irx) = mpsc::channel(8);
        let (ptx, mut prx) = mpsc::channel(8);
        let (ttx, mut trx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        s.apply_input(Input::User("go".into()));
        s.admit_steering();
        let root = CancellationToken::new();
        assert!(s.start_provider_call(&root).is_some());
        // One successful write via the shared harness path: `edits` and the
        // `observe_action` tripwire increment exactly like `run`.
        ptx.send(ProviderMsg::Settled {
            turn: 1,
            message: assistant(vec![ToolCallRef {
                id: "e1".into(),
                name: "write".into(),
                args: serde_json::json!({"path": "w.txt"}),
            }]),
            stop: StopReason::ToolUse,
            usage: None,
        })
        .await
        .unwrap();
        let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(v, PhaseVerdict::Continue));
        ttx.send(ToolMsg {
            call_id: "e1".into(),
            result: ToolResult {
                content: "wrote w.txt".into(),
                is_error: false,
            },
        })
        .await
        .unwrap();
        let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(v, PhaseVerdict::Continue));
        assert_eq!(s.edits, 1);
        assert_eq!(s.budget.counters().actions_this_trial, 1);
        assert!(!s.verified_since_write);
        // Unverified declare: held, not Done. No `ToolStart` for the empty
        // declare, `call_model` keeps the turn alive for the next request.
        assert!(s.start_provider_call(&root).is_some());
        let frames_before = emitter.history().len();
        ptx.send(ProviderMsg::Settled {
            turn: 1,
            message: assistant(vec![]),
            stop: StopReason::Stop,
            usage: None,
        })
        .await
        .unwrap();
        let v = drive_tick(&mut s, &mut irx, &mut prx, &mut trx, &cancel, &mut emitter).await;
        assert!(matches!(v, PhaseVerdict::Continue));
        assert!(s.call_model);
        assert_eq!(s.verify_hold.as_deref(), Some(VERIFY_NUDGE));
        assert_eq!(s.verify_nudges_used, 1);
        assert!(emitter.history()[frames_before..]
            .iter()
            .all(|e| !matches!(e, AgentEvent::ToolStart { .. })));
    }

    /// Rollback refunds batch counters: `edits`, `actions_this_trial` (+ the
    /// tripwire streak/sig describing those actions), and
    /// `verified_since_write` return to the pre-batch snapshot on a full
    /// rollback, so rolled-back edits leave no stale counters behind.
    #[test]
    fn rollback_refunds_edits_actions_and_verified() {
        let mut s = LoopState::new();
        // Verified baseline: one write covered by a passing test.
        ok_round(&mut s, "w0", "write", serde_json::json!({"path": "a"}));
        ok_round(&mut s, "t0", "test", Value::Null);
        assert!(s.verified_since_write);
        let snap = snapshot_batch(&s);
        let edits_before = s.edits;
        let actions_before = s.budget.counters().actions_this_trial;
        // Claim the batch that the tree will roll back, then apply the
        // shared effects (`run`'s `note_tool_execution` + `record` order).
        let outcome = s.step_claim(
            assistant(vec![ToolCallRef {
                id: "w1".into(),
                name: "write".into(),
                args: serde_json::json!({"path": "b"}),
            }]),
            StopReason::ToolUse,
        );
        assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
        let (wname, wargs) = {
            let c = &s.tool_calls["w1"];
            (c.name.clone(), c.args.clone())
        };
        let w1 = ToolResult {
            content: "wrote b".into(),
            is_error: false,
        };
        let _ = note_tool_execution(&mut s, &wname, &wargs, &w1);
        assert!(s.record_tool_result(ToolMsg {
            call_id: "w1".into(),
            result: w1,
        }));
        // The batch moved the counters and dirtied the verified flag.
        assert_eq!(s.edits, edits_before + 1);
        assert_eq!(s.budget.counters().actions_this_trial, actions_before + 1);
        assert!(!s.verified_since_write);
        // The tree rolled the batch back: refund to the snapshot.
        refund_batch(&mut s, &snap);
        assert_eq!(s.edits, snap.edits);
        assert_eq!(
            s.budget.counters().actions_this_trial,
            snap.actions_this_trial
        );
        assert_eq!(
            s.budget.counters().same_action_streak,
            snap.same_action_streak
        );
        assert_eq!(s.last_sig, snap.last_sig);
        assert_eq!(s.last_obs, snap.last_obs);
        assert_eq!(s.verified_since_write, snap.verified_since_write);
        assert!(s.verified_since_write);
    }

    /// Per-step budget: a fresh step head resets to [`STEP_RETRY_BUDGET`]
    /// (init 2), an in-step retry continuation keeps its remaining budget so
    /// 3 consecutive fails still exhaust. Run-wide would exhaust after any 2
    /// fails across successes; unconditional reset would never exhaust.
    #[test]
    fn step_retries_resets_on_fresh_step_head_not_retry_continuation() {
        let mut s = LoopState::new();
        assert_eq!(s.step_retries, STEP_RETRY_BUDGET);
        assert_eq!(STEP_RETRY_BUDGET, 2);
        s.turn = 1;
        let root = CancellationToken::new();
        assert!(s.start_provider_call(&root).is_some());
        assert_eq!(s.step_retries, 2);
        assert!(s.finish_provider_msg(ProviderMsg::Failed {
            turn: 1,
            err: "e1".into(),
            cancelled: false,
            usage: None,
        }));
        assert_eq!(s.step_retries, 1);
        assert!(s.call_model); // retry latched
                               // Retry continuation keeps the remaining 1 (no reset).
        assert!(s.start_provider_call(&root).is_some());
        assert_eq!(s.step_retries, 1, "retry must not reset");
        // Settle the retry successfully; the next fresh step resets to 2.
        assert!(s.finish_provider_msg(ProviderMsg::Settled {
            turn: 1,
            message: assistant(vec![]),
            stop: StopReason::Stop,
            usage: None,
        }));
        assert_eq!(s.step_retries, 1, "success does not itself reset");
        assert!(s.start_provider_call(&root).is_some());
        assert_eq!(s.step_retries, 2, "fresh step after success resets");
    }

    /// Hanging provider that never answers unless raced: without the
    /// parent-side `select!` the run would wait out the full sleep.
    struct HangingClient;

    #[async_trait::async_trait]
    impl LlmClient for HangingClient {
        async fn complete(&self, _model: &str, _req: &Request) -> Result<Response, LlmError> {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(text_resp("never"))
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

    /// Cancel during `provider.complete` aborts promptly: the step records a
    /// `Failed { cancelled: true }` (budget refunded, no `Attempt` row) and
    /// the run returns `Cancelled` in ~50ms instead of 30s. The per-turn
    /// child token is cancelled when the race loses (stop_hard + aborting
    /// gate propagate it to any tool children).
    #[tokio::test]
    async fn cancel_during_provider_complete_aborts_promptly_as_cancelled() {
        let root = run_tmp("cancel-race");
        let client = HangingClient;
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let mut emitter = Emitter::new();
        let cancel = CancellationToken::new();
        let fired = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            fired.cancel();
        });
        let started = Instant::now();
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
        let elapsed = started.elapsed();
        assert!(
            matches!(outcome, Outcome::Cancelled),
            "cancel must abort as Cancelled, got {outcome:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "must not wait out the 30s provider, took {elapsed:?}"
        );
        // Step-level `Failed { cancelled: true }`: refunded, never retried.
        assert_eq!(
            state.budget.counters().steps,
            0,
            "cancelled refunds the step"
        );
        assert!(
            !state
                .items
                .iter()
                .any(|i| matches!(&i.kind, ItemKind::Attempt { .. })),
            "cancelled records no Attempt row"
        );
        assert!(state.stop_hard);
        assert_eq!(state.gate.status(), GateStatus::Aborting);
        assert!(emitter.history().iter().any(|e| matches!(
            e,
            AgentEvent::Error { error } if error.code == "provider-failed"
                && error.message.contains("cancelled")
        )));
        let _ = std::fs::remove_dir_all(&root);
    }

    // --- PHASE 5 fixes: attribution, headroom, replay, termination, log boundary ---

    /// (1) consumption: `build_request` peeks without consuming; `run` takes
    /// only on successful send, so a provider Err+retry re-arms the hold.
    #[test]
    fn verify_hold_peek_survives_retry_and_takes_on_success() {
        let root = run_tmp("hold-peek");
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        state.phase = Phase::Running;
        state.apply_input(Input::User("go".into()));
        state.admit_steering();
        ok_round(&mut state, "e1", "edit", Value::Null);
        assert!(matches!(declare(&mut state), ClaimOutcome::VerifyHold));
        assert!(state.verify_hold.is_some());
        let cfg = RunConfig::default();
        // Peek: still armed after the build.
        let r1 = build_request(&mut state, &registry, &root, &cfg);
        assert!(r1.messages.iter().any(|m| m.content == VERIFY_NUDGE));
        assert!(state.verify_hold.is_some(), "peek must not consume");
        // Provider Err path in `run` keeps it: rebuild still carries it.
        let r_retry = build_request(&mut state, &registry, &root, &cfg);
        assert!(r_retry.messages.iter().any(|m| m.content == VERIFY_NUDGE));
        // `run`'s take-on-Ok: delivered once, never twice.
        state.verify_hold.take();
        let r2 = build_request(&mut state, &registry, &root, &cfg);
        assert!(r2.messages.iter().all(|m| m.content != VERIFY_NUDGE));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (1) attribution: the hold is its own user-role row, never merged into
    /// the model's assistant declare text.
    #[test]
    fn verify_hold_own_row_not_merged_into_assistant_declare() {
        let root = run_tmp("hold-attr");
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        state.phase = Phase::Running;
        state.apply_input(Input::User("go".into()));
        state.admit_steering();
        ok_round(&mut state, "e1", "edit", Value::Null);
        assert!(matches!(declare(&mut state), ClaimOutcome::VerifyHold));
        let req = build_request(&mut state, &registry, &root, &RunConfig::default());
        // The declare row is assistant; the hold row is a separate user row.
        let assistant_declares: Vec<&ProviderMessage> = req
            .messages
            .iter()
            .filter(|m| m.role == "assistant" && !m.tool_calls.is_empty() == false)
            .collect();
        let _ = assistant_declares;
        let last = req.messages.last().unwrap();
        assert_eq!(last.role, "user");
        assert_eq!(last.content, VERIFY_NUDGE);
        for m in &req.messages[1..req.messages.len() - 1] {
            assert!(!m.content.contains(VERIFY_NUDGE), "merged into {m:?}");
        }
        // The assistant declare text itself never carries the nudge.
        let declares: Vec<&ProviderMessage> = req
            .messages
            .iter()
            .filter(|m| m.role == "assistant")
            .collect();
        assert!(!declares.is_empty());
        for d in declares {
            assert!(!d.content.contains(VERIFY_NUDGE));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (2) reserve: fewer than 2 steps remaining vetoes the hold (test +
    /// re-declare need two calls).
    #[test]
    fn verify_nudge_no_hold_without_two_step_headroom() {
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        let max = s.budget.config().max_steps.get();
        // Two remaining: hold fires.
        s.budget.counters_mut().steps = max - 2;
        assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
        // One remaining: silent Done, no grace hold.
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        s.budget.counters_mut().steps = max - 1;
        assert!(matches!(declare(&mut s), ClaimOutcome::Done));
        assert!(s.verify_hold.is_none());
        // Zero remaining (already at max): silent Done.
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        s.budget.counters_mut().steps = max;
        assert!(matches!(declare(&mut s), ClaimOutcome::Done));
    }

    /// (2) tokens cap vetoes the hold.
    #[test]
    fn verify_nudge_no_hold_when_tokens_exhausted() {
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        s.budget.counters_mut().tokens = s.budget.config().max_tokens;
        assert!(matches!(declare(&mut s), ClaimOutcome::Done));
        assert!(s.verify_hold.is_none());
    }

    /// (2) spend cap vetoes the hold.
    #[test]
    fn verify_nudge_no_hold_when_spend_exhausted() {
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        let limit = s.budget.config().max_spend_cents.unwrap();
        s.budget.counters_mut().spent_cents = limit;
        assert!(matches!(declare(&mut s), ClaimOutcome::Done));
        assert!(s.verify_hold.is_none());
    }

    /// (2) wallclock cap vetoes the hold.
    #[test]
    fn verify_nudge_no_hold_when_wallclock_exhausted() {
        use agent_budget::{config_for, BudgetGuard, Capability};
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        // Elapsed past the wallclock cap: consults the same gate `terminate` halts on.
        s.budget = BudgetGuard::new(
            config_for(Capability::UnattendedBatch),
            Instant::now() - Duration::from_secs(10_000),
        );
        // Re-apply the write (new guard reset the counters): one edit, unverified.
        // `ok_round` already burned edits=1 on the old guard; restore it.
        s.edits = 1;
        assert!(matches!(declare(&mut s), ClaimOutcome::Done));
        assert!(s.verify_hold.is_none());
    }

    /// (2) exhausted budget (any cap, e.g. actions) gets no grace hold:
    /// consults `terminate`'s AND-gate instead of bypassing it via `continue`.
    #[test]
    fn verify_nudge_no_hold_when_budget_exceeded() {
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        s.budget.counters_mut().actions_this_trial = s.budget.config().actions_per_trial;
        assert!(s.budget.exceeded().is_some());
        assert!(matches!(declare(&mut s), ClaimOutcome::Done));
        assert!(s.verify_hold.is_none());
    }

    /// (3) replay: the hold fire persists a durable Attempt record so the log
    /// (and a file replay via `read_log`) reproduces the fire, while the
    /// model-visible copy rides the next request as its own row.
    #[test]
    fn verify_hold_persisted_as_attempt_for_replay() {
        let mut s = LoopState::new();
        ok_round(&mut s, "e1", "edit", Value::Null);
        assert!(matches!(declare(&mut s), ClaimOutcome::VerifyHold));
        let holds: Vec<&str> = s
            .items
            .iter()
            .filter_map(|i| match &i.kind {
                ItemKind::Attempt {
                    error,
                    will_retry: true,
                } if error == VERIFY_NUDGE => Some(error.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(holds.len(), 1);
        // Replay from the items vec reproduces the fire.
        let replayed = s.items.clone();
        assert!(replayed.iter().any(|i| matches!(
            &i.kind,
            ItemKind::Attempt { error, .. } if error == VERIFY_NUDGE
        )));
        // `derived_messages` still folds items only (Attempt is log-only),
        // so the durable row is the replay source, not a folded message.
        assert!(s
            .derived_messages()
            .iter()
            .all(|m| m.content != VERIFY_NUDGE));
    }

    /// (4) `same_action_cycles == 0` disables the tripwire lesson, mirroring
    /// `observe_action`'s `> 0` guard.
    #[test]
    fn terminate_same_action_zero_disables_lesson() {
        use agent_budget::{config_for, BudgetConfig, BudgetGuard, Capability};
        let mut s = LoopState::new();
        let cfg = BudgetConfig {
            same_action_cycles: 0,
            ..config_for(Capability::UnattendedBatch)
        };
        s.budget = BudgetGuard::new(cfg, Instant::now());
        // Streak 0 >= cycles 0 would fire without the guard.
        assert_eq!(s.budget.counters().same_action_streak, 0);
        assert!(matches!(s.terminate(), PhaseVerdict::Continue));
        assert!(s.lessons.is_empty(), "cycles=0 must not lesson");
    }

    /// (5) already-started final step: `record_step` hit max at the head, the
    /// admitted step's Done still lands Done instead of Halted(steps).
    #[tokio::test]
    async fn last_step_done_wins_over_steps_halt() {
        let root = run_tmp("last-done");
        let client = ScriptClient {
            order: Default::default(),
            requests: Default::default(),
            queue: Mutex::new(VecDeque::from([text_resp("finished")])),
        };
        let registry = run_registry(&root);
        let mut state = LoopState::new();
        let max = state.budget.config().max_steps.get();
        state.budget.counters_mut().steps = max - 1;
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
        assert_eq!(state.budget.counters().steps, max);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (7) stop labels are explicit matches, never `Debug`.
    #[test]
    fn stop_label_is_explicit_not_debug() {
        assert_eq!(stop_label(StopReason::Pending), "Pending");
        assert_eq!(stop_label(StopReason::Stop), "Stop");
        assert_eq!(stop_label(StopReason::ToolUse), "ToolUse");
        assert_eq!(stop_label(StopReason::MaxTokens), "MaxTokens");
        assert_eq!(stop_label(StopReason::Refused), "Refused");
        assert_eq!(stop_label(StopReason::Error), "Error");
        assert_eq!(stop_label(StopReason::Aborted), "Aborted");
        assert_eq!(stop_label(StopReason::Deferred), "Deferred");
    }

    /// (7) corrupt assistant rows surface an explicit marker, never silent
    /// defaults or raw JSON.
    #[test]
    fn assistant_corrupt_row_is_explicit_marker_not_raw_json() {
        // Corrupt stored rows (Null and object-without-content): explicit
        // corrupt marker, never silent defaults or raw JSON (`Null` -> "null",
        // `{"bad":1}` -> raw object text).
        for bad in [Value::Null, serde_json::json!({"bad": 1})] {
            let mut s = LoopState::new();
            s.items.push(Item {
                seq: 1,
                id: "x".into(),
                parent_id: None,
                recorded_at: SystemTime::now(),
                kind: ItemKind::Assistant {
                    message: bad,
                    stop_reason: "Stop".into(),
                    interrupted: false,
                },
            });
            let msgs = s.derived_messages();
            assert_eq!(msgs.len(), 1);
            assert!(
                msgs[0].content.starts_with("[corrupt assistant row:"),
                "{}",
                msgs[0].content
            );
            // Fail-closed marker, not silent wrong data: never the bare
            // raw-JSON fallback nor an empty default.
            assert_ne!(msgs[0].content, "null");
            assert!(!msgs[0].content.contains("bad"));
            assert!(!msgs[0].content.is_empty());
        }
    }

    /// (7) `push_assistant` never stores a silent Null.
    #[test]
    fn push_assistant_never_stores_null() {
        let mut s = LoopState::new();
        s.push_assistant(
            &AssistantMessage {
                content: "hi".into(),
                tool_calls: Vec::new(),
                thinking: None,
            },
            "Stop",
        );
        match &s.items[0].kind {
            ItemKind::Assistant {
                message,
                stop_reason,
                ..
            } => {
                assert_ne!(message, &Value::Null);
                assert_eq!(message["content"], "hi");
                assert_eq!(stop_reason, "Stop");
            }
            other => panic!("expected Assistant, got {other:?}"),
        }
    }
}
