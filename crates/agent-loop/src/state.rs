//! Shared turn/step state: `LoopState`, items, and the durable-log helpers.

use crate::{BudgetShape, RunConfig, VerifyState};
use agent_budget::{config_for, halt_name, BudgetGuard, Capability};
use agent_event::{TurnEndReason as EventTurnEndReason, UsageReport};
use agent_log::{InputSource, Item, ItemKind, RecoveryCode, TurnEndReason};
use provider_core::{AssistantMessage, Usage};
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
    /// The operator-supplied completion predicate did not pass or could not run.
    Completion,
}

#[derive(Debug, Clone)]
pub enum Input {
    User(String),
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
pub struct ToolCallState {
    pub name: String,
    /// Args JSON at claim time, for the `observe_action` sig (`"{name}:{args}"`).
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
/// method that needs it; the one mirror is `budget_shape`, seeded by `run`
/// (`cfg` wins) — everything else reads `cfg` directly.
pub struct LoopState {
    pub turn: u64,
    pub step: u32,
    pub phase: Phase,
    pub items: Vec<Item>,
    /// Leading `items` already durable in the log; `run` starts its sync
    /// cursor here and writes it back, so repeated runs on one state (chat)
    /// and a restored history never re-append rows.
    pub persisted: usize,
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
    pub call_model: bool,
    pub(crate) in_flight: bool,
    pub tool_calls: HashMap<String, ToolCallState>,
    pub stop_hard: bool,
    pub turn_reason: Option<TurnEndReason>,
    pub budget: BudgetGuard,
    /// What the token cap counts; seeded from [`RunConfig::budget_shape`] at
    /// run start (direct `LoopState` users keep the gross default).
    pub budget_shape: BudgetShape,
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
    /// Tokens the last settled model request charged to the budget (0 = none
    /// yet, or after a resume); sizes the `--budget-line requests` estimate.
    pub last_request_tokens: u64,
}

/// Mutable experiment runtime (incentive-scaffold B→+A→+C): ablation
/// counters plus the half-cap/near-cap directive one-shot latches. The
/// immutable half of the scaffold (incentive level, bets hook) lives in
/// [`RunConfig`]/`Run`, so the base loop reads cleanly. Grouped so
/// `LoopState`'s surface stays on the base loop; no behavior change.
#[derive(Debug, Clone, Default)]
pub struct Experiment {
    pub(crate) streak_reads: u32,
    pub(crate) streak_crossed: bool,
    pub(crate) streak_pending: bool,
    pub(crate) streak_write_seen: bool,
    /// Ablation observability (Bet B→+A→+C): verdict/assessment counters
    /// for the run-end report. Never drives behavior.
    pub ablation: bets::AblationMetrics,
    /// One-shot latches: the half-cap and near-cap directives fire once each.
    pub half_directive_sent: bool,
    pub late_directive_sent: bool,
    /// The named-deliverable check ran (it fires once per run).
    pub deliverable_checked: bool,
}

impl LoopState {
    pub fn new() -> Self {
        Self {
            turn: 0,
            step: 0,
            phase: Phase::Idle,
            items: Vec::new(),
            persisted: 0,
            file_map: None,
            pins: None,
            steering: VecDeque::new(),
            call_model: false,
            in_flight: false,
            tool_calls: HashMap::new(),
            stop_hard: false,
            turn_reason: None,
            budget: BudgetGuard::new(config_for(Capability::UnattendedBatch), Instant::now()),
            budget_shape: BudgetShape::Gross,
            usage_totals: UsageReport::default(),
            emit_next: 0,
            open_msg: None,
            step_retries: STEP_RETRY_BUDGET,
            fatal_error: None,
            last_sig: String::new(),
            last_obs: 0,
            pending_directives: VecDeque::new(),
            edits: 0,
            verify: VerifyState::default(),
            experiment: Experiment::default(),
            drain_until: None,
            checkpoint: None,
            anchor: None,
            compacted_turn: None,
            last_request_tokens: 0,
        }
    }

