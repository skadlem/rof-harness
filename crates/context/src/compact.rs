// ===== compaction checkpoint (refs-gap-matrix rank 2) =====

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Tool results in the summarizer payload are cut to this many chars: a
/// checkpoint buys a summary, not a second copy of the transcript.
pub const SUMMARY_TOOL_CAP: usize = 2_000;

/// Compaction checkpoint knobs. `Default` is OFF: until a caller opts in, the
/// collapse-5 request path is byte-identical.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionConfig {
    pub enabled: bool,
    /// Trigger fraction of usable context capacity (0.7 = 70%).
    pub frac: f64,
    /// Recent tokens kept verbatim (pi `keepRecentTokens`).
    pub keep_tokens: usize,
    /// Known model input window. `None` means compaction cannot run.
    pub context_capacity_tokens: Option<u64>,
    /// Output headroom carved out of the context window.
    pub output_reserve_tokens: u64,
    /// Use source-attributed JSON checkpoint summaries as an opt-in treatment.
    pub checkpoint_sources: bool,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            frac: 0.7,
            keep_tokens: 20_000,
            context_capacity_tokens: None,
            output_reserve_tokens: 0,
            checkpoint_sources: false,
        }
    }
}

/// Usage anchor: the provider-reported prompt tokens of the last settled
/// request and the history length that request carried. The estimate walks
/// only the messages the anchor does not cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageAnchor {
    pub messages: usize,
    pub input_tokens: u64,
}

