//! Budgeted prompt selection. Salvage of ~/rof-harness/src/context/assembler.rs
//! (one budget, dedupe, windowing) + retriever.rs (file map, named files, windows).
//! Rule: select what enters, never summarize the edit surface. Volatile named
//! files are must_include and excluded from mid-layer double delivery: the
//! caller keeps them out of the mid layer, this crate delivers them last.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// Below this many chars a window is too narrow to judge, so a `must_include`
/// windowed item that still does not fit after halvings is emitted at this
/// floor rather than silently cut.
pub const MIN_WINDOW: usize = 2_000;

/// Old observations kept verbatim (SWE-agent collapse-5, +3.0pp over full history).
pub const COLLAPSE_KEEP: usize = 5;

/// Active file window in lines (SWE-agent: 30 lines −3.7pp, full file −5.3pp).
pub const WINDOW_LINES: usize = 100;

const SKIP_DIRS: &[&str] = &["target", ".git", "node_modules", ".hg", ".svn", "baselines"];

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemKey {
    pub path: String,
    pub region: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Fidelity {
    Exact,
    Windowed { anchor: String, cap: usize },
    Drop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextItem {
    pub key: ItemKey,
    pub fidelity: Fidelity,
    pub must_include: bool,
    pub text: String,
}

pub struct ContextAssembler {
    budget: usize,
    items: Vec<ContextItem>,
    volatile_items: Vec<ContextItem>,
    seen: HashSet<ItemKey>,
}

impl ContextAssembler {
    pub fn new(_budget_chars: usize) -> Self {
        Self {
            budget: _budget_chars,
            items: Vec::new(),
            volatile_items: Vec::new(),
            seen: HashSet::new(),
        }
    }

    /// Dedupe by key. Volatile items bypass the mid layer.
    pub fn add(&mut self, _item: ContextItem) {
        self.place(_item, false);
    }

    /// Turn-only evidence (requested files, skill bodies): fitted against the
    /// same budget, delivered last so a re-ask extends the cached prefix.
    /// Callers mark these must_include and keep them out of the mid layer.
    pub fn add_volatile(&mut self, _item: ContextItem) {
        self.place(_item, true);
    }

    fn place(&mut self, item: ContextItem, volatile: bool) {
        if self.seen.contains(&item.key) {
            return;
        }
        self.seen.insert(item.key.clone());
        if volatile {
            self.volatile_items.push(item);
        } else {
            self.items.push(item);
        }
    }

    /// Halving-to-floor fit; oversized must_include narrows, never silently cuts.
    pub fn assemble(&self) -> String {
        self.assemble_inner(self.budget)
    }

    fn shape(&self, item: &ContextItem, remaining: usize) -> Option<String> {
        match &item.fidelity {
            Fidelity::Exact | Fidelity::Drop => {
                let fits = item.text.chars().count() <= remaining;
                if fits || item.must_include {
                    Some(item.text.clone())
                } else {
                    None
                }
            }
            Fidelity::Windowed { anchor, cap } => {
                let mut cap = *cap;
                loop {
                    let w = window_anchored(&item.text, anchor, cap);
                    if w.chars().count() <= remaining {
                        return Some(w);
                    }
                    if cap <= MIN_WINDOW {
                        return if item.must_include { Some(w) } else { None };
                    }
                    cap = (cap / 2).max(MIN_WINDOW);
                }
            }
        }
    }

    fn fit_list(
        &self,
        list: &[ContextItem],
        out: &mut [Option<String>],
        used: &mut usize,
        budget: usize,
    ) {
        for (item, slot) in list.iter().zip(out.iter_mut()) {
            if let Some(text) = self.shape(item, budget.saturating_sub(*used)) {
                *used += format!("--- {}\n{text}\n", item.key.path).chars().count();
                *slot = Some(text);
            }
        }
    }

    fn assemble_inner(&self, budget: usize) -> String {
        let mut used = 0usize;
        let mut stable = vec![None; self.items.len()];
        let mut vol = vec![None; self.volatile_items.len()];
        self.fit_list(&self.items, &mut stable, &mut used, budget);
        self.fit_list(&self.volatile_items, &mut vol, &mut used, budget);
        let mut sections = Vec::new();
        for (item, text) in self
            .items
            .iter()
            .zip(stable.iter())
            .filter_map(|(i, t)| t.as_ref().map(|t| (i, t)))
        {
            sections.push(format!("--- {}\n{text}", item.key.path));
        }
        for (item, text) in self
            .volatile_items
            .iter()
            .zip(vol.iter())
            .filter_map(|(i, t)| t.as_ref().map(|t| (i, t)))
        {
            sections.push(format!("--- {}\n{text}", item.key.path));
        }
        sections.join("\n\n")
    }
}

/// Byte-stable capped path listing for the file map (rides the cached prefix).
/// Sorted, dotfiles and build/VCS dirs skipped, truncated to `max_paths`.
pub fn file_map(_root: &Path, _max_paths: usize) -> Vec<String> {
    let mut v = Vec::new();
    walk(_root, _root, 0, &mut v);
    v.sort();
    v.truncate(_max_paths);
    v
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 8 || out.len() >= 2000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk(root, &p, depth + 1, out);
        } else if let Ok(rel) = p.strip_prefix(root) {
            out.push(rel.to_string_lossy().to_string());
        }
    }
}