    /// Resume: adopt a durable log (already validated by `read_log`) as the
    /// history. Calls with no recorded result get a synthetic error result
    /// and an open last turn is closed `Interrupted`, so the next request is
    /// well-formed. Not restored: budgets, verify/experiment counters,
    /// checkpoint, pins/file map (the next `run` re-freezes those).
    pub fn restore(&mut self, items: Vec<Item>) {
        self.turn = items
            .iter()
            .filter(|i| matches!(i.kind, ItemKind::TurnStart { .. }))
            .count() as u64;
        self.persisted = items.len();
        self.items = items;
        let answered: std::collections::HashSet<String> = self
            .items
            .iter()
            .filter_map(|i| match &i.kind {
                ItemKind::ToolResult { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .collect();
        let dangling: Vec<String> = self
            .items
            .iter()
            .filter_map(|i| match &i.kind {
                ItemKind::Assistant { message, .. } => {
                    serde_json::from_value::<AssistantMessage>(message.clone()).ok()
                }
                _ => None,
            })
            .flat_map(|m| m.tool_calls)
            .map(|c| c.id)
            .filter(|id| !answered.contains(id))
            .collect();
        for call_id in dangling {
            append_to(
                &mut self.items,
                ItemKind::ToolResult {
                    call_id,
                    content: agent_log::UNKNOWN_OUTCOME_TEXT.into(),
                    is_error: true,
                    recovery: Some(RecoveryCode::ToolOutcomeUnknown),
                },
            );
        }
        let open = self.items.iter().rev().find_map(|i| match i.kind {
            ItemKind::TurnStart { .. } => Some(true),
            ItemKind::TurnEnd { .. } => Some(false),
            _ => None,
        });
        if open == Some(true) {
            append_to(
                &mut self.items,
                ItemKind::TurnEnd {
                    turn_id: turn_id(self.turn),
                    reason: TurnEndReason::Interrupted,
                },
            );
        }
    }

    /// [`Self::restore`] from a log file on disk.
    pub fn restore_from(&mut self, path: &std::path::Path) -> std::io::Result<()> {
        self.restore(agent_log::read_log(path)?);
        Ok(())
    }

    /// Chat: clear the per-run latches a finished or failed `run` leaves
    /// behind so the same state takes the next user line. History, usage
    /// totals and counters stay; the caller re-seeds `budget`.
    pub fn reset_for_input(&mut self) {
        self.phase = Phase::Idle;
        self.turn_reason = None;
        self.fatal_error = None;
        self.stop_hard = false;
        self.call_model = false;
        self.drain_until = None;
        self.in_flight = false;
        self.steering.clear();
        self.step_retries = STEP_RETRY_BUDGET;
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
        if self.stop_hard || self.fatal_error.is_some() {
            return false;
        }
        self.call_model || (!self.steering.is_empty() && !self.in_flight && self.open_tools() == 0)
    }

    pub fn is_idle(&self) -> bool {
        !self.in_flight && self.open_tools() == 0 && self.steering.is_empty()
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

    /// Inbox classification by phase: Running steers, Idle opens a turn.
    pub fn apply_input(&mut self, input: Input) {
        let Input::User(text) = input;
        let queued = QueuedInput {
            text,
            source: InputSource::External,
        };
        if self.phase == Phase::Idle {
            self.open_turn();
        }
        self.steering.push_back(queued);
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
        admitted
    }

    /// Single-flight provider admission. Returns the per-turn child token.
    /// Step head: the BudgetGuard AND-gate runs here, once per step, so its
    /// one grace step fires exactly once; never recreate the guard per step.
    pub fn start_provider_call(&mut self, root: &CancellationToken) -> Option<CancellationToken> {
        if self.in_flight || self.stop_hard || self.fatal_error.is_some() {
            return None;
        }
        if self.budget.may_step().is_err() {
            return None;
        }
        let token = root.child_token();
        self.in_flight = true;
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

    /// Settle a completed provider call: clear single-flight, meter billed
    /// usage, and size the next request's `--budget-line requests` estimate.
    pub(crate) fn on_provider_settled(&mut self, usage: Option<&Usage>) {
        self.in_flight = false;
        if let Some(usage) = usage {
            let before = self.budget.counters().tokens;
            self.record_usage(usage);
            self.last_request_tokens = self.budget.counters().tokens - before;
        }
    }

    /// Settle a failed provider call: clear single-flight, meter any billed
    /// usage, refund on cancel, or consume a step retry / latch fatal error.
    pub(crate) fn on_provider_failed(&mut self, err: &str, cancelled: bool, usage: Option<&Usage>) {
        self.in_flight = false;
        // Attempts that returned usage are billed: meter before the
        // refund/retry fork so an exhausted ladder's re-sends are never
        // invisible (pre-generation failures carry no usage to meter).
        if let Some(u) = usage {
            self.record_usage(u);
        }
        if cancelled {
            self.budget.refund_step(); // no progress: bounded inside the guard
            return;
        }
        let retry = self.step_retries > 0;
        if retry {
            self.step_retries -= 1;
        }
        append_to(
            &mut self.items,
            ItemKind::Attempt {
                error: err.to_owned(),
                will_retry: retry,
            },
        );
        if retry {
            self.call_model = true; // in-step retry reuses the same assembly
        } else {
            self.fatal_error = Some(err.to_owned());
        }
    }

    /// Tokens meter every settle and every metered failure; spend converts
    /// cost_usd to cents. What counts toward the cap follows
    /// [`crate::BudgetShape`]; the reported totals below are always gross.
    /// The same reading folds into the live running totals
    /// (`TurnEnd.usage_totals`).
    pub fn record_usage(&mut self, usage: &Usage) {
        self.budget.record_tokens(match self.budget_shape {
            BudgetShape::Gross => usage.total_tokens(),
            // Cache reads are a subset of `input`: saturate so a provider
            // that over-reports them clamps the input share at 0.
            BudgetShape::MissPlusOut => usage.input.saturating_sub(usage.cache_read) + usage.output,
        });
        if let Some(cost) = usage.cost_usd {
            let before = self.usage_totals.cost_usd.unwrap_or(0.0);
            let cents = |v: f64| (v.mul_add(100.0, 1e-9)).floor().max(0.0) as u64;
            self.budget
                .record_spend_cents(cents(before + cost).saturating_sub(cents(before)));
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

    /// Ordered termination, top-down, first match wins. `cfg` is the single
    /// source of run configuration (drain deadline here).
    pub fn terminate(&mut self, cfg: &RunConfig) -> PhaseVerdict {
        // 1. Cancel / StopHard drains to terminal under a bounded deadline.
        if self.stop_hard {
            let until = *self
                .drain_until
                .get_or_insert_with(|| Instant::now() + cfg.drain_timeout);
            if (self.in_flight || self.open_tools() > 0) && Instant::now() < until {
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
            && !self.in_flight
            && self.open_tools() == 0
        {
            return PhaseVerdict::Return(Outcome::Halted("max-tokens".into()));
        }
        // Retry and reflection latches keep the run alive without new decisions.
        if self.call_model {
            return PhaseVerdict::Continue;
        }
        // 5. Semantic termination: drained turn, empty queues.
        if !self.in_flight && self.open_tools() == 0 && self.steering.is_empty() {
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
