use crate::agents::Verdict;
use crate::config::AppConfig;
use crate::context::{Assembly, ContextAssembler, ContextItem, CtxState, Fidelity, ItemKey};
use crate::llm::{ContextService, ExecutorService};
use crate::obs::{TraceEvent, TraceSink};
use crate::tools::ToolRegistry;
use std::sync::Arc;

/// The widest window one evidence file is given before the budget forces it
/// narrower. Larger than a requested file's cap: the reviewer judges a whole
/// change, not one symbol.
const EVIDENCE_WINDOW: usize = 24_000;

/// One touched file's text as the implementer left it, plus the region the
/// reviewer's window should centre on.
struct Carried {
    path: String,
    content: String,
    anchor: String,
}

/// The `[VERIFIED FILES]` block plus what the assembler decided about it.
/// `excess` is empty when every touched file fit; the names are the files the
/// reviewer is judging without seeing them, which is a different failure from
/// a file that merely got a narrower window. `artifact` is the artifact with
/// its file bodies removed: `file_state` keeps the path and the anchor, and
/// `[VERIFIED FILES]` carries the text once, so the JSON is not the second
/// copy of it. `eliminated` counts both.
#[derive(Debug, Clone, Default)]
pub struct Evidence {
    pub block: String,
    pub artifact: String,
    pub eliminated: usize,
    pub excess: Vec<String>,
}

/// One user goal execution. Holds the layered context others read/write.
pub struct Session {
    pub id: String,
    pub goal: String,
    pub ctx: CtxState,
    /// Allowlisted commands the reviewer runs as acceptance evidence.
    pub checks: Vec<String>,
    /// Whether this goal requires file changes (guards against passing empty work).
    pub expect_writes: bool,
    /// Per-task token override; None = config default.
    pub max_tokens: Option<u64>,
}

impl Session {
    pub fn new(goal: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            ctx: CtxState {
                long_term: "conventions: small diffs, cargo test must pass".to_string(),
                mid_term: String::new(),
                short_term: String::new(),
            },
            goal,
            checks: Vec::new(),
            expect_writes: true,
            max_tokens: None,
        }
    }

    pub fn with_checks(mut self, checks: Vec<String>) -> Self {
        self.checks = checks;
        self
    }

    /// Analysis-only goals set this to false; coding goals leave it at "yes".
    pub fn with_expect_writes(mut self, expect: bool) -> Self {
        self.expect_writes = expect;
        self
    }

    pub fn with_token_limit(mut self, limit: Option<u64>) -> Self {
        self.max_tokens = limit;
        self
    }

    pub fn expecting_writes(mut self, yes: bool) -> Self {
        self.expect_writes = yes;
        self
    }
}

/// One configured check and its outcome. §4.3: the verdict used to be a
/// substring match on a log (`checks_log.contains("STATUS: FAILED")`); a
/// structured result lets a mode decide pass by field, and lets `compare`
/// say *which* check flipped for a task that moved.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CheckResult {
    /// The command as configured (the name a report row is keyed on).
    pub name: String,
    pub passed: bool,
    /// Condensed stdout/stderr — the same text the prompt log carried.
    pub output: String,
}

/// The shared services both execution modes hold. §4.3 exists because the
/// skill index, the goal-quality note, auto-poke, the check runner and the
/// reviewer's file evidence all lived in `run_loop` only, so direct mode
/// silently drifted out of every one of them. Moving the methods here makes
/// "which prompt part does a mode forget" a compile error: a mode that wants
/// a thing must call the shared method, and there is exactly one of each.
///
/// Borrows only — the orchestrator owns the services for the run's lifetime.
pub struct RoundServices<'a> {
    pub cfg: &'a AppConfig,
    pub trace: &'a Arc<TraceSink>,
    pub context: &'a ContextService,
    pub executor: &'a ExecutorService,
    pub verify: &'a ExecutorService,
    pub tools: &'a ToolRegistry,
}

