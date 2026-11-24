//! Multi-tick driver: headless [`run`].

use crate::proof::BatchSnapshot;
use crate::state::THINKING_KEEP;
use crate::verify::is_verification_call;
use crate::{
    append_to, batch_hunks, build_request, checkpoint, freeze_context_head, incremental_hunks,
    note_tool_execution, outcome_log_reason, outcome_to_result, refund_batch, settle_tool_tail,
    snapshot_batch, turn_end_reason_to_event, turn_id, BetsHook, ClaimOutcome, FailureKind,
    IncentivesLevel, Input, LoopState, Outcome, Phase, PhaseVerdict, ROLLBACK_NOTICE,
};
use agent_budget::{halt_name, BudgetHalt};
use agent_event::{
    AgentError, AgentEvent, DeltaKind, Emitter, Message, MessageDelta, Role,
    RunOutcome as EventRunOutcome, UsageReport,
};
use agent_log::{Item, ItemKind, LogWriter, LOG_VERSION};
use provider_core::{AssistantMessage, LlmClient, LlmError, Request, Response, Usage};
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tool_core::{Invocation, ToolCall, ToolResult};

/// Provider settle usage → the live-event payload.
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
    content: &str,
    usage: Option<&Usage>,
    emitter: &mut Emitter,
) {
    let partial = Message {
        role: Role::Assistant,
        content: content.to_owned(),
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
            text: Some(content.to_owned()),
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

/// What the token cap counts. `Gross` charges `input + output` (today's
/// behavior: the frozen recipe's default); `MissPlusOut` charges
/// `(input - cache_read) + output`, so provider-cached prompt re-reads stop
/// counting against the cap (the subtraction saturates). `cache_read ⊆
/// input` is a property of the OpenAI-shaped wire
/// (`provider-openai/src/protocol.rs` maps `cached_tokens` inside
/// `prompt_tokens`, the same miss share `pricing.rs` charges), not a
/// documented `provider-core` invariant — an adapter that reports cache
/// reads outside `input` must not use `MissPlusOut`. Reporting and dollars
/// are unchanged by either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BudgetShape {
    #[default]
    Gross,
    MissPlusOut,
}

impl std::str::FromStr for BudgetShape {
    type Err = String;
    /// CLI spellings, one source for both the `run` and `eval` tables.
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "gross" => Ok(Self::Gross),
            "miss-plus-out" | "miss_plus_out" => Ok(Self::MissPlusOut),
            _ => Err(format!(
                "unknown budget shape {s:?}: expected gross or miss-plus-out"
            )),
        }
    }
}

/// Step at whose head the named-deliverable check runs. Provenance: chosen
/// from docs/experiments/2026-11-11-allowlist-and-named-deliverable.md --
/// 0/13 banked runs lacking the deliverable at step 15 solved, and 0/2
/// solves would have been flagged.
pub const DELIVERABLE_CHECK_STEP: usize = 15;

/// Headless run knobs. `drain_timeout` defaults to 30s (unattended runs must
/// not wedge on a stuck tool).
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// Opt-in six-read intervention; counterfactual crossings are logged in both arms.
    pub streak_steer: bool,
    /// Output files the task names, relative to the workdir. Checked once at
    /// [`DELIVERABLE_CHECK_STEP`]; a missing one is logged as a crossing.
    pub deliverables: Vec<PathBuf>,
    /// Opt-in: also queue one directive naming the missing deliverables.
    pub deliverable_steer: bool,
    pub goal: String,
    pub model: String,
    pub max_tokens: usize,
    pub context_files: Vec<String>,
    pub drain_timeout: Duration,
    pub log_path: Option<PathBuf>,
    /// Ablation knob: incentive scaffold level (default [`IncentivesLevel::Full`]).
    pub incentives: IncentivesLevel,
    /// Per-hunk incremental proof probe (Bet A): whitespace-argv, no shell,
    /// run against each leading-prefix tree in `workdir`. `None` = the uniform
    /// proof flag (the batch's tool results all passed).
    pub proof_cmd: Option<String>,
    /// Exact operator-supplied command checked when the model declares Done.
    pub completion_check: Option<String>,
    /// Advertise current WAL-local IDs on collapsed observations.
    pub history_read: bool,
    /// Compaction checkpoint knobs (default OFF: no summary call, no fold).
    pub compaction: context::CompactionConfig,
    /// Transcript-fold knobs, the single source of truth: echoed-reasoning
    /// keep-count (default [`THINKING_KEEP`]) and collapse-boundary
    /// hysteresis (default [`context::COLLAPSE_HYSTERESIS`]). Arms set these
    /// fields directly; the loop and `context` never read the environment.
    pub thinking_keep: usize,
    pub collapse_hysteresis: usize,
    /// Token-cap accounting shape (default [`BudgetShape::Gross`]: the frozen
    /// recipe's gross input+output). Arms set the field directly; the loop
    /// never reads the environment.
    pub budget_shape: BudgetShape,
    /// Steering interventions: the unverified-declare hold, the half/late
    /// action directives and the budget wrap-up nudge. The loop default stays
    /// `true` (the substrate's historical behavior, which the nudge tests
    /// exercise); BOTH shipped CLI surfaces default to `false` — `dphn-steer2`
    /// measured the nudges dead (2026-11-06, docs/decisions.md): no named-test
    /// change on either cell, 2.64x tokens per solved task. `--steer on`
    /// re-enables them; the workflow contract and the rollback notice ship
    /// either way.
    pub steer: bool,
    /// Keep changes made before a completed test/exec check fails.
    pub preserve_failed_checks: bool,
    /// `--rollback-scope errors`: a call that ran to completion and reported
    /// failure (non-zero exit, FAIL verdict) no longer rolls the batch back;
    /// denials, invalid args and tool errors still do, and the rollback
    /// notice is sent only when the batch changed files. Off = legacy.
    pub rollback_errors_only: bool,
    /// Replace the restrictive legacy workflow text with a concise workflow.
    pub minimal_workflow: bool,
    /// Number of newest tool observations kept verbatim in the prompt.
    pub observation_keep: usize,
    /// Intervene after this many identical failed checks with unchanged files; zero disables it.
    pub stall_threshold: usize,
    /// Budget line in requests left, sized by the last request, without the
    /// ambiguous `N/M` form (`--budget-line requests`).
    pub budget_requests: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            streak_steer: false,
            deliverables: Vec::new(),
            deliverable_steer: false,
            goal: "run".into(),
            model: "run-model".into(),
            max_tokens: 2000,
            context_files: Vec::new(),
            drain_timeout: Duration::from_secs(30),
            log_path: None,
            incentives: IncentivesLevel::Full,
            proof_cmd: None,
            completion_check: None,
            history_read: false,
            compaction: context::CompactionConfig::default(),
            thinking_keep: THINKING_KEEP,
            collapse_hysteresis: context::COLLAPSE_HYSTERESIS,
            budget_shape: BudgetShape::Gross,
            steer: true,
            preserve_failed_checks: false,
            rollback_errors_only: false,
            minimal_workflow: false,
            observation_keep: 5,
            stall_threshold: 0,
            budget_requests: false,
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

