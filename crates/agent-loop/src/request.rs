//! Prompt build: request fold, budget tail, and compaction.

use crate::{Checkpoint, IncentivesLevel, LoopState, RunConfig};
use agent_event::{AgentError, AgentEvent, Emitter};
use agent_log::ItemKind;
use provider_core::{
    AssistantMessage, LlmClient, LlmError, ProviderMessage, Request, StopReason, Thinking,
};
use serde_json::Value;
use std::path::Path;

impl LoopState {
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
    /// the collapse hysteresis from the run-head snapshot
    /// ([`Self::resolve_fold_config`]), never the environment.
    pub(crate) fn raw_messages(&self) -> Vec<ProviderMessage> {
        self.raw_messages_with(self.collapse_hysteresis)
    }

    /// [`Self::raw_messages`] with the collapse hysteresis taken as a plain
    /// parameter: tests drive H directly instead of mutating the environment.
    /// The thinking keep-count always comes from the run-head snapshot
    /// ([`Self::thinking_keep`]).
    pub(crate) fn raw_messages_with(&self, hysteresis: usize) -> Vec<ProviderMessage> {
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
        // keep the last cached `thinking_keep` assistant rows, blank the older ones.
        let keep_from = out
            .iter()
            .filter(|m| m.role == "assistant")
            .count()
            .saturating_sub(self.thinking_keep);
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

/// Header of the one row a checkpoint injects: the model must read it as
/// context state, not as a new user instruction.
pub(crate) const CHECKPOINT_PREFIX: &str =
    "[context checkpoint: earlier turns were summarized to save tokens; the durable log still holds them]";

pub(crate) fn summary_message(summary: &str) -> ProviderMessage {
    ProviderMessage {
        role: "user".into(),
        content: format!("{CHECKPOINT_PREFIX}\n{summary}"),
        tool_calls: Vec::new(),
        tool_call_id: None,
        thinking: None,
    }
}

/// Static workflow contract: the read→edit→finish obligations the transcript
/// alone does not state. Byte-identical for the whole run. Live budget digits
/// must not appear here: the system message is the cached-prefix head, and a
/// changing head invalidates the provider's prefix cache every request. The
/// counters ride the request tail instead (see [`build_request`]).
pub(crate) const WORKFLOW_CONTRACT: &str = "WORKFLOW CONTRACT\n\
    budgets remaining are printed on the last line of the newest message; read them there.\n\
    tool results older than the last 5 are collapsed to one line; re-open a file immediately before editing it.\n\
    work file-by-file: view -> edit immediately -> next file. `edit` and `write` are the ONLY patch mechanisms — never write files via exec; exec/test are for checks only.\n\
    when your patch is complete, reply with a text message and NO tool calls — that finishes the run.";

/// Live counters, request-scoped by design: appended to the final outgoing
/// message after the transcript is derived, so they are never persisted.
/// Durability lives in `BudgetGuard` counters + `TurnEnd.usage_totals`;
/// persisting these digits into the transcript would rotate the cached-prefix
/// head every request. Pinned by `build_request_system_is_static...` tests.
pub(crate) fn budget_line(state: &LoopState) -> String {
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
pub(crate) const PIN_CAP_CHARS: usize = 8000;

/// Run-start pin contents: one read per named file, reused for the whole run
/// (see [`LoopState::pins`]). Unreadable files are simply not pinned.
pub(crate) fn pin_snapshot(files: &[String], workdir: &Path) -> Vec<(String, String)> {
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
/// budget line is the sole per-request variation
/// (wire-only clone of `Request.messages`, never `state.items`; the empty-history
/// edge makes it the sole `user` message): it is appended to the last
/// outgoing message only and never to the transcript, so the durable log stays
/// exactly what was recorded. History is delivered exactly once, raw, as
/// messages (collapse-5 rides [`LoopState::derived_messages`]) — never fitted
/// into the system copy. Never summarizes.
pub(crate) fn build_request(
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
    if let Some(nudge) = state.verify.hold.as_ref().cloned() {
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
pub(crate) fn compact_view(messages: &[ProviderMessage]) -> Vec<context::CompactMessage> {
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
pub(crate) async fn checkpoint<P: LlmClient>(
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