impl<'a> RoundServices<'a> {
    /// Runs the session's allowlisted checks through the tool gate and
    /// returns one structured result each (empty when none configured).
    /// The rendered log a prompt shows is [`render_checks`] on the result.
    pub async fn run_checks(
        &self,
        session: &Session,
        workdir: &std::path::Path,
    ) -> Vec<CheckResult> {
        let mut out = Vec::with_capacity(session.checks.len());
        for cmd in &session.checks {
            let (r, lat) = self
                .tools
                .call(
                    "reviewer",
                    "proc.run",
                    Some(workdir),
                    serde_json::json!({ "cmd": cmd }),
                )
                .await;
            self.trace.emit(TraceEvent::ToolCall {
                agent: "reviewer".to_string(),
                tool: "proc.run".to_string(),
                ok: r.is_ok(),
                latency_ms: lat,
            });
            out.push(match r {
                Ok(o) => {
                    // State the outcome explicitly. A passing build prints only
                    // progress lines, which condensation drops — the reviewer
                    // must never have to infer "passed" from an empty body.
                    let code = o.error.clone().unwrap_or_else(|| "exit 0".to_string());
                    CheckResult {
                        name: cmd.clone(),
                        passed: o.ok,
                        output: format!(
                            "$ {cmd}\nSTATUS: {} ({code})\n{}\n",
                            if o.ok { "PASSED" } else { "FAILED" },
                            condense_output(&o.output)
                        ),
                    }
                }
                Err(e) => CheckResult {
                    name: cmd.clone(),
                    passed: false,
                    output: format!("$ {cmd}\nSTATUS: FAILED ({e})\n"),
                },
            });
        }
        out
    }