pub(crate) fn event_outcome(outcome: &Outcome) -> EventRunOutcome {
    match outcome {
        Outcome::Done => EventRunOutcome::Passed,
        Outcome::Halted(s) => EventRunOutcome::Failed(s.clone()),
        Outcome::Failed { message, .. } => EventRunOutcome::Failed(message.clone()),
        Outcome::Cancelled => EventRunOutcome::Aborted,
    }
}

/// Blocking fs/git work off the async executor: snapshot, file-map, pins,
/// and log IO all run here so a slow disk never stalls the executor. These
/// complete fast and are not raced against cancel (abandoning a half-done
/// rollback would leave the workdir unknown); the provider/tool races that
/// must return promptly keep their own `select!`.
pub(crate) async fn blocking<F, T>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .expect("blocking worker panicked")
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
    tree: Arc<snapshot::TreeService>,
    writer: Option<LogWriter>,
    synced: usize,
    run_id: u64,
    root: CancellationToken,
    cancel: &'r CancellationToken,
    stalled_check: Option<StalledCheck>,
    completion_checks: u8,
}

struct StalledCheck {
    signature: String,
    output: String,
    tree: u64,
    count: usize,
    reported: bool,
}

fn workspace_identity(root: &Path, excluded: Option<&Path>) -> Result<u64, String> {
    fn visit(
        root: &Path,
        dir: &Path,
        excluded: Option<&Path>,
        hasher: &mut DefaultHasher,
    ) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let rel = path.strip_prefix(root).map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if Some(path.as_path()) == excluded
                || rel == Path::new(".git")
                || rel == Path::new("target")
                || rel == Path::new(".rof-events.jsonl")
                || name.starts_with(".rof-dump")
            {
                continue;
            }
            let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            rel.hash(hasher);
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path).map_err(|e| e.to_string())?;
                b'L'.hash(hasher);
                target.hash(hasher);
            } else if meta.is_dir() {
                b'D'.hash(hasher);
                visit(root, &path, excluded, hasher)?;
            } else if meta.is_file() {
                b'F'.hash(hasher);
                let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
                let mut chunk = [0u8; 8192];
                loop {
                    let n = file.read(&mut chunk).map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    chunk[..n].hash(hasher);
                }
            }
        }
        Ok(())
    }
    let mut hasher = DefaultHasher::new();
    visit(root, root, excluded, &mut hasher)?;
    Ok(hasher.finish())
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

async fn wait_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// One executed tool batch: the proof gate's input plus the pre-batch
/// counter snapshot a full rollback refunds to.
struct BatchReport {
    claim: bets::Claim,
    hunks: Vec<(String, bool)>,
    batch_failed: bool,
    check_failed: bool,
    snap: BatchSnapshot,
}