/// One conversation message as the compactor sees it: wire role and the text
/// the summarizer reads (assistant thinking and tool calls are folded into
/// `content` by the caller). `role == "tool"` marks a tool result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactMessage {
    pub role: String,
    pub content: String,
    /// Original durable item ID, captured before transcript folding.
    pub source_item_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    Fact,
    Hypothesis,
    Plan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointEntry {
    pub kind: CheckpointKind,
    pub text: String,
    pub source_item_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointDocument {
    entries: Vec<CheckpointEntry>,
}

fn chars_to_tokens(chars: usize) -> u64 {
    (chars as u64).div_ceil(4)
}

/// Chars/4 token estimate, no tokenizer. Anchored: the last request's
/// provider-reported input plus every message appended after it. An anchor
/// longer than the list (or none at all: the first request, and the request
/// right after a checkpoint) falls back to the whole list. The estimate
/// covers the history only — the system head and the tool declarations ride
/// the anchor once a request has settled.
pub fn estimate_tokens(messages: &[CompactMessage], anchor: Option<&UsageAnchor>) -> u64 {
    let anchor = anchor.filter(|a| a.messages <= messages.len());
    let from = anchor.map(|a| a.messages).unwrap_or(0);
    let chars: usize = messages[from..]
        .iter()
        .map(|m| m.content.chars().count())
        .sum();
    anchor.map(|a| a.input_tokens).unwrap_or(0) + chars_to_tokens(chars)
}

/// Trigger above the configured context window's usable input capacity
/// (`context_capacity_tokens - output_reserve_tokens`).
pub fn compaction_due(estimate: u64, cfg: &CompactionConfig) -> bool {
    let Some(total) = cfg.context_capacity_tokens else {
        return false;
    };
    if !cfg.enabled
        || total == 0
        || cfg.output_reserve_tokens >= total
        || !cfg.frac.is_finite()
        || !(0.0..=1.0).contains(&cfg.frac)
    {
        return false;
    }
    let capacity = total.saturating_sub(cfg.output_reserve_tokens);
    (estimate as f64) > capacity as f64 * cfg.frac
}

/// Cut point: index of the first history message kept verbatim. Walks back
/// from the newest message accumulating chars/4 until the kept tail reaches
/// `keep_tokens`, then keeps from the closest valid cut at or after that
/// point. A tool result is never a cut point: the assistant call that
/// produced it stays with it. When trailing tool results alone blow the
/// budget, the cut falls back to that assistant call rather than into the
/// group. `None` = below the keep budget, or nothing to summarize.
pub fn cut_point(messages: &[CompactMessage], keep_tokens: usize) -> Option<usize> {
    if messages.len() < 2 {
        return None;
    }
    let mut accumulated = 0u64;
    let mut crossing = None;
    for i in (1..messages.len()).rev() {
        accumulated += chars_to_tokens(messages[i].content.chars().count());
        if accumulated >= keep_tokens as u64 {
            crossing = Some(i);
            break;
        }
    }
    let crossing = crossing?;
    let valid = |i: usize| messages[i].role != "tool";
    (crossing..messages.len())
        .find(|&i| valid(i))
        .or_else(|| (1..crossing).rev().find(|&i| valid(i)))
}

/// Summarizer payload: one `[role]: text` block per message, tool results cut
/// to [`SUMMARY_TOOL_CAP`] chars with an explicit truncation marker.
pub fn summary_payload(messages: &[CompactMessage]) -> String {
    summary_payload_inner(messages, false)
}

/// Source-attributed variant for the checkpoint-sources treatment.
pub fn summary_payload_with_sources(messages: &[CompactMessage]) -> String {
    summary_payload_inner(messages, true)
}

fn summary_payload_inner(messages: &[CompactMessage], include_sources: bool) -> String {
    let parts: Vec<String> = messages
        .iter()
        .map(|m| {
            let text = if m.role == "tool" {
                truncate_for_summary(&m.content)
            } else {
                m.content.clone()
            };
            let source = if include_sources {
                m.source_item_id
                    .as_deref()
                    .map(|id| format!(" [source {id:?}]"))
                    .unwrap_or_else(|| " [source: unavailable]".into())
            } else {
                String::new()
            };
            format!("[{}]{source}: {text}", m.role)
        })
        .collect();
    parts.join("\n\n")
}

/// Validate model-produced checkpoint JSON against the source records eligible
/// for attribution. IDs establish provenance only; they do not prove a claim.
pub fn parse_checkpoint(
    response: &str,
    eligible_source_ids: &[String],
) -> Result<Vec<CheckpointEntry>, String> {
    let mut document: CheckpointDocument = serde_json::from_str(response)
        .map_err(|error| format!("invalid checkpoint JSON: {error}"))?;
    if document.entries.is_empty() {
        return Err("checkpoint has no entries".into());
    }
    let eligible: HashSet<&str> = eligible_source_ids.iter().map(String::as_str).collect();
    for entry in &mut document.entries {
        if entry.text.trim().is_empty() {
            return Err("checkpoint entry text is empty".into());
        }
        let all_sources_resolve = !entry.source_item_ids.is_empty()
            && entry
                .source_item_ids
                .iter()
                .all(|id| eligible.contains(id.as_str()));
        let mut seen = HashSet::new();
        entry
            .source_item_ids
            .retain(|id| eligible.contains(id.as_str()) && seen.insert(id.clone()));
        if entry.kind == CheckpointKind::Fact && !all_sources_resolve {
            entry.kind = CheckpointKind::Hypothesis;
        }
    }
    Ok(document.entries)
}

/// Render a validated checkpoint while retaining its epistemic label and any
/// eligible source references.
pub fn render_checkpoint(entries: &[CheckpointEntry]) -> String {
    entries
        .iter()
        .map(|entry| {
            let kind = match entry.kind {
                CheckpointKind::Fact => "fact",
                CheckpointKind::Hypothesis => "hypothesis",
                CheckpointKind::Plan => "plan",
            };
            if entry.source_item_ids.is_empty() {
                format!("[{kind}]: {}", entry.text)
            } else {
                format!(
                    "[{kind}] sources={:?}: {}",
                    entry.source_item_ids, entry.text
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_for_summary(text: &str) -> String {
    let head = crate::cut_chars(text, SUMMARY_TOOL_CAP);
    let tail = &text[head.len()..];
    if tail.is_empty() {
        return text.to_string();
    }
    format!(
        "{head}\n\n[... {} more characters truncated]",
        tail.chars().count()
    )
}

/// Summarizer instruction: structured checkpoint, never a continuation of the
/// conversation. Kept short: it is one extra request per checkpoint.
pub const SUMMARY_PROMPT: &str = "Summarize the conversation above as a context checkpoint for an agent that must \
continue the work. Do not continue the conversation or answer anything in it. Use this exact format:\n\
## Goal\n[the task being accomplished]\n\
## Progress\n[done / in progress / blocked, with exact file paths and commands]\n\
## Key decisions\n[what was decided and why, including errors hit and how they were resolved]\n\
## Next steps\n[the immediate next action]";

/// Independent opt-in prompt for source-attributed checkpoint JSON.
pub const CHECKPOINT_PROMPT: &str = "Summarize the supplied conversation as a compact checkpoint for an agent that must continue the work. Output only a JSON object shaped like {\"entries\":[{\"kind\":\"fact\",\"text\":\"...\",\"source_item_ids\":[\"...\"]}]}. The kind value must be exactly one of fact, hypothesis, or plan. Use facts only for claims tied to the provided eligible user/tool source IDs; copy those IDs exactly. Unsupported or conflicting claims remain hypotheses. Keep useful next actions as plans even when they have no source ID. IDs identify source records but do not prove an interpretation true. Preserve user requirements and unresolved conflicts; do not invent IDs or omit important requirements. Output at least one non-empty entry.";

/// System row of the summarizer request: the summary call must not act.
pub const SUMMARY_SYSTEM: &str =
    "You are a context summarization assistant. Read the conversation and output \
only the structured summary requested; never continue the conversation and never call a tool.";