    /// The skill index as `agent` may see it. Empty when the grant does not
    /// cover `skills.list`, when the store is empty, or when the tool fails —
    /// an agent that may not list skills simply gets no `[SKILLS]` block.
    /// Emits `SkillOp{op: "list"}` only when there was something to deliver.
    pub async fn skill_index(&self, agent: &str) -> SkillIndex {
        let (r, _) = self
            .tools
            .call(agent, "skills.list", None, serde_json::json!({}))
            .await;
        let Ok(o) = r else {
            return SkillIndex::default();
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&o.output) else {
            return SkillIndex::default();
        };
        let text = v
            .get("index")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string();
        let names: Vec<String> = v
            .get("skills")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| Some(s.get("name")?.as_str()?.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if !text.trim().is_empty() {
            self.trace.emit(TraceEvent::SkillOp {
                agent: agent.to_string(),
                op: "list".to_string(),
                name: String::new(),
                ok: true,
                bytes: text.len() as u64,
            });
        }
        SkillIndex { text, names }
    }

    /// Bodies of the skills `text` names, as `agent`, through the gate. This is
    /// the reuse path: the task says what it wants by name and the recorded
    /// procedure arrives, the same way the retriever hands over a named file.
    pub async fn skill_bodies(&self, agent: &str, text: &str, index: &SkillIndex) -> String {
        // Two is already a lot of procedure for one task; more is a prompt
        // stuffed with instructions nobody asked for.
        const MAX_BODIES: usize = 2;
        let mut out = String::new();
        let mut taken = 0usize;
        for name in &index.names {
            if taken >= MAX_BODIES {
                break;
            }
            if !crate::skills::mentions(text, name) {
                continue;
            }
            let (r, _) = self
                .tools
                .call(
                    agent,
                    "skills.view",
                    None,
                    serde_json::json!({ "name": name }),
                )
                .await;
            let body = match r {
                Ok(o) => serde_json::from_str::<serde_json::Value>(&o.output)
                    .ok()
                    .and_then(|v| v.get("body").and_then(|b| b.as_str()).map(str::to_string)),
                Err(_) => None,
            };
            let Some(body) = body else {
                continue;
            };
            taken += 1;
            self.trace.emit(TraceEvent::SkillOp {
                agent: agent.to_string(),
                op: "reuse".to_string(),
                name: name.clone(),
                ok: true,
                bytes: body.len() as u64,
            });
            out.push_str(&format!(
                "\nSKILL {name} (recorded procedure — follow it):\n{}\n",
                body.trim()
            ));
        }
        out
    }

    /// Read touched files through the reviewer's grant after the implementer
    /// The touched files as the reviewer will see them, shaped by the §4.1
    /// assembler: one volatile budget, one window per file centred on what the
    /// artifact did to it, and no second read of text the artifact already
    /// carries. Returns the `[VERIFIED FILES]` block and the chars a duplicate
    /// carried.
    pub async fn reviewer_file_evidence(
        &self,
        workdir: &std::path::Path,
        artifact: &serde_json::Value,
    ) -> Evidence {
        // The implementer already read each touched file after it wrote, so
        // its `file_state` is the tree's current content. Re-reading here was
        // the measured triplicate's worst case: the same bytes, twice in one
        // prompt, at up to 262 KB a file.
        let mut carried = Vec::new();
        if let Some(entries) = artifact.get("file_state").and_then(|v| v.as_array()) {
            for entry in entries {
                let (Some(path), Some(content)) = (
                    entry.get("path").and_then(|v| v.as_str()),
                    entry.get("current_content").and_then(|v| v.as_str()),
                ) else {
                    continue;
                };
                if !carried.iter().any(|c: &Carried| c.path == path) {
                    carried.push(Carried {
                        path: path.to_string(),
                        content: content.to_string(),
                        anchor: entry
                            .get("why")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    });
                }
            }
        }

        let cap = self.cfg.context_policy().short.budget * 4;
        let mut asm = ContextAssembler::new("", cap);
        for c in &carried {
            asm.add(ContextItem {
                key: ItemKey {
                    path: c.path.clone(),
                    region: "current".to_string(),
                    role: "reviewer".to_string(),
                },
                label: format!("--- {}", c.path),
                text: c.content.clone(),
                // ponytail: the anchor the implementer named (`applied` /
                // `rewritten` / the refusal's search) is the region the
                // reviewer is judging, so the window centres on it. A refusal
                // carries the refused search text, which is where the model
                // must look next.
                fidelity: Fidelity::Windowed {
                    anchor: c.anchor.clone(),
                    cap: EVIDENCE_WINDOW,
                },
                must_include: true,
            });
        }
        // Paths the artifact did not carry (a write the gate refused before
        // any read-back) still get independent evidence from the tree.
        for path in self.touched_paths(artifact) {
            if carried.iter().any(|c: &Carried| c.path == path) {
                continue;
            }
            let target = workdir.join(&path);
            let (result, latency_ms) = self
                .tools
                .call(
                    "reviewer",
                    "fs.read",
                    Some(&target),
                    serde_json::json!({"path": path, "max_bytes": 262_144}),
                )
                .await;
            self.trace.emit(TraceEvent::ToolCall {
                agent: "reviewer".to_string(),
                tool: "fs.read".to_string(),
                ok: result.is_ok(),
                latency_ms,
            });
            match result {
                Ok(read) => asm.add(ContextItem {
                    key: ItemKey {
                        path: path.clone(),
                        region: "current".to_string(),
                        role: "reviewer".to_string(),
                    },
                    label: format!("--- {path}"),
                    text: read.output,
                    fidelity: Fidelity::Windowed {
                        anchor: String::new(),
                        cap: EVIDENCE_WINDOW,
                    },
                    must_include: true,
                }),
                Err(error) => asm.add(ContextItem {
                    key: ItemKey {
                        path: path.clone(),
                        region: "unreadable".to_string(),
                        role: "reviewer".to_string(),
                    },
                    label: format!("--- {path}"),
                    text: format!("(unreadable: {error})"),
                    fidelity: Fidelity::Exact,
                    must_include: true,
                }),
            };
        }

        let eliminated = asm.eliminated_chars();
        // `file_state` travels twice in the reviewer's prompt: as JSON under
        // `ARTIFACT:` and as text under `[VERIFIED FILES]`. The block above is
        // the copy the reviewer reads, so the JSON keeps the path and the
        // anchor and drops the body — otherwise the same bytes pay for a
        // second trip through a prompt that already cut room for the first.
        let (artifact, stripped) = strip_file_bodies(artifact);
        match asm.assemble() {
            Assembly::Ok(parts) => Evidence {
                block: parts.tail,
                artifact,
                eliminated: eliminated + stripped,
                excess: Vec::new(),
            },
            // The reviewer would judge a region it cannot see. That is a
            // first-class signal, not a silent head+tail collapse: the parts
            // that fit still go through, and the caller names what did not.
            Assembly::SelectionFailure { parts, excess } => Evidence {
                block: parts.tail,
                artifact,
                eliminated: eliminated + stripped,
                excess: excess.into_iter().map(|i| i.key.path).collect(),
            },
        }
    }

    /// Every path the artifact touched, deduped, whether or not it carried the
    /// file's text.
    fn touched_paths(&self, artifact: &serde_json::Value) -> Vec<String> {
        let mut paths = Vec::new();
        for key in ["file_state", "writes"] {
            if let Some(entries) = artifact.get(key).and_then(|v| v.as_array()) {
                for entry in entries {
                    if let Some(path) = entry.get("path").and_then(|v| v.as_str()) {
                        if !paths.iter().any(|seen: &String| seen == path) {
                            paths.push(path.to_string());
                        }
                    }
                }
            }
        }
        paths.into_iter().take(5).collect()
    }

    /// A pre-check cheaper than the model that will consume the goal. It never
    /// blocks — it emits a trace event and (when enabled) returns a note for
    /// the prompt, because a pre-check that refuses goals would be the harness
    /// claiming a judgement it cannot make. Off by default (`goal_quality`).
    pub fn goal_note(&self, goal: &str) -> Option<String> {
        if !self.cfg.goal_quality {
            return None;
        }
        match crate::eval::goal_quality::check_goal_quality(goal) {
            Some(note) => {
                self.trace.emit(TraceEvent::GoalQuality {
                    goal: goal.to_string(),
                    note: note.clone(),
                });
                Some(note)
            }
            None => None,
        }
    }

    /// Per-task token budget. O(1) spend accounting: the trace sink totals
    /// model-call tokens at the emit choke point ([`TraceSink::total_tokens`]),
    /// so `spent()` is a subtraction and not a rescan of the event stream —
    /// the pre-§4.3 `tokens_since` was O(rounds) per round, once per task.
    pub fn budget(&self, limit: u64) -> Budget {
        Budget::new(self.trace.clone(), limit)
    }

    /// The chars the §4.1 assembler may spend below the layers. The short
    /// layer's cap, because that is where these parts were being silently cut
    /// before there was an owner for them.
    pub fn volatile_budget(&self) -> usize {
        self.cfg.context_policy().short.budget * 4
    }

    /// The stable head a prompt gets: the session's conventions, plus the
    /// skill index when there is one. Byte-stable per task, which is what
    /// keeps it in the provider's cached prefix.
    pub fn head_with_index(base: &str, index: &str) -> String {
        if index.trim().is_empty() {
            return base.to_string();
        }
        format!("{base}\n[SKILLS]\n{index}")
    }
}

/// Per-task token budget over a trace sink. [`Budget::exceeded`] is the one
/// question a round loop asks before spending another expensive round.
pub struct Budget {
    start: u64,
    limit: u64,
    sink: Arc<TraceSink>,
}

impl Budget {
    pub fn new(sink: Arc<TraceSink>, limit: u64) -> Self {
        Self {
            start: sink.total_tokens(),
            limit,
            sink,
        }
    }

