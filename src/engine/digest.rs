//! Transcript-only compaction (`/compact`): a prose-only round digest.
//!
//! Stage 2 proved a summarizer must never paraphrase file bytes (mid-layer
//! arming cost 8/12 → 2/12, because the mid layer carries the file being
//! edited). The digest therefore admits only prose: verdict feedback,
//! harness rejections, the model's prose answer, skill lines, changed path
//! NAMES, and condensed check output. Retrieval snippets, `file_state`
//! bodies, patch bodies, and `[VERIFIED FILES]` never enter it.

use serde_json::Value;

/// Below this size the digest rides raw; at/above it, one cheap-model call
/// compacts it (same 1200-char floor as the layer summarizer).
pub const MIN_DIGEST_CHARS: usize = 1200;

#[derive(Default)]
pub struct RoundDigest {
    notes: Vec<String>,
}

impl RoundDigest {
    pub fn new() -> Self {
        Self { notes: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    /// One prose-only note per round. `artifact` is mined for prose fields
    /// ONLY (answer, skill lines) — file and patch bodies are never read.
    pub fn note_round(
        &mut self,
        round: u32,
        pass: bool,
        feedback: &str,
        artifact: &Value,
        changed_names: &str,
        checks: &str,
    ) {
        let mut parts = vec![format!(
            "round {round} ({}): {feedback}",
            if pass { "pass" } else { "fail" }
        )];
        let answer = crate::engine::session::answer_of(artifact);
        if !answer.trim().is_empty() && answer != "(none given)" {
            parts.push(format!("answer: {answer}"));
        }
        let skills = crate::engine::session::skill_changes_line(artifact);
        if skills != "none" {
            parts.push(format!("skills: {skills}"));
        }
        if !changed_names.trim().is_empty() {
            parts.push(format!("changed: {changed_names}"));
        }
        if !checks.trim().is_empty() {
            parts.push(format!("checks: {checks}"));
        }
        self.notes.push(parts.join("\n"));
    }

    pub fn render_raw(&self) -> String {
        self.notes.join("\n---\n")
    }

    pub fn needs_compaction(&self) -> bool {
        self.render_raw().chars().count() >= MIN_DIGEST_CHARS
    }
}

/// `ROF_COMPACT=yes/true/1` arms the digest. Default off: unset or anything
/// else leaves prompts byte-identical.
pub fn compact_enabled() -> bool {
    matches!(
        std::env::var("ROF_COMPACT")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "yes" | "true" | "1"
    )
}

/// Head+tail fallback when the summarize call fails: a compaction miss must
/// never fail the round. Char-boundary safe.
pub fn head_tail(text: &str, budget: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= budget {
        return text.to_string();
    }
    let half = budget / 2;
    let head: String = chars[..half].iter().collect();
    let tail: String = chars[chars.len() - half..].iter().collect();
    format!(
        "{head}\n...{} chars truncated...\n{tail}",
        chars.len() - budget
    )
}
