//! Multi-tick drivers: headless `run` and channel `drive_tick`.

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
use provider_core::{AssistantMessage, LlmClient, LlmError, Usage};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tool_core::ToolResult;

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

/// Durable TurnEnd first, terminal frames second — never inverted. The emit
/// still runs when the sync fails so live always sees a terminal frame.
pub(crate) fn finish_run(
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

/// Follow-up opened a new turn mid-`terminate` (closer already appended
/// there): sync it, then emit its frame before the new TurnStart. False
/// means the durable append failed and the caller must fail closed.
pub(crate) fn close_hopped_turn(
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
    // Cfg-over-state (precedence is documented on `run`): cfg wins over
    // pre-set state; the fold knobs resolve from the env once, here.
    state.drain_timeout = cfg.drain_timeout;
    state.incentives = cfg.incentives;
    state.resolve_fold_config();
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
                    source: Box::new(LlmError::Cancelled),
                    usage: None,
                    exhausted: false,
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
                    LlmError::Metered { source, usage, .. } => (format!("{source}"), usage),
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
                state.verify.hold.take();
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
                        // `actions` / `verify.verified_since_write` past the verdict.
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