/// Whole named file up to cap, never summarized. Caller excludes it from mid.
pub fn named_file_contents(
    _root: &Path,
    _path: &str,
    _cap_chars: usize,
) -> std::io::Result<String> {
    let c = std::fs::read_to_string(_root.join(_path))?;
    Ok(cut_chars(&c, _cap_chars).to_owned())
}

/// Window text on an anchor line with head+tail around it: `cap` chars centred
/// on the anchor substring, head+tail with an elision marker when absent.
pub fn window_anchored(_text: &str, _anchor: &str, _cap_chars: usize) -> String {
    let total = _text.chars().count();
    if total <= _cap_chars {
        return _text.to_string();
    }
    if !_anchor.is_empty() {
        if let Some(pos) = _text.find(_anchor) {
            let at = _text[..pos].chars().count();
            let start = at.saturating_sub(_cap_chars / 2);
            let win: String = _text.chars().skip(start).take(_cap_chars).collect();
            let end = start + _cap_chars.min(total - start);
            return format!("...[chars {start}..{end} of {total}]...\n{win}");
        }
    }
    let half = _cap_chars / 2;
    let head: String = _text.chars().take(half).collect();
    let tail: String = _text.chars().skip(total - half).collect();
    format!(
        "{head}\n...[{} chars elided]...\n{tail}",
        total - _cap_chars
    )
}

