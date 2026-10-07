//! Shared turn/step state: `LoopState`, items, and the durable-log helpers.

use crate::{EffectGate, RunConfig, VerifyState};
use agent_budget::{config_for, halt_name, BudgetGuard, Capability};
use agent_event::{TurnEndReason as EventTurnEndReason, UsageReport};
use agent_log::{InputSource, Item, ItemKind, RecoveryCode, TurnEndReason};
use provider_core::{AssistantMessage, StopReason, Usage};
use std::collections::{HashMap, VecDeque};
use std::time::{Instant, SystemTime};
use tokio_util::sync::CancellationToken;
use tool_core::{ToolCall, ToolResult};

/// Assistant rows whose echoed reasoning survives folding into the derived
/// transcript. Default for [`RunConfig::thinking_keep`]; arms set the field
/// directly, so the fold never reads the environment.
pub(crate) const THINKING_KEEP: usize = 2;

/// Per-step provider-failure budget (init + per-step reset value).
/// Nesting: the provider adapter retries INSIDE each `complete` (cold-start
/// 503s up to 8 attempts with 60s sleeps, other retryables 5, plus the
/// truncation ladder), and this budget nests OUTSIDE it — each in-step retry
/// re-runs the full adapter ladder. Provider retry counts are unchanged here.
pub(crate) const STEP_RETRY_BUDGET: u32 = 2;

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
    Failed { kind: FailureKind, message: String },
}

/// Why a run failed: the failure sites in `run` map here so callers can
/// react without parsing message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The append-only log could not open or persist: fail closed, never
    /// run from process-only state.
    Log,
    /// The snapshot tree (ensure/baseline/rollback/restore, hunk probe)
    /// failed: the workdir is not in a known-good state.
    Snapshot,
    /// The provider ladder failed fatally (retries exhausted, refused):
    /// the model made no progress.
    Provider,
    /// The run was asked to do nothing: no input seeds a turn.
    Input,
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
/// driver. Run configuration (incentives, drain deadline, fold knobs) lives
/// in [`RunConfig`], the single source of truth threaded through every
/// method that needs it — no field here mirrors it, so there is no
/// precedence to document.
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
    /// Mid-run verification nudge (Full only); see [`VerifyState`].
    pub verify: VerifyState,
    /// Mutable experiment runtime (incentive-scaffold B→+A→+C); see
    /// [`Experiment`]. The immutable half (incentive level, bets hook)
    /// lives in [`RunConfig`]/`Run`, so the base loop reads cleanly.
    pub experiment: Experiment,
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

/// Mutable experiment runtime (incentive-scaffold B→+A→+C): ablation
/// counters plus the half-cap/near-cap directive one-shot latches. The
/// immutable half of the scaffold (incentive level, bets hook) lives in
/// [`RunConfig`]/`Run`, so the base loop reads cleanly. Grouped so
/// `LoopState`'s surface stays on the base loop; no behavior change.
#[derive(Debug, Clone, Default)]
pub struct Experiment {
    /// Ablation observability (Bet B→+A→+C): verdict/assessment counters
    /// for the run-end report. Never drives behavior.
    pub ablation: bets::AblationMetrics,
    /// One-shot latches: the half-cap and near-cap directives fire once each.
    pub half_directive_sent: bool,
    pub late_directive_sent: bool,
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
            verify: VerifyState::default(),
            experiment: Experiment::default(),
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

    pub(crate) fn open_turn(&mut self) {
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

    /// First terminal reason wins: a later Completed must not downgrade MaxTokens.
    pub fn stick_turn_reason(&mut self, reason: TurnEndReason) {
        if self.turn_reason.is_none() {
            self.turn_reason = Some(reason);
        }
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

    /// Ordered termination, top-down, first match wins. `cfg` is the single
    /// source of run configuration (drain deadline here).
    pub fn terminate(&mut self, cfg: &RunConfig) -> PhaseVerdict {
        // 1. Cancel / StopHard drains to terminal under a bounded deadline.
        if self.stop_hard {
            let until = *self
                .drain_until
                .get_or_insert_with(|| Instant::now() + cfg.drain_timeout);
            if (self.in_flight.is_some() || self.open_tools() > 0) && Instant::now() < until {
                return PhaseVerdict::Break;
            }
            return PhaseVerdict::Return(Outcome::Cancelled);
        }
        // 2. Hard error: in-step retries exhausted.
        if let Some(err) = self.fatal_error.clone() {
            return PhaseVerdict::Return(Outcome::Failed {
                kind: FailureKind::Provider,
                message: err,
            });
        }
        // 3. Budget exhausted: the guard AND-gate owns every cap; the loop
        // holds no shadow caps and halts the moment any counter trips.
        if let Some(exceeded) = self.budget.exceeded() {
            return PhaseVerdict::Return(Outcome::Halted(halt_name(&exceeded.halt).into()));
        }
        // 4. Refusal is terminal-with-error; sticky max-tokens halts once drained.
        if let Some(TurnEndReason::Error(reason)) = self.turn_reason.clone() {
            return PhaseVerdict::Return(Outcome::Failed {
                kind: FailureKind::Provider,
                message: reason,
            });
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
}

impl Default for LoopState {
    fn default() -> Self {
        Self::new()
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

pub(crate) fn outcome_log_reason(
    outcome: &Outcome,
    sticky: Option<&TurnEndReason>,
) -> TurnEndReason {
    if let Some(r) = sticky {
        return r.clone();
    }
    match outcome {
        Outcome::Done => TurnEndReason::Completed,
        Outcome::Halted(s) if s == "max-tokens" => TurnEndReason::MaxTokens,
        Outcome::Halted(_) => TurnEndReason::Budget,
        Outcome::Cancelled => TurnEndReason::Interrupted,
        Outcome::Failed { message, .. } => TurnEndReason::Error(message.clone()),
    }
}

pub(crate) fn turn_id(turn: u64) -> String {
    format!("turn-{turn}")
}

pub(crate) fn append_to(items: &mut Vec<Item>, kind: ItemKind) {
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
/// TEST-ONLY substrate today: prod turns use `open_turn`/`append_to(TurnEnd)`
/// (`state.rs`, `run.rs`); only `src/tests.rs` constructs this. Kept for the
/// unwind-safety property, not wired into `run()`.
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
