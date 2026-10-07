//! Multi-tick drivers: headless `run` and channel `drive_tick`.

use crate::proof::BatchSnapshot;
use crate::{
    append_to, batch_hunks, build_request, checkpoint, incremental_hunks, note_tool_execution,
    outcome_log_reason, outcome_to_result, pin_snapshot, refund_batch, settle_tool_msg,
    settle_tool_tail, snapshot_batch, turn_end_reason_to_event, turn_id, BetsHook, ClaimOutcome,
    IncentivesLevel, Input, LoopState, Outcome, Phase, PhaseVerdict, ProviderMsg, ToolMsg,
    ROLLBACK_NOTICE,
};
use agent_budget::BudgetHalt;
use agent_event::{
    AgentError, AgentEvent, ControlAck, ControlKind, ControlStatus, DeltaKind, Emitter, Message,
    MessageDelta, Role, RunOutcome as EventRunOutcome, TurnEndReason as EventTurnEndReason,
    UsageReport,
};
use agent_log::{Item, ItemKind, LogWriter, LOG_VERSION};
use provider_core::{AssistantMessage, LlmClient, LlmError, Request, Response, Usage};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tool_core::{Invocation, ToolCall, ToolResult};

/// Provider settle usage → the live-event payload. `cache_write` stays out:
/// the live vocabulary names only what consumers read.
pub(crate) fn usage_report(u: &Usage) -> UsageReport {
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
pub(crate) fn emit_message_frames(
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
                                // `verify.hold` on its tail. No `ToolStart`.
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
pub(crate) fn sync_log(
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

pub(crate) fn event_outcome(outcome: &Outcome) -> EventRunOutcome {
    match outcome {
        Outcome::Done => EventRunOutcome::Passed,
        Outcome::Halted(s) | Outcome::Failed(s) => EventRunOutcome::Failed(s.clone()),
        Outcome::Cancelled => EventRunOutcome::Aborted,
    }
}

/// The durable closer for a hopped turn, read back from the log so live and
/// replay agree exactly.
pub(crate) fn hopped_turn_reason(state: &LoopState, before: u64) -> EventTurnEndReason {
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

/// Private run assembly: the loop state, the snapshot tree, the append-only
/// log writer, the live emitter, and how much of the log is durable. Every
/// phase below goes through [`RunCtx::record`] (append + sync + emit) and
/// [`RunCtx::finish`], so the terminal sites share one fail-closed path and
/// no function grows past ~150 lines.
struct RunCtx<'s, 'r, P> {
    state: &'s mut LoopState,
    provider: &'r P,
    registry: &'r tool_core::Registry,
    agent: &'r str,
    workdir: &'r Path,
    emitter: &'r mut Emitter,
    bets: &'r dyn BetsHook,
    cfg: RunConfig,
    tree: snapshot::TreeService,
    writer: Option<LogWriter>,
    synced: usize,
    run_id: u64,
    root: CancellationToken,
    cancel: &'r CancellationToken,
}

/// Step-head decision: a provider call was admitted (`Call`), the run parks
/// (`Yield` lets the executor breathe, `Again` polls immediately), or the
/// run is terminal (`Exit`, still unfinished until `finish` runs it).
enum Head {
    Call(CancellationToken),
    Yield,
    Again,
    Exit(Outcome),
}

/// One executed tool batch: the proof gate's input plus the pre-batch
/// counter snapshot a full rollback refunds to.
struct BatchReport {
    claim: bets::Claim,
    hunks: Vec<(String, bool)>,
    batch_failed: bool,
    snap: BatchSnapshot,
}

impl<P> RunCtx<'_, '_, P> {
    /// Tail sync: `Err` is already the terminal log-append failure; the
    /// caller finishes with it.
    fn sync(&mut self) -> Result<(), Outcome> {
        sync_log(&self.state.items, self.writer.as_mut(), &mut self.synced)
            .map_err(|_| Outcome::Failed("log append failed".into()))
    }

    /// Durable TurnEnd first, terminal frames second — never inverted. The
    /// emit still runs when the sync fails so live always sees a terminal
    /// frame.
    fn finish(&mut self, outcome: Outcome) -> Outcome {
        let reason = outcome_log_reason(&outcome, self.state.turn_reason.as_ref());
        self.state.stick_turn_reason(reason.clone());
        let turn = self.state.turn;
        let usage = self.state.usage_totals.clone();
        append_to(
            &mut self.state.items,
            ItemKind::TurnEnd {
                turn_id: turn_id(turn),
                reason: reason.clone(),
            },
        );
        let _ = sync_log(&self.state.items, self.writer.as_mut(), &mut self.synced);
        self.emitter.emit(AgentEvent::TurnEnd {
            turn,
            reason: turn_end_reason_to_event(&reason),
            usage_totals: usage,
        });
        self.emitter.emit(AgentEvent::RunEnd {
            outcome: event_outcome(&outcome),
            messages: Vec::new(),
        });
        outcome
    }

    /// Log-before-event by construction: the durable row is appended and
    /// synced before its live frame emits. Rows without a frame pass `None`.
    /// State helpers shared with `drive_tick` (`step_claim`,
    /// `record_tool_result`, `finish_provider_msg`) own their appends, so
    /// their frames still emit after an explicit `sync` at the same site —
    /// the same order, pinned by the characterization tests.
    fn record(&mut self, kind: ItemKind, frame: Option<AgentEvent>) -> Result<(), Outcome> {
        append_to(&mut self.state.items, kind);
        self.sync()?;
        if let Some(frame) = frame {
            self.emitter.emit(frame);
        }
        Ok(())
    }

    /// Follow-up opened a new turn mid-`terminate` (closer already appended
    /// there): sync it, then emit its frame before the new TurnStart.
    fn close_hop(&mut self, before: u64) -> Result<(), Outcome> {
        if self.state.turn == before || before == 0 {
            return Ok(());
        }
        self.sync()?;
        let reason = hopped_turn_reason(self.state, before);
        let usage = self.state.usage_totals.clone();
        self.emitter.emit(AgentEvent::TurnEnd {
            turn: before,
            reason,
            usage_totals: usage,
        });
        self.emitter.emit(AgentEvent::TurnStart {
            turn: self.state.turn,
        });
        Ok(())
    }

    /// Shared tail for terminal-ish claims (Done past its fast path,
    /// Truncated, and the post-batch verdict): terminate, hop the turn, or
    /// park Done when idle.
    fn settle_terminal(&mut self, before: u64) -> Result<(), Outcome> {
        match self.state.terminate() {
            PhaseVerdict::Return(o) => Err(o),
            _ => {
                self.close_hop(before)?;
                if self.state.phase == Phase::Idle && self.state.is_idle() {
                    return Err(Outcome::Done);
                }
                Ok(())
            }
        }
    }
}

impl<P: LlmClient> RunCtx<'_, '_, P> {
    /// Run open: RunStart frame, log writer, Header row, snapshot `ensure`,
    /// input seeding, and the prefix-cache freeze. `Some` is terminal (the
    /// finish-closed cases are already finished inside; log-open and
    /// no-input already emitted their own RunEnd).
    async fn open_run(&mut self, inputs: Vec<Input>) -> Option<Outcome> {
        // Cfg-over-state (precedence is documented on `run`): cfg wins over
        // pre-set state; the fold knobs resolve from the env once, here.
        self.state.drain_timeout = self.cfg.drain_timeout;
        self.state.incentives = self.cfg.incentives;
        self.state.resolve_fold_config();
        self.run_id = self.state.next_emit_id();
        self.emitter.emit(AgentEvent::RunStart {
            run_id: self.run_id,
            goal: self.cfg.goal.clone(),
        });
        if let Some(p) = self.cfg.log_path.clone() {
            match LogWriter::open(&p) {
                Ok(w) => self.writer = Some(w),
                Err(e) => {
                    let o = Outcome::Failed(format!("log open: {e}"));
                    self.emitter.emit(AgentEvent::RunEnd {
                        outcome: event_outcome(&o),
                        messages: Vec::new(),
                    });
                    return Some(o);
                }
            }
        }
        if self.state.items.is_empty() {
            // ponytail: the Header row has no live frame, so `record` covers
            // it with `None` instead of a bespoke append-then-sync.
            if let Err(o) = self.record(
                ItemKind::Header {
                    version: LOG_VERSION,
                    session_id: format!("run-{}", self.run_id),
                    cwd: self.workdir.to_string_lossy().into_owned(),
                    model: self.cfg.model.clone(),
                },
                None,
            ) {
                return Some(self.finish(o));
            }
        }
        if let Err(e) = self.tree.ensure() {
            let o = Outcome::Failed(format!("snapshot ensure: {e}"));
            return Some(self.finish(o));
        }
        if let Err(o) = self.sync() {
            return Some(self.finish(o));
        }
        for input in inputs {
            let before = self.state.turn;
            self.state.apply_input(input);
            if let Err(o) = self.sync() {
                return Some(self.finish(o));
            }
            if self.state.turn != before {
                self.emitter.emit(AgentEvent::TurnStart {
                    turn: self.state.turn,
                });
            }
        }
        if self.state.turn == 0 {
            let o = Outcome::Failed("run needs at least one input".into());
            self.emitter.emit(AgentEvent::RunEnd {
                outcome: event_outcome(&o),
                messages: Vec::new(),
            });
            return Some(o);
        }
        // Prefix-cache head: freeze the file map once, before the first request;
        // mid-run edits must not rotate the system message. Named-file pins freeze
        // on the same beat (dropout rule in `build_request`).
        self.state.file_map = Some(context::file_map(self.workdir, 200).join("\n"));
        self.state.pins = Some(pin_snapshot(&self.cfg.context_files, self.workdir));
        None
    }

    /// Loop head: cancel latch, steering admission, bets step head, then the
    /// provider admission gate with `terminate` as the fallback.
    /// Grace runs inside `start_provider_call`, the halt lands after.
    fn step_head(&mut self) -> Result<Head, Outcome> {
        if self.cancel.is_cancelled() {
            self.state.stop_hard = true;
            self.state.gate.begin_abort();
        }
        self.state.admit_steering();
        self.sync()?;
        // Bets site 1: step head.
        if let PhaseVerdict::Return(o) = self.bets.on_step_head(self.state.step as u64) {
            return Ok(Head::Exit(o));
        }
        // Grace runs here (may_step inside); terminate halts after.
        match self.state.start_provider_call(&self.root) {
            Some(t) => Ok(Head::Call(t)),
            None => match self.state.terminate() {
                PhaseVerdict::Return(o) => Ok(Head::Exit(o)),
                PhaseVerdict::Break => Ok(Head::Yield),
                PhaseVerdict::Continue => {
                    if self.state.phase == Phase::Idle && self.state.is_idle() {
                        return Ok(Head::Exit(Outcome::Done));
                    }
                    Ok(Head::Again)
                }
            },
        }
    }

    /// One provider round: checkpoint race, request build, cancel-raced
    /// `complete`, then settle (failure or claim).
    async fn provider_round(&mut self, turn_token: CancellationToken) -> Result<(), Outcome> {
        // Checkpoint before the real request, after the budget admitted the
        // step: the summary request is real spend and must never be paid for
        // a step the guard would have refused. Raced against cancel so Ctrl-C
        // during the summary call returns promptly (parent-side only; the
        // race drops the summary future and cancels the per-turn child token).
        tokio::select! {
            biased;
            _ = self.cancel.cancelled() => {}
            _ = checkpoint(self.state, self.provider, &self.cfg, self.emitter) => {}
        }
        if self.cancel.is_cancelled() {
            turn_token.cancel();
            self.state.stop_hard = true;
            self.state.gate.begin_abort();
        }
        let req = build_request(self.state, self.registry, self.workdir, &self.cfg);
        // Parent-side cancel race: Ctrl-C during a model call returns promptly
        // instead of waiting out the adapter's 8x60s ladder. The `LlmClient`
        // trait is frozen (no token param), so the cancel branch drops the
        // `complete` future and cancels the per-turn child token.
        let completed = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => None,
            res = self.provider.complete(&self.cfg.model, &req) => Some(res),
        };
        let completed = match completed {
            None => {
                turn_token.cancel();
                self.state.stop_hard = true;
                self.state.gate.begin_abort();
                Err(LlmError::Metered {
                    source: Box::new(LlmError::Cancelled),
                    usage: None,
                    exhausted: false,
                })
            }
            Some(res) => res,
        };
        match completed {
            Err(e) => self.settle_provider_error(e).await,
            Ok(resp) => self.settle_success(resp, req, &turn_token).await,
        }
    }

    /// Provider failure: meter the billed attempts, record the Attempt row,
    /// emit the Error frame, then retry in-step or terminate.
    async fn settle_provider_error(&mut self, e: LlmError) -> Result<(), Outcome> {
        let cancelled = self.cancel.is_cancelled() || self.root.is_cancelled();
        // Metered failures carry what the attempts were billed; every
        // other error shape has no usage to report.
        let (msg, usage) = match e {
            LlmError::Metered {
                source,
                usage,
                exhausted,
            } => {
                // An exhausted ladder already ran the adapter's full retry
                // ladder inside one `complete`: an in-step retry would re-bill
                // every rung for no new information, so the retry budget is
                // spent and the failure goes fatal below.
                if exhausted {
                    self.state.step_retries = 0;
                }
                (format!("{source}"), usage)
            }
            other => (format!("{other:?}"), None),
        };
        let turn = self.state.turn;
        self.state.finish_provider_msg(ProviderMsg::Failed {
            turn,
            err: msg.clone(),
            cancelled,
            usage,
        });
        self.sync()?;
        self.emitter.emit(AgentEvent::Error {
            error: AgentError {
                code: "provider-failed".into(),
                message: msg,
            },
        });
        if self.state.call_model {
            return Ok(()); // in-step retry reuses the same assembly
        }
        match self.state.terminate() {
            PhaseVerdict::Return(o) => Err(o),
            _ => {
                // Never spin: parked-idle without input is Done.
                if self.state.phase == Phase::Idle && self.state.is_idle() {
                    return Err(Outcome::Done);
                }
                Ok(())
            }
        }
    }

    /// Settled response: anchor, meter the bill, claim, sync, frames, then
    /// the claim dispatch.
    async fn settle_success(
        &mut self,
        resp: Response,
        req: Request,
        turn_token: &CancellationToken,
    ) -> Result<(), Outcome> {
        // The request (including any peeked hold row) reached the
        // provider: consume the hold now. A provider Err below keeps
        // it armed for the in-step retry.
        self.state.verify.hold.take();
        let turn = self.state.turn;
        // Checkpoint anchor: this request's history length and its
        // provider-reported prompt tokens (the final attempt's, not
        // the retry ladder's sum). History only (`len() - 1`: the one
        // system head), so the next estimate walks exactly the
        // messages appended after it.
        self.state.anchor = Some(context::UsageAnchor {
            messages: req.messages.len().saturating_sub(1),
            input_tokens: resp.usage.input,
        });
        // Meter the BILL, not just the final attempt: a recovered
        // retry ladder was billed every failed re-send too.
        let billed = resp.billed_usage();
        self.state.finish_provider_msg(ProviderMsg::Settled {
            turn,
            message: resp.message.clone(),
            stop: resp.stop,
            usage: Some(billed.clone()),
        });
        let outcome = self.state.step_claim(resp.message.clone(), resp.stop);
        self.sync()?;
        emit_message_frames(self.state, &resp.message, Some(&billed), self.emitter);
        match outcome {
            ClaimOutcome::VerifyHold => {
                // Unverified declare held: the turn stays alive and
                // the next request carries the directive on its own row.
                // Durable hold record syncs at the next loop head before
                // that request, so the file never lags the wire.
                self.sync()?;
                Ok(())
            }
            ClaimOutcome::Done => {
                // Already-started final step: `record_step` at the head
                // may have hit max, so `terminate`'s `exceeded` would
                // preempt this Done with Halted(steps). The admitted
                // step's declare wins when drained with no followups.
                if self.state.in_flight.is_none()
                    && self.state.open_tools() == 0
                    && !self.state.call_model
                    && self.state.steering.is_empty()
                    && self.state.followups.is_empty()
                    && matches!(
                        self.state.budget.exceeded().as_ref().map(|e| &e.halt),
                        Some(BudgetHalt::Steps)
                    )
                {
                    return Err(Outcome::Done);
                }
                let before = self.state.turn;
                self.settle_terminal(before)
            }
            ClaimOutcome::Truncated(_) | ClaimOutcome::Refused => {
                let before = self.state.turn;
                self.settle_terminal(before)
            }
            ClaimOutcome::HardExit(label) => {
                self.state.fatal_error = Some(label.clone());
                self.state.gate.close(label);
                match self.state.terminate() {
                    PhaseVerdict::Return(o) => Err(o),
                    _ => Ok(()),
                }
            }
            ClaimOutcome::Dispatch(calls) => {
                for c in &calls {
                    self.emitter.emit(AgentEvent::ToolStart {
                        id: c.call_id.clone(),
                        name: c.name.clone(),
                        args: c.args.clone(),
                    });
                }
                let report = self.tool_batch(calls, turn_token).await?;
                self.proof_gate(report).await
            }
        }
    }

    /// One tool call through the registry with the parent-side cancel race:
    /// Ctrl-C during a tool returns promptly instead of waiting it out. The
    /// race drops the `execute` future and cancels the per-turn child token
    /// (which owns each tool's child token) when it loses.
    async fn execute_call(
        &mut self,
        inv: Invocation,
        turn_token: &CancellationToken,
    ) -> ToolResult {
        let name = inv.name.clone();
        match self.registry.resolve(&inv.name) {
            Some(tool) => {
                let tool_token = turn_token.child_token();
                let raced = tokio::select! {
                    biased;
                    _ = self.cancel.cancelled() => None,
                    r = tool.execute(inv, tool_token) => Some(r),
                };
                match raced {
                    None => {
                        turn_token.cancel();
                        self.state.stop_hard = true;
                        self.state.gate.begin_abort();
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
        }
    }

    /// One dispatched batch: snapshot baseline, per-call execute + record +
    /// tail + sync + frame, then the hunk probe. A failed batch rolls back
    /// here (batch scope: the baseline already committed the proven prefix).
    async fn tool_batch(
        &mut self,
        calls: Vec<ToolCall>,
        turn_token: &CancellationToken,
    ) -> Result<BatchReport, Outcome> {
        if let Err(e) = self.tree.baseline() {
            return Err(Outcome::Failed(format!("snapshot baseline: {e}")));
        }
        let mut batch_failed = false;
        // Pre-batch counters: a full rollback refunds `edits` /
        // `actions` / `verify.verified_since_write` past the verdict.
        let batch_snap = snapshot_batch(self.state);
        for c in &calls {
            if self.cancel.is_cancelled() {
                let content = "aborted before dispatch".to_owned();
                self.state.record_tool_result(ToolMsg {
                    call_id: c.call_id.clone(),
                    result: ToolResult {
                        content: content.clone(),
                        is_error: true,
                    },
                });
                batch_failed = true;
            } else {
                let inv = match self.registry.prepare(self.agent, c.clone()) {
                    tool_core::CallStatus::Dispatch(inv) => Some(inv),
                    tool_core::CallStatus::Result(res) => {
                        batch_failed |= res.is_error;
                        self.state.record_tool_result(ToolMsg {
                            call_id: c.call_id.clone(),
                            result: res,
                        });
                        None
                    }
                };
                if let Some(inv) = inv {
                    let name = inv.name.clone();
                    let args = inv.args.to_string();
                    let res = self.execute_call(inv, turn_token).await;
                    batch_failed |= res.is_error;
                    // Shared with `drive_tick`: the same
                    // `note_tool_execution` + `record` order.
                    let _ = note_tool_execution(self.state, &name, &args, &res);
                    self.state.record_tool_result(ToolMsg {
                        call_id: c.call_id.clone(),
                        result: res.clone(),
                    });
                }
            }
            // Directives and the nudge land on the fresh tail
            // BEFORE the sync, so the file never holds a stale
            // tail and the row keeps the exact model text.
            // Shared with `drive_tick` (`settle_tool_tail`).
            settle_tool_tail(self.state);
            self.sync()?;
            if let Some(done) = self
                .state
                .tool_calls
                .get(&c.call_id)
                .and_then(|s| s.result.clone())
            {
                self.emitter.emit(AgentEvent::ToolEnd {
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
            let probe = match &self.cfg.proof_cmd {
                Some(cmd) => incremental_hunks(&self.tree, self.workdir, cmd).await,
                None => batch_hunks(&self.tree, !batch_failed),
            };
            match probe {
                Ok(h) => h,
                Err(e) => return Err(Outcome::Failed(e)),
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
            // ponytail: `record` with no frame keeps the row durable
            // without inventing an event for it.
            self.record(
                ItemKind::Attempt {
                    error: ROLLBACK_NOTICE.into(),
                    will_retry: true,
                },
                None,
            )?;
            self.state.push_directive(ROLLBACK_NOTICE.into());
            if let Err(e) = self.tree.rollback() {
                return Err(Outcome::Failed(format!("snapshot rollback: {e}")));
            }
            // Rolled-back edits must not linger in the
            // counters: refund to the pre-batch snapshot.
            refund_batch(self.state, &batch_snap);
            // Not counted in AblationMetrics.rollbacks: this
            // legacy rollback is pre-gate and identical in all
            // ablation arms. rollbacks = GATE interventions
            // (Partial/Aborted verdicts) only.
        }
        Ok(BatchReport {
            claim,
            hunks,
            batch_failed,
            snap: batch_snap,
        })
    }

    /// Proof-gated post-batch verdict: Committed stands (the failed-batch
    /// rollback above is the existing path either way), Partial restores the
    /// kept prefix from baseline, Aborted runs the existing rollback path
    /// again (idempotent). A restore error fails closed: restore_hunks leaves
    /// the tree at baseline when any fragment does not apply.
    async fn proof_gate(&mut self, report: BatchReport) -> Result<(), Outcome> {
        let BatchReport {
            claim,
            hunks,
            batch_failed,
            snap,
        } = report;
        let observation = if batch_failed {
            "tool batch results not all passed"
        } else {
            "tool batch results all passed"
        };
        self.state
            .ablation
            .note_assessment(bets::assess_claim(&claim, observation));
        let verdict = self.bets.on_post_batch(&claim, &hunks);
        if !hunks.is_empty() {
            // The gate's domain is patch batches: an empty
            // batch has nothing proven, so its Committed must
            // not inflate proven_hunks. rollbacks counts gate
            // interventions (Partial/Aborted) only — the
            // legacy failed-batch rollback above is pre-gate
            // and identical across ablation arms.
            self.state.ablation.note_commit(&verdict);
        }
        match verdict {
            bets::CommitVerdict::Committed => {}
            bets::CommitVerdict::Partial { savepoint } => {
                if let Err(e) = self.tree.restore_hunks(&savepoint.kept_hunks) {
                    return Err(Outcome::Failed(format!("snapshot restore: {e}")));
                }
            }
            bets::CommitVerdict::Aborted { .. } => {
                if let Err(e) = self.tree.rollback() {
                    return Err(Outcome::Failed(format!("snapshot rollback: {e}")));
                }
                // Gate-aborted batch is a full rollback: the
                // counters refund like the tool-error path.
                // (`Partial` keeps its prefix, so its
                // counters stand.)
                refund_batch(self.state, &snap);
            }
        }
        // Bets site 2 step hook (unchanged): Return ends the run.
        if let PhaseVerdict::Return(o) = self.bets.on_step() {
            return Err(o);
        }
        let before = self.state.turn;
        self.settle_terminal(before)
    }

    /// Headless drive: one step head + provider round per turn until a
    /// phase returns terminal. Every `Err` is unfinished; `finish` runs it.
    async fn drive(&mut self) -> Outcome {
        loop {
            let token = match self.step_head() {
                Err(o) => return self.finish(o),
                Ok(Head::Exit(o)) => return self.finish(o),
                Ok(Head::Yield) => {
                    tokio::task::yield_now().await;
                    continue;
                }
                Ok(Head::Again) => continue,
                Ok(Head::Call(token)) => token,
            };
            if let Err(o) = self.provider_round(token).await {
                return self.finish(o);
            }
        }
    }
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
///
/// Precedence — the single place this is stated (cfg-over-state): `cfg`
/// wins over pre-set `LoopState` fields. `run` applies `cfg.incentives` over
/// `state.incentives` and resolves the fold knobs from the environment;
/// `state.budget` arrives pre-seeded (rof builds it from the capability
/// preset plus CLI overrides via the `agent_budget` constructors) and `run`
/// never rebuilds it.
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
    let tree = snapshot::TreeService::new(workdir);
    let mut ctx = RunCtx {
        state,
        provider,
        registry,
        agent,
        workdir,
        emitter,
        bets,
        cfg,
        tree,
        writer: None,
        synced: 0,
        run_id: 0,
        root: CancellationToken::new(),
        cancel,
    };
    if let Some(o) = ctx.open_run(inputs).await {
        return o;
    }
    ctx.drive().await
}