    /// Tokens model calls have spent since this budget opened.
    pub fn spent(&self) -> u64 {
        self.sink.total_tokens().saturating_sub(self.start)
    }

    /// `Some(spent)` when a positive limit is crossed, else `None`.
    pub fn exceeded(&self) -> Option<u64> {
        if self.limit > 0 && self.spent() > self.limit {
            Some(self.spent())
        } else {
            None
        }
    }
}

// The counter lives on the sink; `Budget` is plain data a loop can move.

/// What a model may see about skills: the rendered index and the names, so a
/// task's text can be tested against them without re-reading the store.
#[derive(Debug, Default, Clone)]
pub struct SkillIndex {
    pub text: String,
    pub names: Vec<String>,
}

/// The log a prompt shows, from structured results. One source of truth: the
/// verdict reads the fields, the prompt reads this, neither parses the other.
pub fn render_checks(checks: &[CheckResult]) -> String {
    let mut log = String::new();
    for c in checks {
        log.push_str(&c.output);
    }
    log
}

/// A verdict folded from check results — direct mode's decision, without a
/// reviewer in the loop. Every configured check must pass; no checks
/// configured is a pass (the suite's expectation decides what that means).
pub fn checks_pass(checks: &[CheckResult]) -> bool {
    checks.iter().all(|c| c.passed)
}

/// Keeps the lines a reviewer can act on and drops build noise. Raw
/// `cargo test` output is mostly "test x ... ok" lines that the reviewer
/// never uses but which every retry pays for (~2.4k tokens measured).
fn condense_output(raw: &str) -> String {
    const KEEP: [&str; 9] = [
        "FAILED",
        "error",
        "panicked",
        "assertion",
        "warning",
        "test result",
        "failures:",
        "left:",
        "right:",
    ];
    let mut kept: Vec<&str> = Vec::new();
    let mut dropped = 0usize;
    for line in raw.lines() {
        let t = line.trim();
        let noise = t.is_empty()
            || t.starts_with("Compiling")
            || t.starts_with("Finished")
            || t.starts_with("Running")
            || t.starts_with("Doc-tests")
            || t.starts_with("Blocking")
            || (t.starts_with("test ") && t.ends_with("... ok"));
        if noise {
            dropped += 1;
            continue;
        }
        if KEEP.iter().any(|k| t.contains(k)) {
            kept.push(line);
        } else {
            dropped += 1;
        }
    }
    if kept.is_empty() {
        // Silence must be legible: an empty body means the command printed
        // nothing actionable, not that the check failed to run.
        return format!(
            "(no actionable lines in {} lines of output)",
            raw.lines().count()
        );
    }
    let omitted = kept.len().saturating_sub(80);
    let body = kept[..kept.len().min(80)].join("\n");
    if omitted > 0 || dropped > 0 {
        format!("{body}\n[...{dropped} noise lines and {omitted} kept-lines over cap omitted]")
    } else {
        body
    }
}

/// One line for the reviewer: what the implementer did to the skill store.
/// Empty work in the skills channel must be as legible as a zero WRITES MADE.
pub fn skill_changes_line(artifact: &serde_json::Value) -> String {
    let entries = artifact
        .get("skill_changes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if entries.is_empty() {
        return "none".to_string();
    }
    let parts: Vec<String> = entries
        .iter()
        .map(|e| {
            let outcome = e.get("outcome").and_then(|v| v.as_str()).unwrap_or("?");
            let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let detail = e.get("detail").and_then(|v| v.as_str()).unwrap_or("");
            format!("{outcome} {name} ({detail})")
        })
        .collect();
    format!("{} - {}", entries.len(), parts.join("; "))
}

/// What the next round needs after a failed attempt: the tool verdicts, plus
/// for a refused patch the file's text. A change that *landed* is reported
/// without its text — §4.2 rolled the tree back to the baseline, so the
/// post-attempt read describes a state the tree no longer has, and a retry that
/// trusted it would skip a change that is gone. Two failures this addresses,
/// both measured: "search string not found" with no file in front of the model
/// makes it guess again, and a retry told an applied edit is in place neither
/// re-applies it nor re-reads the file (duplicate definitions, E0592/E0428).
pub fn file_state_evidence(artifact: &serde_json::Value) -> String {
    // ponytail: one shared budget, first file takes it; split it per file when
    // a run shows two touched files that both need their text.
    const CAP: usize = 12_000;
    let mut out = String::new();
    let entries = artifact
        .get("file_state")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    // One entry per path, the LAST one: a file patched twice in an artifact has
    // two reads, and the earlier one describes a state the model already moved
    // past — keeping it would hand the retry a stale file and a fresh-looking
    // label (measured: the duplicate-definition class returning at 3 rounds).
    let mut last: Vec<serde_json::Value> = Vec::new();
    for e in entries {
        let path = e.get("path").and_then(|v| v.as_str()).unwrap_or("?");
        if let Some(slot) = last
            .iter_mut()
            .find(|k| k.get("path").and_then(|v| v.as_str()) == Some(path))
        {
            *slot = e;
        } else {
            last.push(e);
        }
    }
    for e in last {
        let left = CAP.saturating_sub(out.len());
        if left == 0 {
            break;
        }
        let path = e.get("path").and_then(|v| v.as_str()).unwrap_or("?");
        let why = e.get("why").and_then(|v| v.as_str()).unwrap_or("changed");
        if why == "applied" || why == "rewritten" {
            // The text is deliberately not included: the harness restored the
            // baseline, so this content is not on disk. `retrieved:` and a
            // fresh read show the file as it is.
            out.push_str(&format!(
                "\nFILE {path} ({why} by your previous round, then ROLLED BACK by the harness: \
                 the tree is back to its baseline and the change is NOT in it now — re-read \
                 the file and re-apply the change if the task still needs it).\n"
            ));
        } else {
            let text = e
                .get("current_content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // A refused patch changed no file, so its read survived the
            // rollback and is still the tree's content.
            let win: String = text.chars().take(left).collect();
            out.push_str(&format!(
                "\nPATCH REFUSED for {path}: {why}\n--- {path} (current content, read back) ---\n{win}\n"
            ));
        }
    }
    out
}

/// A verdict from a shape the reviewer returns, or a harness refusal when the
/// JSON is not the expected shape. Kept here so both callers fold the same
/// failure mode instead of each inventing one.
pub fn parse_verdict(data: &serde_json::Value, fallback_feedback: &str) -> Verdict {
    match serde_json::from_value::<Verdict>(data.clone()) {
        Ok(v) => v,
        Err(_) => Verdict {
            pass: false,
            feedback: if fallback_feedback.is_empty() {
                "bad verdict shape".to_string()
            } else {
                fallback_feedback.to_string()
            },
        },
    }
}

/// The artifact with every `file_state` body removed, keeping the path and the
/// anchor (`why`). Returns the JSON and the chars that no longer travel twice.
/// A reviewer that needs the text reads `[VERIFIED FILES]`, which is the same
/// text once, windowed and budgeted.
fn strip_file_bodies(artifact: &serde_json::Value) -> (String, usize) {
    let mut slim = artifact.clone();
    let mut stripped = 0usize;
    if let Some(states) = slim
        .pointer_mut("/file_state")
        .and_then(|v| v.as_array_mut())
    {
        for entry in states {
            if let Some(body) = entry.get("current_content").and_then(|v| v.as_str()) {
                stripped += body.chars().count();
                if let Some(obj) = entry.as_object_mut() {
                    obj.remove("current_content");
                }
            }
        }
    }
    (serde_json::to_string(&slim).unwrap_or_default(), stripped)
}

#[cfg(test)]
mod condense_tests {
    use super::condense_output;

    #[test]
    fn keeps_failures_drops_noise() {
        let raw = "Compiling rof v0.1.0\nFinished test profile\nRunning tests/a.rs\ntest a ... ok\ntest b ... FAILED\nassertion `left == right` failed\nleft: 1\nright: 2\ntest result: FAILED. 1 passed; 1 failed\n";
        let out = condense_output(raw);
        assert!(out.contains("test b ... FAILED"));
        assert!(out.contains("left: 1"));
        assert!(out.contains("test result: FAILED"));
        assert!(!out.contains("test a ... ok"));
        assert!(!out.contains("Compiling"));
        assert!(out.len() < raw.len());
    }

    #[test]
    fn green_run_collapses_to_summary() {
        let raw = "test a ... ok\ntest b ... ok\ntest result: ok. 2 passed\n";
        let out = condense_output(raw);
        assert!(out.contains("test result: ok"));
        assert!(!out.contains("test a ... ok"));
    }

    #[test]
    fn empty_body_says_so() {
        // A green `cargo check` prints only progress lines: the condensed
        // body must read as "nothing to report", not as a blank.
        let out = condense_output("   Compiling rof v0.1.0\n    Finished dev profile\n");
        assert!(out.contains("no actionable lines"), "{out}");
    }
}

#[cfg(test)]
mod file_state_tests {
    use super::file_state_evidence;

    #[test]
    fn the_last_read_of_a_path_wins() {
        // Two patches to one file: the retry must get the second (current)
        // read, not the first. Here the second is a refusal, so its text is
        // the one the retry sees — the stale applied read is dropped, and
        // with §4.2 an applied read carries no text at all (its change was
        // rolled back).
        let artifact = serde_json::json!({
            "file_state": [
                {"path": "src/a.rs", "why": "applied", "current_content": "OLD"},
                {"path": "src/a.rs", "why": "search string not found", "current_content": "NEW"},
            ]
        });
        let out = file_state_evidence(&artifact);
        assert!(out.contains("NEW"), "{out}");
        assert!(!out.contains("OLD"), "{out}");
        assert!(out.contains("PATCH REFUSED for src/a.rs"), "{out}");
    }

    #[test]
    fn an_applied_change_is_reported_as_rolled_back_without_its_text() {
        // §4.2: the tree went back to the baseline, so the post-attempt text
        // is not on disk and must not be handed to the retry as if it were.
        let artifact = serde_json::json!({
            "file_state": [{"path": "src/a.rs", "why": "applied", "current_content": "LANDED"}]
        });
        let out = file_state_evidence(&artifact);
        assert!(out.contains("ROLLED BACK"), "{out}");
        assert!(!out.contains("LANDED"), "{out}");
    }

    #[test]
    fn separate_files_each_keep_their_evidence() {
        // Applied files get the rollback line; a refused patch still hands over
        // its text — the attempt changed nothing there, so the read survived.
        let artifact = serde_json::json!({
            "file_state": [
                {"path": "src/a.rs", "why": "applied", "current_content": "AAA"},
                {"path": "src/b.rs", "why": "search string not found", "current_content": "BBB"},
            ]
        });
        let out = file_state_evidence(&artifact);
        assert!(out.contains("ROLLED BACK"), "{out}");
        assert!(out.contains("BBB"), "{out}");
        assert!(!out.contains("AAA"), "{out}");
    }
}

#[cfg(test)]
mod evidence_tests {
    use super::strip_file_bodies;
    use serde_json::json;

    #[test]
    fn file_bodies_are_stripped_but_paths_and_anchors_stay() {
        let artifact = json!({
            "result": {"patches": [{"path": "a.rs", "search": "old", "replace": "new"}]},
            "file_state": [
                {"path": "a.rs", "why": "applied", "current_content": "fn new() {}"},
                {"path": "b.rs", "why": "search not found", "current_content": "untouched"},
            ],
            "writes": [],
        });
        let (slim, stripped) = strip_file_bodies(&artifact);
        assert_eq!(stripped, "fn new() {}".len() + "untouched".len());
        assert!(!slim.contains("current_content"), "a body survived: {slim}");
        // The reviewer still knows what was touched and where the window sits.
        assert!(slim.contains("a.rs") && slim.contains("b.rs"));
        assert!(slim.contains("applied"));
        // The intent is not duplication: a patch's `replace` stays.
        assert!(slim.contains("\"new\""));
    }

    #[test]
    fn an_artifact_without_file_state_is_unchanged_and_costs_nothing() {
        let artifact = json!({"result": {"artifact": "x"}});
        let (slim, stripped) = strip_file_bodies(&artifact);
        assert_eq!(stripped, 0);
        assert!(slim.contains("artifact"));
    }
}

#[cfg(test)]
mod budget_tests {
    use super::{checks_pass, render_checks, Budget, CheckResult};
    use crate::obs::{TraceEvent, TraceSink};
    use std::sync::Arc;

    #[test]
    fn a_budget_counts_only_what_was_emitted_after_it_opened() {
        let sink = Arc::new(TraceSink::new());
        emit_call(&sink, 100, 50);
        let budget = Budget::new(sink.clone(), 1000);
        assert_eq!(budget.spent(), 0, "the opening calls are not this budget's");
        emit_call(&sink, 200, 75);
        assert_eq!(budget.spent(), 275);
        assert_eq!(budget.exceeded(), None);
        emit_call(&sink, 700, 30);
        assert_eq!(budget.exceeded(), Some(1005));
    }

    #[test]
    fn a_zero_limit_never_exceeds() {
        // `max_tokens_per_task = 0` means "no ceiling", not "nothing allowed".
        let sink = Arc::new(TraceSink::new());
        let budget = Budget::new(sink, 0);
        assert_eq!(budget.exceeded(), None);
    }

    #[test]
    fn checks_pass_reads_fields_not_log_text() {
        // §4.3: the verdict is a field read. A check whose *output* happens to
        // contain the words "STATUS: FAILED" inside a passing body (an error
        // the model fixed, quoted in the log) must not flip the verdict.
        let ok = CheckResult {
            name: "cargo test".into(),
            passed: true,
            output: "previous STATUS: FAILED in a quoted message\n".into(),
        };
        assert!(checks_pass(std::slice::from_ref(&ok)));
        let bad = CheckResult {
            name: "cargo test".into(),
            passed: false,
            output: String::new(),
        };
        assert!(!checks_pass(&[ok.clone(), bad]));
        assert!(checks_pass(&[]), "no checks configured is a pass");
    }

    #[test]
    fn render_checks_round_trips_the_log_a_prompt_shows() {
        let checks = vec![
            CheckResult {
                name: "a".into(),
                passed: true,
                output: "$ a\nSTATUS: PASSED (exit 0)\n".into(),
            },
            CheckResult {
                name: "b".into(),
                passed: false,
                output: "$ b\nSTATUS: FAILED (exit 1)\n".into(),
            },
        ];
        let log = render_checks(&checks);
        assert!(log.contains("STATUS: PASSED"));
        assert!(log.contains("STATUS: FAILED"));
    }

    fn emit_call(sink: &TraceSink, input: u64, output: u64) {
        sink.emit(TraceEvent::ModelCall {
            agent: "x".into(),
            model: "m".into(),
            input_tokens: input,
            output_tokens: output,
            latency_ms: 0,
            cost_usd: None,
            cached_input_tokens: 0,
            attempts: 1,
        });
    }
}
