//! Batch proof: patch splitting, incremental hunks, and bets wiring.

use crate::run::blocking;
use crate::LoopState;
use agent_budget::BudgetHalt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tool_core::{ToolOutcome, ToolResult};

#[cfg(unix)]
struct ProofGroup(u32);

#[cfg(unix)]
impl Drop for ProofGroup {
    fn drop(&mut self) {
        unsafe {
            libc::killpg(self.0 as libc::pid_t, libc::SIGKILL);
        }
    }
}

/// Model-facing notice when one failed tool reverts the whole batch.
pub(crate) const ROLLBACK_NOTICE: &str = "a tool in your last batch failed and the whole batch was reverted — your successful changes in it are gone; re-apply them";

/// Bets hook with a no-op default: loop compiles and ships with bets disabled.
///
/// Receivers are `&self` (`Run::bets` is `&dyn`, which `rof` requires); hooks
/// that need mutable state use interior mutability.
pub trait BetsHook: Send + Sync {
    /// Post-batch: proof-gated commit over the batch's patch fragments
    /// (`(hunk, proof_passed)`). Permissive default = feature off unless a hook
    /// opts in: `Committed` leaves the batch as the existing path left it,
    /// `Partial` restores `savepoint.kept_hunks`, `Aborted` rolls the batch back.
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

/// The batch's patch fragments with the uniform proof flag attached to each.
/// On a truncated patch the final fragment may be incomplete, so it is dropped.
/// Snapshot reads run on the blocking worker (git IO off the executor).
pub(crate) async fn batch_hunks(
    tree: Arc<snapshot::TreeService>,
    proof_passed: bool,
) -> Result<Vec<(String, bool)>, String> {
    blocking(move || {
        let diff = tree.diff().map_err(|e| format!("snapshot diff: {e}"))?;
        let patch = tree
            .patch(&diff)
            .map_err(|e| format!("snapshot patch: {e}"))?;
        let mut hunks = snapshot::split_patch(&patch.text);
        if patch.truncated {
            hunks.pop(); // the marker landed in the last fragment: never restore it
        }
        Ok(hunks.into_iter().map(|h| (h, proof_passed)).collect())
    })
    .await
}

/// Per-hunk incremental proof (Bet A): probe every leading prefix against the
/// tree it actually produces — restore baseline + hunks[0..=k], run `proof_cmd`
/// (whitespace argv, no shell, 60s kill; timeout/failure = unproven), record
/// ok_k. The bet gate keeps the proven LEADING prefix
/// ([`bets::split_savepoint`]); a later hunk that passes after a failing
/// prefix is not proven against committed state and reverts with the rest.
/// Snapshot restores run on the blocking worker; the probe child stays async.
/// Leaves the tree at baseline + all hunks (the last prefix); the verdict
/// mapping below performs the final restore.
pub(crate) async fn incremental_hunks(
    tree: Arc<snapshot::TreeService>,
    workdir: &Path,
    proof_cmd: &str,
    deadline: Option<tokio::time::Instant>,
) -> Result<Vec<(String, bool)>, String> {
    if proof_cmd.split_whitespace().next().is_none() {
        return Err("proof-cmd empty".to_string());
    }
    let hunks = blocking({
        let tree = tree.clone();
        move || {
            let diff = tree.diff().map_err(|e| format!("snapshot diff: {e}"))?;
            let patch = tree
                .patch(&diff)
                .map_err(|e| format!("snapshot patch: {e}"))?;
            if patch.truncated {
                return Err("snapshot patch truncated; refusing proof restoration".to_string());
            }
            Ok::<_, String>(snapshot::split_patch(&patch.text))
        }
    })
    .await?;
    let mut flags = Vec::with_capacity(hunks.len());
    for k in 0..hunks.len() {
        if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) {
            let tree = tree.clone();
            blocking(move || {
                tree.restore_hunks(&hunks)
                    .map_err(|e| format!("snapshot restore: {e}"))
            })
            .await?;
            return Err("proof wallclock deadline exceeded".into());
        }
        let prefix = hunks[..=k].to_vec();
        let restore_tree = tree.clone();
        if let Err(error) = blocking(move || {
            restore_tree
                .restore_hunks(&prefix)
                .map_err(|e| format!("snapshot restore: {e}"))
        })
        .await
        {
            let tree = tree.clone();
            blocking(move || {
                tree.restore_hunks(&hunks)
                    .map_err(|e| format!("snapshot restore: {e}"))
            })
            .await?;
            return Err(error);
        }
        let mut argv = proof_cmd.split_whitespace();
        let bin = argv.next().ok_or_else(|| "proof-cmd empty".to_string())?;
        let mut command = tokio::process::Command::new(bin);
        command
            .args(argv)
            .current_dir(workdir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                let tree = tree.clone();
                blocking(move || {
                    tree.restore_hunks(&hunks)
                        .map_err(|e| format!("snapshot restore: {e}"))
                })
                .await?;
                return Err(format!("proof spawn: {e}"));
            }
        };
        #[cfg(unix)]
        let _group = child.id().map(ProofGroup);
        let probe_deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let until = deadline.map_or(probe_deadline, |d| d.min(probe_deadline));
        let status = tokio::time::timeout_at(until, child.wait()).await;
        if status.is_err() {
            #[cfg(unix)]
            drop(_group);
            let _ = child.kill().await;
            let _ = child.wait().await;
            if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) {
                let tree = tree.clone();
                blocking(move || {
                    tree.restore_hunks(&hunks)
                        .map_err(|e| format!("snapshot restore: {e}"))
                })
                .await?;
                return Err("proof wallclock deadline exceeded".into());
            }
        }
        let ok = status.is_ok_and(|s| s.is_ok_and(|s| s.success()));
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

pub(crate) fn note_tool_execution(
    state: &mut LoopState,
    name: &str,
    args_str: &str,
    result: &ToolResult,
) -> Option<BudgetHalt> {
    let halt = state.observe_action(&format!("{name}:{args_str}"), &result.content);
    if !result.is_error && crate::is_edit_tool(name) {
        state.edits += 1;
    }
    halt
}

/// Shared tool-tail settlement: counter directives, the budget wrap-up nudge,
/// and delivery onto the newest `ToolResult` tail. Must run BEFORE the log
/// sync, so the file never holds a stale tail and the row keeps the exact
/// model-visible text.
pub(crate) fn settle_tool_tail(state: &mut LoopState, cfg: &crate::RunConfig) {
    state.queue_directives(cfg);
    state.apply_budget_nudge(cfg);
    state.deliver_directives();
}

/// Batch-scope edit and verification snapshot for a full rollback.
/// Executed actions and the tripwire are retained even when edits roll back.
/// Directive one-shot latches are NOT refunded: fired
/// text is already durable on a `ToolResult` tail and must not repeat.
#[derive(Debug, Clone)]
pub(crate) struct BatchSnapshot {
    pub(crate) edits: u32,
    pub(crate) verified_since_write: bool,
}

/// Capture the pre-batch counters. Call once per `Dispatch` batch, before
/// the first tool executes.
pub(crate) fn snapshot_batch(state: &LoopState) -> BatchSnapshot {
    BatchSnapshot {
        edits: state.edits,
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
    state.verify.verified_since_write = snap.verified_since_write;
}