/// Char-boundary cut. Never splits a UTF-8 sequence (v1 context_edges lesson).
pub fn cut_chars(_text: &str, _max_chars: usize) -> &str {
    match _text.char_indices().nth(_max_chars) {
        Some((i, _)) => &_text[..i],
        None => _text,
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(path: &str, region: &str, text: &str, fidelity: Fidelity, must: bool) -> ContextItem {
        ContextItem {
            key: ItemKey {
                path: path.into(),
                region: region.into(),
                role: "t".into(),
            },
            fidelity,
            must_include: must,
            text: text.into(),
        }
    }

    #[test]
    fn dedupe_by_key() {
        let mut a = ContextAssembler::new(100_000);
        a.add(mk("s/a.rs", "cur", "impl A {}", Fidelity::Exact, true));
        a.add(mk("s/a.rs", "cur", "impl A {}", Fidelity::Exact, true));
        assert_eq!(a.assemble().matches("impl A {}").count(), 1);
    }

    #[test]
    fn same_path_two_regions_is_not_a_duplicate() {
        let mut a = ContextAssembler::new(100_000);
        a.add(mk("s/a.rs", "head", "pub mod a;", Fidelity::Exact, true));
        a.add(mk("s/a.rs", "tail", "fn main() {}", Fidelity::Exact, true));
        let out = a.assemble();
        assert!(out.contains("pub mod a;") && out.contains("fn main() {}"));
    }

    #[test]
    fn must_include_survives_tiny_budget() {
        let mut a = ContextAssembler::new(10);
        a.add(mk(
            "big.rs",
            "cur",
            &"x".repeat(5000),
            Fidelity::Exact,
            true,
        ));
        a.add(mk(
            "opt.rs",
            "cur",
            &"y".repeat(5000),
            Fidelity::Drop,
            false,
        ));
        let out = a.assemble();
        assert!(out.contains('x') && !out.contains('y'));
    }

    #[test]
    fn windowed_narrows_by_halving_until_it_fits() {
        let mut a = ContextAssembler::new(3000);
        a.add(mk(
            "s/a.rs",
            "cur",
            &"fn anchor_sym() {}\n".repeat(300),
            Fidelity::Windowed {
                anchor: "anchor_sym".into(),
                cap: 4000,
            },
            true,
        ));
        let out = a.assemble();
        assert!(out.contains("anchor_sym"));
        assert!(out.chars().count() < 3000);
    }

    #[test]
    fn oversized_must_include_narrows_to_floor_never_silent_cut() {
        let mut a = ContextAssembler::new(10);
        a.add(mk(
            "s/a.rs",
            "cur",
            &"fn anchor_sym() {}\n".repeat(600),
            Fidelity::Windowed {
                anchor: "anchor_sym".into(),
                cap: 8000,
            },
            true,
        ));
        let out = a.assemble();
        assert!(out.contains("anchor_sym"));
        assert!(out.chars().count() <= MIN_WINDOW + 200);
    }

    #[test]
    fn window_anchored_on_symbol() {
        let text = "a\n".repeat(3000) + "fn target_sym() {}\n" + &"b\n".repeat(3000);
        let w = window_anchored(&text, "target_sym", 2000);
        assert!(w.contains("target_sym") && w.chars().count() <= 2000 + 100);
        let w2 = window_anchored(&text, "absent", 2000);
        assert!(w2.contains("elided"));
    }

    #[test]
    fn cut_chars_respects_multibyte_boundaries() {
        let t = "λ".repeat(10);
        let c = cut_chars(&t, 4);
        assert_eq!(c.chars().count(), 4);
        assert_eq!(c, "λλλλ");
        assert_eq!(cut_chars("abc", 99), "abc");
    }

    #[test]
    fn named_file_cap_returns_whole_small_file_and_cuts_big() {
        let dir = std::env::temp_dir().join(format!("ctx-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("small.rs"), "fn f() {}").unwrap();
        std::fs::write(dir.join("big.rs"), "λ".repeat(5000)).unwrap();
        assert_eq!(
            named_file_contents(&dir, "small.rs", 2000).unwrap(),
            "fn f() {}"
        );
        let big = named_file_contents(&dir, "big.rs", 2000).unwrap();
        assert_eq!(big.chars().count(), 2000);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn msg(role: &str, chars: usize) -> CompactMessage {
        CompactMessage {
            role: role.into(),
            content: "x".repeat(chars),
        }
    }

    #[test]
    fn estimate_is_anchor_plus_chars4_over_the_tail() {
        let msgs = vec![msg("user", 4), msg("assistant", 8)];
        let anchor = UsageAnchor {
            messages: 1,
            input_tokens: 100,
        };
        assert_eq!(estimate_tokens(&msgs, Some(&anchor)), 102);
        // Whole list when nothing settled yet: ceil(12 / 4).
        assert_eq!(estimate_tokens(&msgs, None), 3);
        // An anchor longer than the list is stale, not authoritative.
        let stale = UsageAnchor {
            messages: 9,
            input_tokens: 100,
        };
        assert_eq!(estimate_tokens(&msgs, Some(&stale)), 3);
        // Chars/4 rounds up: a 1-char tail is not free.
        assert_eq!(estimate_tokens(&[msg("user", 1)], None), 1);
    }

    #[test]
    fn compaction_due_is_strict_and_disabled_never_fires() {
        let cfg = CompactionConfig {
            enabled: true,
            frac: 0.5,
            keep_tokens: 20_000,
        };
        assert!(!compaction_due(100_000, 200_000, &cfg), "at the threshold");
        assert!(compaction_due(100_001, 200_000, &cfg), "just above");
        assert!(!compaction_due(99_999, 200_000, &cfg), "just below");
        let off = CompactionConfig {
            enabled: false,
            ..cfg
        };
        assert!(!compaction_due(u64::MAX, 1, &off));
    }

    /// The cut walks back until the kept tail reaches the budget and lands on
    /// a non-tool message; each message here is 40 chars = 10 tokens.
    #[test]
    fn cut_point_keeps_recent_tail_and_never_starts_on_a_tool_result() {
        let msgs = vec![
            msg("user", 4),
            msg("assistant", 40),
            msg("tool", 40),
            msg("assistant", 40),
            msg("tool", 40),
        ];
        // Crossing lands on the assistant at 3: keep [3, 4] = 20 tokens.
        assert_eq!(cut_point(&msgs, 15), Some(3));
        // Crossing lands on the tool result at 4: the cut backs up to the
        // assistant call that produced it instead of orphaning the result.
        assert_eq!(cut_point(&msgs, 10), Some(3));
        assert_eq!(msgs[cut_point(&msgs, 10).unwrap()].role, "assistant");
        // Whole history below the keep budget: nothing to summarize.
        assert_eq!(cut_point(&msgs, 100), None);
        // A lone trailing tool result cannot be cut around at all.
        assert_eq!(cut_point(&[msg("user", 4), msg("tool", 40)], 1), None);
        assert_eq!(cut_point(&[msg("user", 4)], 1), None);
    }

    #[test]
    fn summary_payload_caps_tool_results_only() {
        let payload = summary_payload(&[
            msg("user", 4),
            msg("tool", 2_500),
            CompactMessage {
                role: "assistant".into(),
                content: "short".into(),
            },
        ]);
        assert!(payload.starts_with("[user]: xxxx\n\n[tool]: x"));
        assert!(payload.contains("[... 500 more characters truncated]"));
        assert!(payload.ends_with("[assistant]: short"));
        // Only the tool block is cut: 2500 x's became 2000.
        assert_eq!(payload.matches('x').count(), 4 + SUMMARY_TOOL_CAP);
        let small = summary_payload(&[msg("tool", 10)]);
        assert_eq!(small, format!("[tool]: {}", "x".repeat(10)));
    }
}
