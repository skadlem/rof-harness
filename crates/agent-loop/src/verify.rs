//! Mid-run verification nudge: declare hold and verification calls.

use crate::{
    append_to, ClaimOutcome, IncentivesLevel, LoopState, RunConfig, ToolCallState, ToolMsg,
};
use agent_log::{ItemKind, TurnEndReason};
use provider_core::{AssistantMessage, StopReason};
use serde_json::Value;
use tool_core::{ToolCall, ToolResult, TOOL_EDIT, TOOL_EXEC, TOOL_TEST, TOOL_WRITE};

/// Mid-run verification nudge: held-declare directive (spec draft verbatim)
/// plus the per-run budget. Full incentives only; Base/Contract never see it.
pub(crate) const VERIFY_NUDGE: &str = "Unverified declare held: no passing verification run since your last write. Either run the task's verification now, or state exactly what blocks it (missing tool/package/file) and what you verified instead. Declaring without one of those two is not completing.";
pub(crate) const VERIFY_NUDGE_CAP: u32 = 2;

/// Mid-run verification nudge state (Full incentives only): nudges used
/// (cap [`VERIFY_NUDGE_CAP`]), edits at the last nudge (a second declare
/// without an intervening write stays silent), the passing-verify-since-write
/// flag, and the request-scoped hold text for the next request tail.
#[derive(Debug, Clone, Default)]
pub struct VerifyState {
    pub nudges_used: u32,
    pub edits_at_last_nudge: u32,
    pub verified_since_write: bool,
    pub hold: Option<String>,
}

impl LoopState {
    pub(crate) fn push_assistant(&mut self, message: &AssistantMessage, stop_label: &str) {
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
    /// `cfg` is the single source of run configuration (incentive level
    /// gates the nudge here).
    pub fn step_claim(
        &mut self,
        message: AssistantMessage,
        stop: StopReason,
        cfg: &RunConfig,
    ) -> ClaimOutcome {
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
                    if let Some(text) = self.verify_nudge_due(cfg) {
                        self.verify.hold = Some(text.clone());
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
    pub(crate) fn verify_nudge_due(&mut self, cfg: &RunConfig) -> Option<String> {
        if cfg.incentives < IncentivesLevel::Full {
            return None;
        }
        if self.verify.nudges_used >= VERIFY_NUDGE_CAP {
            return None;
        }
        if self.edits <= self.verify.edits_at_last_nudge {
            return None;
        }
        if self.verify.verified_since_write {
            return None;
        }
        let cap = self.budget.config().actions_per_trial;
        let actions = self.budget.counters().actions_this_trial;
        if cap > 0 && actions.saturating_mul(10) >= cap.saturating_mul(9) {
            return None;
        }
        // Exhausted budget gets no grace hold: consult the guard `terminate`
        // halts on, covering steps/tokens/spend/wallclock and both action
        // caps in one AND-gate read.
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
        self.verify.nudges_used += 1;
        self.verify.edits_at_last_nudge = self.edits;
        Some(VERIFY_NUDGE.into())
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
                if name == TOOL_EDIT || name == TOOL_WRITE {
                    self.verify.verified_since_write = false;
                } else if is_verification {
                    self.verify.verified_since_write = true;
                }
            }
        }
        if self.batch_complete() {
            self.call_model = true;
        }
        true
    }
}

/// A passing result on one of these counts as a verification run since the
/// last write: the `test` tool, or `exec` whose `cmd` arg runs a test
/// command (`pytest`, `cargo test`, `go test`, `npm test`, `npm run test`).
/// Only the command position counts: `pip install pytest` and `grep pytest`
/// name pytest without running it, so neither verifies.
pub(crate) fn is_verification_call(name: &str, args: &Value) -> bool {
    if name == TOOL_TEST {
        return true;
    }
    if name != TOOL_EXEC {
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
pub(crate) fn stop_label(stop: StopReason) -> String {
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
