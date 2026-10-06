//! Batch proof: patch splitting, incremental hunks, and bets wiring.

use crate::{LoopState, PhaseVerdict, ToolMsg};
use agent_budget::BudgetHalt;
use std::path::Path;
use std::time::Duration;
use tool_core::{ToolOutcome, ToolResult};

/// Model-facing notice when one failed tool reverts the whole batch.
pub(crate) const ROLLBACK_NOTICE: &str = "a tool in your last batch failed and the whole batch was reverted — your successful changes in it are gone; re-apply them";

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

/// Split `TreeService::patch` text into the fragments
/// `TreeService::restore_hunks` accepts: one file header (`diff --git` /
/// `index` / `---` / `+++`) directly followed by exactly one `@@` hunk — the
/// same companion splitter the snapshot tests exercise. Untracked-file
/// evidence (appended without an `@@`) rides the trailing fragment's tail; a
/// restore that hits it fails closed to baseline. On a truncated patch the
/// final fragment may be incomplete, so it is dropped.
pub(crate) fn split_patch(patch: &snapshot::PatchText) -> Vec<String> {
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
pub(crate) fn batch_hunks(
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
pub(crate) async fn incremental_hunks(
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
pub(crate) fn outcome_to_result(outcome: ToolOutcome) -> ToolResult {
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
pub(crate) fn note_tool_execution(
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
pub(crate) fn settle_tool_tail(state: &mut LoopState) {
    state.queue_directives();
    state.apply_budget_nudge();
    state.deliver_directives();
}

/// Shared single-result settlement for the channel harness: the same
/// observe/`edits`/`record`/tail order `run` uses per executed call. Looks
/// the `name`/`args` sig up from the claim row, so the tripwire sees the
/// identical bytes. Returns false when the call id was unknown or already
/// answered (no effects applied).
pub(crate) fn settle_tool_msg(state: &mut LoopState, msg: ToolMsg) -> bool {
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
/// actions), and `verify.verified_since_write` are refunded to this snapshot on a
/// full-batch rollback. Directive one-shot latches are NOT refunded: fired
/// text is already durable on a `ToolResult` tail and must not repeat.
#[derive(Debug, Clone)]
pub(crate) struct BatchSnapshot {
    pub(crate) edits: u32,
    pub(crate) actions_this_trial: u32,
    pub(crate) same_action_streak: u32,
    pub(crate) last_sig: String,
    pub(crate) last_obs: u64,
    pub(crate) verified_since_write: bool,
}

/// Capture the pre-batch counters. Call once per `Dispatch` batch, before
/// the first tool executes.
pub(crate) fn snapshot_batch(state: &LoopState) -> BatchSnapshot {
    BatchSnapshot {
        edits: state.edits,
        actions_this_trial: state.budget.counters().actions_this_trial,
        same_action_streak: state.budget.counters().same_action_streak,
        last_sig: state.last_sig.clone(),
        last_obs: state.last_obs,
        verified_since_write: state.verify.verified_since_write,
    }
}

/// Refund a fully rolled-back batch to its pre-batch snapshot. Call after
/// `tree.rollback` (tool-error path) and after a gate `Aborted` restore.
/// A gate `Partial` keeps its prefix on disk, so its counters stand (the
/// kept hunks still describe edits; per-hunk counter attribution is
/// deferred).
pub(crate) fn refund_batch(state: &mut LoopState, snap: &BatchSnapshot) {
    state.edits = snap.edits;
    state.budget.counters_mut().actions_this_trial = snap.actions_this_trial;
    state.budget.counters_mut().same_action_streak = snap.same_action_streak;
    state.last_sig = snap.last_sig.clone();
    state.last_obs = snap.last_obs;
    state.verify.verified_since_write = snap.verified_since_write;
}
