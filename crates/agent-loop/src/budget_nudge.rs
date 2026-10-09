//! Incentive scaffold: directives and budget nudges.

use crate::LoopState;
use crate::RunConfig;
use agent_budget::{BudgetHalt, Nudge};
use agent_log::{Item, ItemKind};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Queued directives kept before the oldest is dropped (drop-oldest: the
/// newest counter warning outranks stale advice).
pub(crate) const DIRECTIVE_CAP: usize = 3;

impl LoopState {
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

    /// One queued directive, capped at [`DIRECTIVE_CAP`] (drop-oldest).
    /// `cfg` is the single source of run configuration (the incentive
    /// level gates the channel here).
    pub fn push_directive(&mut self, text: String, cfg: &RunConfig) {
        if cfg.incentives < IncentivesLevel::Full {
            return; // ablation arm: the directive channel is off entirely
        }
        if self.pending_directives.len() >= DIRECTIVE_CAP {
            self.pending_directives.pop_front();
        }
        self.pending_directives.push_back(text);
    }

    /// Action-counter directives, checked after every recorded action so the
    /// text rides that action's own not-yet-synced row. One-shot per run.
    pub fn queue_directives(&mut self, cfg: &RunConfig) {
        let cap = self.budget.config().actions_per_trial;
        let actions = self.budget.counters().actions_this_trial;
        if cap == 0 {
            return;
        }
        if !self.experiment.half_directive_sent && actions >= cap / 2 && self.edits == 0 {
            self.experiment.half_directive_sent = true;
            self.push_directive(format!(
                "0 edits so far after {actions} actions. Stop reading. Apply your first edit with the edit tool NOW."
            ), cfg);
        }
        if !self.experiment.late_directive_sent && actions >= cap * 4 / 5 {
            self.experiment.late_directive_sent = true;
            self.push_directive(format!(
                "only {} actions remain before the run is stopped. Finish and submit your patch now.",
                cap - actions
            ), cfg);
        }
    }

    /// Deliver queued directives onto the newest
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
        delivered
    }

    /// Newest ToolResult row: the one mutable delivery surface. Appends must
    /// happen before that row is synced, so the durable log carries exactly
    /// the text the model saw and replay from the file stays faithful.
    pub(crate) fn has_tool_tail(&self) -> bool {
        self.items
            .iter()
            .rev()
            .any(|i| matches!(i.kind, ItemKind::ToolResult { .. }))
    }

    pub(crate) fn append_to_tail(&mut self, text: &str) {
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

    pub(crate) fn next_emit_id(&mut self) -> u64 {
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
}

// --- multi-tick run() assembly (headless; sequential, no channels) ---

/// Incentive scaffold level for the B→+A→+C ablation. `Base` ships no
/// workflow contract and drops the directive channel; `Contract` adds the
/// static workflow contract; `Full` (default = current behavior) adds the
/// model-facing directives (cap notices, rollback notices).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum IncentivesLevel {
    Base,
    Contract,
    #[default]
    Full,
}