impl<P> RunCtx<'_, '_, P> {
    /// Tail sync on the blocking worker: the pending rows are cloned
    /// (usually one or two small items; empty tails skip the thread hop
    /// entirely) and appended + fsynced off the executor, preserving the old
    /// append-only tail sync's stop-at-first-error progress (the vec is the
    /// mirror, the file is the record; a failed pre-effect append is a hard
    /// failure, never run from process-only state). `Err` is already the
    /// terminal log-append failure; the caller finishes with it.
    async fn sync(&mut self) -> Result<(), Outcome> {
        if self.writer.is_none() || self.synced >= self.state.items.len() {
            return Ok(());
        }
        let writer = self.writer.take().expect("writer checked above");
        let pending: Vec<Item> = self.state.items[self.synced..].to_vec();
        let base = self.synced;
        let (writer, done, failed) = blocking(move || {
            let mut writer = writer;
            let mut done = base;
            let mut failed = false;
            for item in &pending {
                match writer.append(item) {
                    Ok(()) => done += 1,
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            (writer, done, failed)
        })
        .await;
        self.writer = Some(writer);
        self.synced = done;
        if failed {
            return Err(Outcome::Failed {
                kind: FailureKind::Log,
                message: "log append failed".into(),
            });
        }
        Ok(())
    }

    /// Durable TurnEnd first, terminal frames second — never inverted. The
    /// emit still runs when the sync fails so live always sees a terminal
    /// frame.
    async fn finish(&mut self, outcome: Outcome) -> Outcome {
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
        // Best-effort durable close: the frames emit even when this fails.
        let _ = self.sync().await;
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
    /// State helpers (`step_claim`, `record_tool_result`,
    /// `on_provider_failed`) own their appends, so their frames still emit
    /// after an explicit `sync` at the same site.
    async fn record(&mut self, kind: ItemKind, frame: Option<AgentEvent>) -> Result<(), Outcome> {
        append_to(&mut self.state.items, kind);
        self.sync().await?;
        if let Some(frame) = frame {
            self.emitter.emit(frame);
        }
        Ok(())
    }

    fn wallclock_deadline(&self) -> Option<tokio::time::Instant> {
        let wall = self.state.budget.config().max_wallclock;
        (!wall.is_zero())
            .then(|| tokio::time::Instant::now() + wall.saturating_sub(self.state.budget.elapsed()))
    }

    fn abort_turn(&mut self, turn_token: &CancellationToken) {
        turn_token.cancel();
        self.state.stop_hard = true;
    }

    async fn rollback_tree(&mut self, with_notice: bool) -> Result<(), Outcome> {
        if with_notice {
            self.record(
                ItemKind::Attempt {
                    error: ROLLBACK_NOTICE.into(),
                    will_retry: true,
                },
                None,
            )
            .await?;
            self.state.push_directive(ROLLBACK_NOTICE.into(), &self.cfg);
        }
        let tree = self.tree.clone();
        match blocking(move || tree.rollback()).await {
            Ok(left) if !left.is_empty() => {
                // Root-owned paths the host-side git cannot delete: the batch is
                // rolled back for every path it COULD restore, and the survivors
                // are named. Failing the stage here loses the row for a
                // permission bit (observed on Slice A).
                let shown = left.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
                self.record(
                    ItemKind::Attempt {
                        error: format!("rollback left {} unremovable path(s): {shown}", left.len()),
                        will_retry: false,
                    },
                    None,
                )
                .await?;
            }
            Ok(_) => {}
            Err(e) => {
                return Err(Outcome::Failed {
                    kind: FailureKind::Snapshot,
                    message: format!("snapshot rollback: {e}"),
                });
            }
        }
        Ok(())
    }

    /// Shared step/terminal settlement: optional last-step Done grace (an
    /// already-started final step whose `record_step` hit `max_steps` wins
    /// when drained), then `terminate`, then parked-idle Done.
    fn settle_step(&mut self, allow_step_grace: bool) -> Result<Head, Outcome> {
        if allow_step_grace
            && !self.state.in_flight
            && self.state.open_tools() == 0
            && !self.state.call_model
            && self.state.steering.is_empty()
            && matches!(
                self.state.budget.exceeded().as_ref().map(|e| &e.halt),
                Some(BudgetHalt::Steps)
            )
        {
            return Err(Outcome::Done);
        }
        match self.state.terminate(&self.cfg) {
            PhaseVerdict::Return(o) => Err(o),
            PhaseVerdict::Break => Ok(Head::Yield),
            PhaseVerdict::Continue => {
                if self.state.phase == Phase::Idle && self.state.is_idle() {
                    Err(Outcome::Done)
                } else {
                    Ok(Head::Again)
                }
            }
        }
    }

    fn settle_terminal(&mut self, allow_step_grace: bool) -> Result<(), Outcome> {
        self.settle_step(allow_step_grace).map(|_| ())
    }
}

impl<P: LlmClient> RunCtx<'_, '_, P> {
    /// Run open: RunStart frame, log writer, Header row, snapshot `ensure`,
    /// input seeding, and the prefix-cache freeze. `Some` is terminal (the
    /// finish-closed cases are already finished inside; log-open and
    /// no-input already emitted their own RunEnd).
    async fn open_run(&mut self, inputs: Vec<Input>) -> Option<Outcome> {
        self.run_id = self.state.next_emit_id();
        self.emitter.emit(AgentEvent::RunStart {
            run_id: self.run_id,
            goal: self.cfg.goal.clone(),
        });
        if let Some(p) = self.cfg.log_path.clone() {
            match blocking(move || LogWriter::open(&p)).await {
                Ok(w) => self.writer = Some(w),
                Err(e) => {
                    let o = Outcome::Failed {
                        kind: FailureKind::Log,
                        message: format!("log open: {e}"),
                    };
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
            if let Err(o) = self
                .record(
                    ItemKind::Header {
                        version: LOG_VERSION,
                        session_id: format!("run-{}", self.run_id),
                        cwd: self.workdir.to_string_lossy().into_owned(),
                        model: self.cfg.model.clone(),
                    },
                    None,
                )
                .await
            {
                return Some(self.finish(o).await);
            }
        }
        let tree = self.tree.clone();
        if let Err(e) = blocking(move || tree.ensure()).await {
            let o = Outcome::Failed {
                kind: FailureKind::Snapshot,
                message: format!("snapshot ensure: {e}"),
            };
            return Some(self.finish(o).await);
        }
        if let Err(o) = self.sync().await {
            return Some(self.finish(o).await);
        }
        for input in inputs {
            let before = self.state.turn;
            self.state.apply_input(input);
            if let Err(o) = self.sync().await {
                return Some(self.finish(o).await);
            }
            if self.state.turn != before {
                self.emitter.emit(AgentEvent::TurnStart {
                    turn: self.state.turn,
                });
            }
        }
        if self.state.turn == 0 {
            let o = Outcome::Failed {
                kind: FailureKind::Input,
                message: "run needs at least one input".into(),
            };
            self.emitter.emit(AgentEvent::RunEnd {
                outcome: event_outcome(&o),
                messages: Vec::new(),
            });
            return Some(o);
        }
        // Prefix-cache head: freeze the file map once, before the first request;
        // mid-run edits must not rotate the system message. Named-file pins freeze
        // on the same beat (dropout rule in `build_request`).
        // Prefix-cache freeze on the blocking worker: the directory walk
        // and pin reads are fs IO, never executor work.
        let root = self.workdir.to_path_buf();
        let files = self.cfg.context_files.clone();
        let (file_map, pins) = blocking(move || freeze_context_head(&root, &files)).await;
        self.state.file_map = Some(file_map);
        self.state.pins = Some(pins);
        None
    }

    /// Loop head: cancel latch, steering admission, then the provider admission
    /// gate with `terminate` as the fallback. Grace runs inside
    /// `start_provider_call`, the halt lands after.
    async fn step_head(&mut self) -> Result<Head, Outcome> {
        if self.cancel.is_cancelled() {
            self.state.stop_hard = true;
        }
        self.state.admit_steering();
        self.sync().await?;
        // Grace runs here (may_step inside); terminate halts after.
        match self.state.start_provider_call(&self.root) {
            Some(t) => Ok(Head::Call(t)),
            None => match self.settle_step(false) {
                Ok(head) => Ok(head),
                Err(o) => Ok(Head::Exit(o)),
            },
        }
    }

    /// Once per run, at the head of step [`DELIVERABLE_CHECK_STEP`]: log the
    /// missing named deliverables; with `deliverable_steer`, queue one directive.
    /// Never terminates the run.
    async fn check_deliverables(&mut self) -> Result<(), Outcome> {
        if self.state.step < DELIVERABLE_CHECK_STEP as u32
            || self.state.experiment.deliverable_checked
        {
            return Ok(());
        }
        self.state.experiment.deliverable_checked = true;
        if self.cfg.deliverables.is_empty() {
            return Ok(());
        }
        let (root, want) = (self.workdir.to_path_buf(), self.cfg.deliverables.clone());
        let missing = blocking(move || {
            want.iter()
                .filter(|p| !root.join(p).exists())
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .await;
        if missing.is_empty() {
            return Ok(());
        }
        self.record(
            ItemKind::Attempt {
                error: format!(
                    "deliverable-missing step={} paths={missing}",
                    self.state.step
                ),
                will_retry: false,
            },
            None,
        )
        .await?;
        if self.cfg.deliverable_steer {
            self.state.push_directive(
                format!("The required output {missing} does not exist yet. Create it now, then refine it."),
                &self.cfg,
            );
        }
        Ok(())
    }

    /// One provider round: checkpoint race, request build, cancel-raced
    /// `complete`, then settle (failure or claim).
    async fn provider_round(&mut self, turn_token: CancellationToken) -> Result<(), Outcome> {
        let deadline = self.wallclock_deadline();
        // Checkpoint before the real request, after the budget admitted the
        // step: the summary request is real spend and must never be paid for
        // a step the guard would have refused. Raced against cancel so Ctrl-C
        // during the summary call returns promptly (parent-side only; the
        // race drops the summary future and cancels the per-turn child token).
        let mut checkpoint_started = false;
        let expired = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => false,
            _ = wait_deadline(deadline) => true,
            _ = checkpoint(self.state, self.provider, &self.cfg, self.emitter, &mut checkpoint_started) => false,
        };
        if expired {
            turn_token.cancel();
            return self.wallclock_halt(checkpoint_started).await;
        }
        if self.cancel.is_cancelled() {
            if checkpoint_started {
                self.provider_usage_unknown();
            }
            self.abort_turn(&turn_token);
            return self.settle_provider_error(LlmError::Cancelled).await;
        }
        self.check_deliverables().await?;
        let req = build_request(self.state, self.registry, self.workdir, &self.cfg);
        if self.state.experiment.streak_pending {
            self.record(
                ItemKind::Attempt {
                    error: format!("streak-request id=read-streak-1 step={}", self.state.step),
                    will_retry: true,
                },
                None,
            )
            .await?;
        }
        // Parent-side cancel race: Ctrl-C during a model call returns promptly
        // instead of waiting out the adapter's 8x60s ladder. The `LlmClient`
        // trait is frozen (no token param), so the cancel branch drops the
        // `complete` future and cancels the per-turn child token.
        let mut provider_started = false;
        let completed = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => None,
            _ = wait_deadline(deadline) => {
                turn_token.cancel();
                return self.wallclock_halt(provider_started).await;
            }
            res = async { provider_started = true; self.provider.complete(&self.cfg.model, &req).await } => Some(res),
        };
        let completed = match completed {
            None => {
                if provider_started {
                    self.provider_usage_unknown();
                }
                self.abort_turn(&turn_token);
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

    fn provider_usage_unknown(&mut self) {
        self.state.usage_totals.cost_usd = None;
        self.emitter.emit(AgentEvent::Error {
            error: AgentError {
                code: "provider-usage-unknown".into(),
                message: "provider request was interrupted; usage may be incomplete".into(),
            },
        });
    }

    async fn wallclock_halt(&mut self, provider_started: bool) -> Result<(), Outcome> {
        self.state.in_flight = false;
        let message = if provider_started {
            self.state.usage_totals.cost_usd = None;
            "wallclock deadline interrupted a provider request; usage may be incomplete"
        } else {
            "wallclock deadline exceeded before a provider request started"
        };
        self.record(
            ItemKind::Attempt {
                error: message.into(),
                will_retry: false,
            },
            None,
        )
        .await?;
        if provider_started {
            self.emitter.emit(AgentEvent::Error {
                error: AgentError {
                    code: "wallclock-usage-unknown".into(),
                    message: message.into(),
                },
            });
        }
        Err(Outcome::Halted("wallclock".into()))
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
        self.state
            .on_provider_failed(&msg, cancelled, usage.as_ref());
        self.sync().await?;
        self.emitter.emit(AgentEvent::Error {
            error: AgentError {
                code: "provider-failed".into(),
                message: msg,
            },
        });
        if self.state.call_model {
            return Ok(()); // in-step retry reuses the same assembly
        }
        self.settle_terminal(false)
    }

    /// Settled response: anchor, meter the bill, claim, sync, frames, then
    /// the claim dispatch.
    async fn settle_success(
        &mut self,
        resp: Response,
        req: Request,
        turn_token: &CancellationToken,
    ) -> Result<(), Outcome> {
        let streak_delivered = self.state.experiment.streak_pending
            && req
                .messages
                .iter()
                .any(|m| m.content == crate::request::READ_STREAK_DIRECTIVE);
        // The request (including any peeked hold row) reached the
        // provider: consume the hold now. A provider Err below keeps
        // it armed for the in-step retry.
        self.state.verify.hold.take();
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
        self.state.on_provider_settled(Some(&billed));
        if streak_delivered {
            self.state.experiment.streak_pending = false;
            self.record(
                ItemKind::Attempt {
                    error: format!("streak-delivered id=read-streak-1 step={}", self.state.step),
                    will_retry: false,
                },
                None,
            )
            .await?;
        }
        let msg_content = resp.message.content.clone();
        let outcome = self.state.step_claim(resp.message, resp.stop, &self.cfg);
        self.sync().await?;
        emit_message_frames(self.state, &msg_content, Some(&billed), self.emitter);
        match outcome {
            ClaimOutcome::VerifyHold => {
                // Unverified declare held: the turn stays alive and
                // the next request carries the directive on its own row.
                // Durable hold record syncs at the next loop head before
                // that request, so the file never lags the wire.
                self.sync().await?;
                Ok(())
            }
            ClaimOutcome::Done => {
                if let Some(command) = self.cfg.completion_check.clone() {
                    return self.settle_completion_check(&command, turn_token).await;
                }
                self.settle_terminal(true)
            }
            ClaimOutcome::Truncated(_) | ClaimOutcome::Refused => self.settle_terminal(false),
            ClaimOutcome::HardExit(label) => {
                self.state.fatal_error = Some(label);
                self.settle_terminal(false)
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

    async fn settle_completion_check(
        &mut self,
        command: &str,
        turn_token: &CancellationToken,
    ) -> Result<(), Outcome> {
        if self.completion_checks >= 2 {
            return Err(Outcome::Failed {
                kind: FailureKind::Completion,
                message: "completion predicate was not satisfied".into(),
            });
        }
        if self.cancel.is_cancelled() || self.state.stop_hard {
            return Err(Outcome::Cancelled);
        }
        if let Some(exceeded) = self.state.budget.exceeded_except_steps() {
            return Err(Outcome::Halted(halt_name(&exceeded.halt).into()));
        }
        let n = self.completion_checks + 1;
        self.completion_checks = n;
        let mut call_id = format!("harness-completion-{n}");
        while self.state.items.iter().any(
            |item| matches!(&item.kind, ItemKind::ToolCall { call_id: id, .. } if id == &call_id),
        ) {
            call_id.push_str("-h");
        }
        let args = serde_json::json!({"cmd": command});
        self.state.budget.counters_mut().actions_this_trial += 1;
        self.record(
            ItemKind::Attempt {
                error: format!("harness-origin completion-check call_id={call_id}"),
                will_retry: false,
            },
            None,
        )
        .await?;
        // Strict providers require an assistant tool-call declaration before
        // its result. This row is explicitly harness-origin and has no usage.
        let declaration = AssistantMessage {
            content: "[harness-origin public completion check]".into(),
            tool_calls: vec![provider_core::ToolCallRef {
                id: call_id.clone(),
                name: tool_core::TOOL_TEST.into(),
                args: args.clone(),
            }],
            thinking: None,
        };
        self.state
            .push_assistant(&declaration, "harness-completion");
        append_to(
            &mut self.state.items,
            ItemKind::ToolCall {
                call_id: call_id.clone(),
                tool: tool_core::TOOL_TEST.into(),
                args: args.clone(),
            },
        );
        self.state.tool_calls.insert(
            call_id.clone(),
            crate::ToolCallState {
                name: tool_core::TOOL_TEST.into(),
                args: args.to_string(),
                is_verification: true,
                result: None,
            },
        );
        self.sync().await?;
        emit_message_frames(self.state, &declaration.content, None, self.emitter);
        self.emitter.emit(AgentEvent::ToolStart {
            id: call_id.clone(),
            name: tool_core::TOOL_TEST.into(),
            args: args.clone(),
        });

        let tool_call = ToolCall {
            call_id: call_id.clone(),
            name: tool_core::TOOL_TEST.into(),
            args: args.clone(),
        };
        let prepared = self.registry.prepare(self.agent, tool_call);
        let (result, completed, dispatched) = match prepared {
            tool_core::CallStatus::Dispatch(inv) => {
                let tree = self.tree.clone();
                if let Err(e) = blocking(move || tree.baseline()).await {
                    return Err(Outcome::Failed {
                        kind: FailureKind::Snapshot,
                        message: format!("snapshot baseline: {e}"),
                    });
                }
                let (result, completed) = self.execute_call(inv, turn_token).await;
                (result, completed, true)
            }
            tool_core::CallStatus::Result(result) => {
                self.state.budget.counters_mut().actions_this_trial -= 1;
                (result, true, false)
            }
        };
        if dispatched {
            // The action was reserved before execution. The shared settlement
            // records it once and updates the same-action tripwire normally.
            self.state.budget.counters_mut().actions_this_trial -= 1;
            let _ =
                note_tool_execution(self.state, tool_core::TOOL_TEST, &args.to_string(), &result);
            self.state.observe_read_streak(
                tool_core::TOOL_TEST,
                result.is_error,
                self.cfg.streak_steer,
            );
        }
        self.state.record_tool_result(&call_id, result.clone());
        settle_tool_tail(self.state, &self.cfg);
        self.sync().await?;
        self.emitter.emit(AgentEvent::ToolEnd {
            id: call_id,
            result: Value::String(result.content.clone()),
            is_error: result.is_error,
        });

        if !result.is_error {
            if self.cancel.is_cancelled() || self.state.stop_hard {
                return Err(Outcome::Cancelled);
            }
            if let Some(exceeded) = self.state.budget.exceeded_except_steps() {
                return Err(Outcome::Halted(halt_name(&exceeded.halt).into()));
            }
            self.state.call_model = false;
            return self.settle_terminal(true);
        }
        let denied = result.content.starts_with("denied:")
            || result.content.starts_with("ask denied")
            || result.content.starts_with("unknown tool:")
            || result.content.starts_with("invalid args:");
        if denied {
            return Err(Outcome::Failed {
                kind: FailureKind::Completion,
                message: format!("completion predicate could not run: {}", result.content),
            });
        }
        if dispatched && result.is_error && (!self.cfg.preserve_failed_checks || !completed) {
            self.rollback_tree(true).await?;
        }
        if self.cancel.is_cancelled() || self.state.stop_hard {
            return Err(Outcome::Cancelled);
        }
        if !completed && result.content.contains("wallclock") {
            return Err(Outcome::Halted("wallclock".into()));
        }
        if let Some(exceeded) = self.state.budget.exceeded() {
            return Err(Outcome::Halted(halt_name(&exceeded.halt).into()));
        }
        if self.completion_checks >= 2 {
            return Err(Outcome::Failed {
                kind: FailureKind::Completion,
                message: format!("completion predicate failed: {}", result.content),
            });
        }
        self.state.call_model = true;
        Ok(())
    }

    async fn monitor_failed_check(
        &mut self,
        name: &str,
        args: &Value,
        output: &str,
    ) -> Result<(), Outcome> {
        let threshold = self.cfg.stall_threshold;
        if threshold == 0 {
            return Ok(());
        }
        let root = self.workdir.to_path_buf();
        let excluded = self.cfg.log_path.as_ref().map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                root.join(path)
            }
        });
        let Ok(tree) = blocking(move || workspace_identity(&root, excluded.as_deref())).await
        else {
            return Ok(());
        };
        let signature = format!("{name}:{args}");
        let count = match self.stalled_check.as_mut() {
            Some(last)
                if last.signature == signature && last.output == output && last.tree == tree =>
            {
                last.count = last.count.saturating_add(1);
                last.count
            }
            _ => {
                self.stalled_check = Some(StalledCheck {
                    signature: signature.clone(),
                    output: output.to_owned(),
                    tree,
                    count: 1,
                    reported: false,
                });
                1
            }
        };
        let Some(last) = self.stalled_check.as_mut() else {
            return Ok(());
        };
        if count == threshold && !last.reported {
            last.reported = true;
            let evidence = format!(
                "stall: same failed check repeated {count} times with identical output and unchanged observed source evidence; check={signature}; output={output}"
            );
            self.record(
                ItemKind::Attempt {
                    error: evidence.clone(),
                    will_retry: true,
                },
                None,
            )
            .await?;
            self.state.push_directive(
                format!("{evidence}. Change the relevant files before rerunning this check."),
                &self.cfg,
            );
        }
        Ok(())
    }

    /// One tool call through the registry with the parent-side cancel race:
    /// Ctrl-C during a tool returns promptly instead of waiting it out. The
    /// race drops the `execute` future and cancels the per-turn child token
    /// (which owns each tool's child token) when it loses.
    async fn execute_call(
        &mut self,
        inv: Invocation,
        turn_token: &CancellationToken,
    ) -> (ToolResult, bool) {
        let name = inv.name.clone();
        match self.registry.resolve(&inv.name) {
            Some(tool) => {
                let tool_token = turn_token.child_token();
                let deadline = self.wallclock_deadline();
                let raced = tokio::select! {
                    biased;
                    _ = self.cancel.cancelled() => Some(None),
                    _ = wait_deadline(deadline) => None,
                    r = tool.execute(inv, tool_token) => Some(Some(r)),
                };
                match raced {
                    Some(None) => {
                        self.abort_turn(turn_token);
                        (
                            ToolResult {
                                content: "cancelled".into(),
                                is_error: true,
                            },
                            false,
                        )
                    }
                    None => {
                        turn_token.cancel();
                        (
                            ToolResult {
                                content: "wallclock deadline exceeded during tool execution".into(),
                                is_error: true,
                            },
                            false,
                        )
                    }
                    Some(Some(Ok(o))) => (outcome_to_result(o), true),
                    Some(Some(Err(e))) => (ToolResult::from(e), false),
                }
            }
            None => (
                ToolResult {
                    content: format!("unknown tool: {name}"),
                    is_error: true,
                },
                false,
            ),
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
        let tree = self.tree.clone();
        if let Err(e) = blocking(move || tree.baseline()).await {
            return Err(Outcome::Failed {
                kind: FailureKind::Snapshot,
                message: format!("snapshot baseline: {e}"),
            });
        }
        let mut batch_failed = false;
        let mut check_failed = false;
        // Rollback restores edit and verification state; executed actions
        // remain charged against the run budget.
        let batch_snap = snapshot_batch(self.state);
        for c in &calls {
            let action_cap = self.state.budget.config().actions_per_trial;
            let wall = self.state.budget.config().max_wallclock;
            if !wall.is_zero() && self.state.budget.elapsed() >= wall {
                self.state.record_tool_result(
                    &c.call_id,
                    ToolResult {
                        content: "wallclock budget exhausted before dispatch".into(),
                        is_error: true,
                    },
                );
            } else if action_cap > 0
                && self.state.budget.counters().actions_this_trial >= action_cap
            {
                self.state.record_tool_result(
                    &c.call_id,
                    ToolResult {
                        content: "action budget exhausted before dispatch".into(),
                        is_error: true,
                    },
                );
            } else if self.cancel.is_cancelled() {
                self.state.record_tool_result(
                    &c.call_id,
                    ToolResult {
                        content: "aborted before dispatch".into(),
                        is_error: true,
                    },
                );
                batch_failed = true;
            } else {
                let inv = match self.registry.prepare(self.agent, c.clone()) {
                    tool_core::CallStatus::Dispatch(inv) => Some(inv),
                    tool_core::CallStatus::Result(res) => {
                        batch_failed |= res.is_error;
                        self.state.record_tool_result(&c.call_id, res);
                        None
                    }
                };
                if let Some(inv) = inv {
                    let name = inv.name.clone();
                    let args = inv.args.to_string();
                    let (res, completed) = self.execute_call(inv, turn_token).await;
                    let executed_failed_check =
                        completed && res.is_error && is_verification_call(&name, &c.args);
                    let failed_check = (self.cfg.preserve_failed_checks && executed_failed_check)
                        || (self.cfg.rollback_errors_only && completed && res.is_error);
                    check_failed |= failed_check;
                    batch_failed |= res.is_error && !failed_check;
                    let _ = note_tool_execution(self.state, &name, &args, &res);
                    self.state.record_tool_result(&c.call_id, res.clone());
                    if executed_failed_check {
                        self.monitor_failed_check(&name, &c.args, &res.content)
                            .await?;
                    } else if is_verification_call(&name, &c.args) {
                        self.stalled_check = None;
                    }
                }
            }
            if let Some(result) = self
                .state
                .tool_calls
                .get(&c.call_id)
                .and_then(|s| s.result.clone())
            {
                self.state
                    .observe_read_streak(&c.name, result.is_error, self.cfg.streak_steer);
            }
            // Directives and the nudge land on the fresh tail
            // BEFORE the sync, so the file never holds a stale
            // tail and the row keeps the exact model text.
            // Shared with `drive_tick` (`settle_tool_tail`).
            settle_tool_tail(self.state, &self.cfg);
            self.sync().await?;
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
            predicted_verdict: if check_failed {
                "a verification check failed".to_owned()
            } else {
                "tool batch results all passed".to_owned()
            },
            on_mismatch: "roll back the batch; keep only proven hunks".to_owned(),
        };
        let hunks = {
            let wall = self.state.budget.config().max_wallclock;
            if self.cfg.proof_cmd.is_some()
                && !batch_failed
                && !wall.is_zero()
                && self.state.budget.elapsed() >= wall
            {
                return Err(Outcome::Halted("wallclock".into()));
            }
            let proof_cmd = self
                .cfg
                .proof_cmd
                .as_ref()
                .filter(|_| wall.is_zero() || self.state.budget.elapsed() < wall);
            let probe = match proof_cmd {
                Some(cmd) => {
                    let deadline = self.wallclock_deadline();
                    incremental_hunks(self.tree.clone(), self.workdir, cmd, deadline).await
                }
                None => batch_hunks(self.tree.clone(), !batch_failed && !check_failed).await,
            };
            match probe {
                Ok(h) if check_failed => h.into_iter().map(|(hunk, _)| (hunk, false)).collect(),
                Ok(h) => h,
                Err(e) if e == "proof wallclock deadline exceeded" => {
                    return Err(Outcome::Halted("wallclock".into()));
                }
                Err(e) => {
                    return Err(Outcome::Failed {
                        kind: FailureKind::Snapshot,
                        message: e,
                    });
                }
            }
        };
        // Batch scope: only the failed batch rolls back; the
        // baseline already committed the proven prefix.
        // Errors-only scope: a failed batch that changed no file has nothing
        // to revert, so it gets neither a rollback nor the notice.
        if batch_failed && !(self.cfg.rollback_errors_only && hunks.is_empty()) {
            // Pre-effect record: the rollback destroys the batch's successful
            // edits, so the notice is durable before the tree moves.
            self.rollback_tree(true).await?;
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
            check_failed,
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
            check_failed,
            snap,
        } = report;
        let observation = if batch_failed || check_failed {
            "tool batch results not all passed"
        } else {
            "tool batch results all passed"
        };
        self.state
            .experiment
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
            self.state.experiment.ablation.note_commit(&verdict);
        }
        match verdict {
            bets::CommitVerdict::Committed => {}
            bets::CommitVerdict::Partial { savepoint } => {
                let tree = self.tree.clone();
                if let Err(e) = blocking(move || tree.restore_hunks(&savepoint.kept_hunks)).await {
                    return Err(Outcome::Failed {
                        kind: FailureKind::Snapshot,
                        message: format!("snapshot restore: {e}"),
                    });
                }
            }
            bets::CommitVerdict::Aborted { .. } => {
                self.rollback_tree(false).await?;
                // Gate-aborted batch is a full rollback: the
                // counters refund like the tool-error path.
                // (`Partial` keeps its prefix, so its
                // counters stand.)
                refund_batch(self.state, &snap);
            }
        }
        self.settle_terminal(false)
    }

    /// Headless drive: one step head + provider round per turn until a
    /// phase returns terminal. Every `Err` is unfinished; `finish` runs it.
    async fn drive(&mut self) -> Outcome {
        loop {
            let token = match self.step_head().await {
                Err(o) => return self.finish(o).await,
                Ok(Head::Exit(o)) => return self.finish(o).await,
                Ok(Head::Yield) => {
                    tokio::task::yield_now().await;
                    continue;
                }
                Ok(Head::Again) => continue,
                Ok(Head::Call(token)) => token,
            };
            if let Err(o) = self.provider_round(token).await {
                return self.finish(o).await;
            }
        }
    }
}

/// Headless multi-tick run on real siblings: [`LlmClient`] provider,
/// [`tool_core::Registry`] tools, [`BudgetGuard`] step head,
/// [`snapshot::TreeService`] baseline-per-batch with batch-scope rollback
/// (proven prefix survives: the baseline commits it first), context prompt,
/// [`BetsHook`] proof-gated post-batch gate (`on_post_batch`: Committed stands,
/// Partial restores the kept hunks, Aborted rolls the batch back), [`Emitter`]
/// frames, [`LogWriter`] file record. Termination order is
/// [`LoopState::terminate`]; the grace step runs inside
/// `start_provider_call`, the halt lands after. Parked-idle with empty queues
/// is Done.
///
/// Single source of truth: `cfg` owns every run knob (incentives, drain
/// deadline, fold knobs, compaction, proof command, budget shape). The one
/// config copy is `cfg.budget_shape` -> `state.budget_shape`, seeded below
/// because the sole charge site (`record_usage`) sees `LoopState`, not `cfg`.
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
    state.budget_shape = cfg.budget_shape; // the one config copy; see the `run` doc
    let tree = Arc::new(snapshot::TreeService::new(workdir));
    let synced = state.persisted;
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
        synced,
        run_id: 0,
        root: CancellationToken::new(),
        cancel,
        stalled_check: None,
        completion_checks: 0,
    };
    let outcome = match ctx.open_run(inputs).await {
        Some(o) => o,
        None => ctx.drive().await,
    };
    ctx.state.persisted = ctx.synced;
    outcome
}
