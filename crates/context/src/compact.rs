// ===== compaction checkpoint (refs-gap-matrix rank 2) =====

/// Tool results in the summarizer payload are cut to this many chars: a
/// checkpoint buys a summary, not a second copy of the transcript.
pub const SUMMARY_TOOL_CAP: usize = 2_000;

/// Compaction checkpoint knobs. `Default` is OFF: until a caller opts in, the
/// collapse-5 request path is byte-identical.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionConfig {
    pub enabled: bool,
    /// Trigger fraction of the token budget (0.7 = checkpoint at 70%).
    pub frac: f64,
    /// Recent tokens kept verbatim (pi `keepRecentTokens`).
    pub keep_tokens: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            frac: 0.7,
            keep_tokens: 20_000,
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

/// Trigger: enabled and the estimate is strictly above `budget_tokens * frac`.
pub fn compaction_due(estimate: u64, budget_tokens: u64, cfg: &CompactionConfig) -> bool {
    cfg.enabled && (estimate as f64) > budget_tokens as f64 * cfg.frac
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
    let parts: Vec<String> = messages
        .iter()
        .map(|m| {
            let text = if m.role == "tool" {
                truncate_for_summary(&m.content)
            } else {
                m.content.clone()
            };
            format!("[{}]: {text}", m.role)
        })
        .collect();
    parts.join("\n\n")
}

fn truncate_for_summary(text: &str) -> String {
    let total = text.chars().count();
    if total <= SUMMARY_TOOL_CAP {
        return text.to_string();
    }
    let head: String = text.chars().take(SUMMARY_TOOL_CAP).collect();
    format!(
        "{head}\n\n[... {} more characters truncated]",
        total - SUMMARY_TOOL_CAP
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

/// System row of the summarizer request: the summary call must not act.
pub const SUMMARY_SYSTEM: &str =
    "You are a context summarization assistant. Read the conversation and output \
only the structured summary requested; never continue the conversation and never call a tool.";
