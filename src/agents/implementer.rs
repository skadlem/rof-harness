use super::{Agent, AgentCtx, AgentOutput};
use crate::context::retriever::window_on;
use crate::context::{Assembly, ContextAssembler, ContextItem, Fidelity, ItemKey};
use crate::llm::LlmReq;
use crate::obs::TraceEvent;
use crate::tools::ToolRegistry;
use async_trait::async_trait;

/// The implementer's contract. `reads` is the one way it can *look something
/// up* before writing: emit `{"reads": [...]}` and nothing else, get those files
/// in one bounded extra turn. Without it a model that needs a struct's real
/// field list has to invent one (measured: E0559/E0063 on every arm).
const IMPLEMENTER_SYSTEM: &str = "You are an implementer. Output JSON {artifact, notes, patches?: [{path, search, replace}], writes?: [{path, content}], reads?: [path], skill_views?: [name], skills?: [{op, ...}]}. Prefer patches (one uniquely-matching hunk each, whitespace-tolerant) for edits to existing files; use writes only to create a file or rewrite most of it. A patch `search` MUST be text you have read from that file in this session — if you have not read it, read it first, because a search string you did not see is a guess and the patch is refused; never replace an existing file with a stub or a fragment of it. If you need a fact to be right (a struct's exact fields, an existing helper's signature, an import path), output {\"reads\": [\"path\", ...]} with no patches and no writes: you will get those files and be asked again. Never invent a field list. The prompt's [SKILLS] block lists recorded procedures; if one applies, output {\"skill_views\": [\"name\"]} (nothing else) and it will be handed to you. After a workflow that took more than one round and worked, record the lesson as a skill: {\"skills\": [{\"op\": \"create\", \"name\": \"lowercase-hyphen\", \"description\": \"when to use it, <=60 chars\", \"body\": \"one rule per lesson\", \"rationale\": \"why it generalizes\"}]} — it is stored as a proposal for a human to approve; other ops are patch {name, find, replace}, write_file {name, rel, content}, delete {name}. Propose only what is genuinely reusable. If the context says WRITES REQUIRED: yes, the JSON MUST include a non-empty patches or writes array — prose, reconnaissance notes or plans are not a deliverable and will be rejected. If it says WRITES REQUIRED: no, the artifact must BE the answer — the complete findings with exact quoted strings and a file:line for every claim, never a progress note, a plan to research, or a list of files you still intend to read: there may be no next turn, so an artifact that is not the answer is a failure. Keep both minimal.";
const DIRECT_SYSTEM: &str = "You are a direct coding agent. Complete the user's goal end to end in this repository. Output JSON {artifact, notes, patches?: [{path, search, replace}], writes?: [{path, content}], reads?: [path]}. Make the requested code and test edits now; do not stop at inspection, explanation, or a test run. Use patches for existing files and writes only for new files. A patch search must be copied from a file you read. If you need a file first, request it with reads and your next turn must contain the actual patch or write. Keep the change minimal. The task is not complete until a non-empty patches or writes array is emitted when WRITES REQUIRED is yes.";

/// Implementer: reads repo state through the gated tools, then asks the
/// Executor LLM for an implementation artifact whose `patches[]`/`writes[]`
/// are applied through the same gate. A refused patch comes back with the
/// file's current text, which the orchestrator hands to the retry round.
pub struct ImplementerAgent<'a> {
    llm: &'a crate::llm::ExecutorService,
}

impl<'a> ImplementerAgent<'a> {
    pub fn new(llm: &'a crate::llm::ExecutorService) -> Self {
        Self { llm }
    }

    /// The tree's paths, replacing the old gather(): five arbitrary root files
    /// cost five tool calls and up to 20k chars per round and told the model
    /// nothing it needed, while the read-request turn failed a quarter of the
    /// time on a path the model had simply guessed. A map is ~2k chars,
    /// byte-stable per task, and lets the implementer name what it wants.
    fn file_map(workdir: &std::path::Path) -> String {
        let r = crate::context::Retriever::new(
            workdir.to_path_buf(),
            crate::config::RetrievalConfig::default(),
        );
        r.file_map()
    }

    pub async fn run_direct(&self, ctx: AgentCtx<'_>) -> anyhow::Result<AgentOutput> {
        self.run_with_system(ctx, DIRECT_SYSTEM).await
    }
}

#[async_trait]
impl Agent for ImplementerAgent<'_> {
    fn name(&self) -> &'static str {
        "implementer"
    }
    async fn run(&self, ctx: AgentCtx<'_>) -> anyhow::Result<AgentOutput> {
        self.run_with_system(ctx, IMPLEMENTER_SYSTEM).await
    }
}

impl ImplementerAgent<'_> {
    async fn run_with_system(
        &self,
        ctx: AgentCtx<'_>,
        system: &str,
    ) -> anyhow::Result<AgentOutput> {
        // §4.1: the file map, requested files and skill bodies are appended
        // *after* the layers were budgeted and cut, so they were the largest
        // token consumers in the system with no budget at all. The assembler
        // owns them: one volatile budget, one window per file, and a duplicate
        // request for a path already delivered is refused and counted.
        let map = ctx.workdir.map(Self::file_map).unwrap_or_default();
        let files_seen = map.lines().count();
        let mut asm = ContextAssembler::new(&ctx.view.prompt, ctx.volatile_budget);
        asm.add(ContextItem {
            key: ItemKey {
                path: "<repo-files>".to_string(),
                region: "map".to_string(),
                role: "implementer".to_string(),
            },
            label: "[REPO FILES]".to_string(),
            text: map,
            fidelity: Fidelity::Drop,
            must_include: false,
        });
        // §4.1: a file the goal names by path is a whole-file answer, so it
        // rides below the layers where the volatile budget (24k chars) can
        // actually hold it. The mid layer caps at 16k and summarizes above
        // 12.8k, which is why a 20k source file named by the goal used to
        // arrive as head+elided-tail and the task then died at
        // "WRITES MADE is 0".
        //
        // Optional, not must_include: two named files over one volatile
        // budget must not become a SelectionFailure that delivers neither. A
        // file that does not fit here is still asked for by name later, and
        // then it is must_include and wins priority. Its region is its own so
        // that the assembler's dedupe (which counts an item as placed whether
        // or not it was delivered) never blocks that later request.
        if let Some(workdir) = ctx.workdir {
            let r = crate::context::Retriever::new(
                workdir.to_path_buf(),
                crate::config::RetrievalConfig::default(),
            );
            for (path, content) in r.named_file_contents(&ctx.view.prompt) {
                asm.add(ContextItem {
                    key: ItemKey {
                        path: path.clone(),
                        region: "goal-named".to_string(),
                        role: "implementer".to_string(),
                    },
                    label: format!("--- {path}"),
                    text: content,
                    fidelity: Fidelity::Windowed {
                        anchor: ctx.view.prompt.to_string(),
                        cap: ctx.volatile_budget,
                    },
                    must_include: false,
                });
            }
        }
        let prompt = match asm.assemble() {
            // The map is optional, so either arm delivers the parts that fit.
            Assembly::Ok(parts) | Assembly::SelectionFailure { parts, .. } => parts.full(),
        };
        let mut data = self.ask_with_system(&ctx, &prompt, system).await?;
        // A model that must not guess asks for the files it needs ("reads"),
        // or for a recorded procedure ("skill_views"). One extra turn, never
        // more: the request is only honored when the artifact proposed no
        // change at all, and a second request is ignored because by then the
        // requested text is already in the prompt.
        let wanted = read_requests(&data);
        let wanted_skills = skill_view_requests(&data);
        if (!wanted.is_empty() || !wanted_skills.is_empty())
            && data.get("patches").is_none()
            && data.get("writes").is_none()
        {
            let mut got_any = false;
            for f in self.requested(&ctx, &wanted).await {
                // The label is in the text when the read failed (`--- path`),
                // and otherwise the assembler's own: a requested file is a
                // whole-file answer, so the window falls back to head+tail
                // when no anchor is named.
                let label = if f.content.starts_with("--- ") {
                    String::new()
                } else {
                    format!("--- {}", f.path)
                };
                asm.add(ContextItem {
                    key: ItemKey {
                        path: f.path,
                        region: "requested".to_string(),
                        role: "implementer".to_string(),
                    },
                    label,
                    text: f.content,
                    // A requested file is a whole-file answer, so the cap is
                    // the volatile budget itself: a fixed 12k cap below it
                    // elided the middle of the files the goal names (a 20k
                    // metrics.rs arrived as head+tail with the struct, `Default`,
                    // `fold` and test module in the hole). The model must not
                    // patch text it has not seen, so the hole was a dead end —
                    // and its re-request was deduped. Only a file over budget
                    // takes a window, centred on what the goal names.
                    fidelity: Fidelity::Windowed {
                        anchor: ctx.view.prompt.to_string(),
                        cap: ctx.volatile_budget,
                    },
                    must_include: true,
                });
                got_any = true;
            }
            for s in self.skill_bodies(&ctx, &wanted_skills).await {
                asm.add(ContextItem {
                    key: ItemKey {
                        path: format!("<skill:{}>", s.name),
                        region: "body".to_string(),
                        role: "implementer".to_string(),
                    },
                    label: format!("--- <skill:{}>", s.name),
                    text: s.body,
                    fidelity: Fidelity::Exact,
                    must_include: true,
                });
                got_any = true;
            }
            if got_any {
                let parts = match asm.assemble() {
                    Assembly::Ok(parts) => parts,
                    // A requested file that does not fit even at the narrowest
                    // window: the model asked for it to settle a fact it will
                    // now have to reason without. Named, not silently cut.
                    Assembly::SelectionFailure { parts, excess } => {
                        ctx.trace.emit(TraceEvent::ModelError {
                            agent: "context".to_string(),
                            error: format!(
                                "requested files too large for the volatile budget: {}",
                                excess
                                    .into_iter()
                                    .map(|i| i.key.path)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        });
                        parts
                    }
                };
                data = self
                    .ask_with_system(&ctx, &reask_prompt(&parts.full()), system)
                    .await?;
            }
        }
        // Patches first, then whole-file writes (a write to the same path wins).
        // Both land in `writes` so the reviewer's WRITES MADE count is unchanged.
        // `file_state` carries the file text each touched path has *now*.
        let (mut writes, mut state) = self.apply_patches(&ctx, &data).await;
        let (w2, s2) = self.apply_writes(&ctx, &data).await;
        writes.extend(w2);
        state.extend(s2);
        // Skill ops go through the same registry as every other side effect.
        // In the default `Propose` policy they land as proposals, never as
        // edits to the store.
        let skill_changes = self.apply_skills(&ctx, &data).await;
        Ok(AgentOutput {
            summary: format!(
                "artifact drafted from {files_seen} files, {} writes, {} files re-read, {} skill changes",
                writes.len(),
                state.len(),
                skill_changes.len()
            ),
            data: serde_json::json!({
                "result": data,
                "files_seen": files_seen,
                "writes": writes,
                "file_state": state,
                "skill_changes": skill_changes,
            }),
        })
    }
}

/// A file an artifact asked to read. The content is delivered whole: the
/// §4.1 assembler owns the windowing and the budget, so a requested file can
/// no longer push the prompt past every budget that already cut.
struct ReqFile {
    path: String,
    content: String,
}

/// A skill body an artifact asked to read.
struct ReqSkill {
    name: String,
    body: String,
}

/// The re-ask after `reads`. The requested files are now in context, but
/// nothing in the prompt says so, so a model that was asked to write can
/// answer with `reads` a second time and stop — a deferral that looks like a
/// model that cannot implement, when it only could not tell it already had
/// what it asked for. This marker closes that loop: name what it has and
/// state the obligation. Appended, not prepended, so the goal text the model
/// keys on stays first.
fn reask_prompt(base: &str) -> String {
    format!(
        "{base}\n\n[RE-ASK] The files you requested are now in context above. Do not \
         emit `reads` again: this turn must contain the patches or writes the \
         goal requires."
    )
}

/// The re-ask marker stops a second `reads` deferral, and it must survive
/// being appended to any prompt: a marker that silently vanished, or that
/// reordered the goal text, would leave the deferral loop in place.
#[test]
fn the_reask_marker_names_the_files_and_forbids_another_read() {
    let p = reask_prompt("GOAL: write the tool\n--- src/lib.rs\nfn main() {}");
    assert!(p.contains("[RE-ASK]"), "the marker must be present");
    assert!(p.contains("Do not emit `reads` again"));
    // The goal stays first — the model keys on it.
    assert!(p.starts_with("GOAL: write the tool"));
    // The requested file text survives.
    assert!(p.contains("--- src/lib.rs"));
    assert!(!p.contains("[RE-ASK][RE-ASK]"), "no duplication");
}

/// The files an artifact asked to read.
fn read_requests(data: &serde_json::Value) -> Vec<String> {
    const MAX: usize = 3;
    string_list(data, "reads", MAX)
}

/// Skills an artifact asked to read before it writes.
fn skill_view_requests(data: &serde_json::Value) -> Vec<String> {
    const MAX: usize = 2;
    string_list(data, "skill_views", MAX)
}

fn string_list(data: &serde_json::Value, key: &str, max: usize) -> Vec<String> {
    data.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .take(max)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

impl ImplementerAgent<'_> {
    /// One Executor call, traced.
    async fn ask_with_system(
        &self,
        ctx: &AgentCtx<'_>,
        prompt: &str,
        system: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let resp = self
            .llm
            .complete(LlmReq {
                system: system.to_string(),
                prompt: prompt.to_string(),
                max_tokens: 8192,
                reasoning_off: false,
                reasoning_low: false,
                roomier: false,
            })
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        ctx.trace.emit(TraceEvent::ModelCall {
            agent: if system == DIRECT_SYSTEM {
                "executor"
            } else {
                self.name()
            }
            .to_string(),
            model: self.llm.model.clone(),
            input_tokens: resp.input_tokens,
            output_tokens: resp.output_tokens,
            latency_ms: resp.latency_ms,
            cost_usd: resp.cost_usd,
            cached_input_tokens: resp.cached_input_tokens,
            attempts: resp.attempts,
        });
        Ok(crate::llm::parse_lenient(&resp.text).unwrap_or(serde_json::json!({"raw": resp.text})))
    }

    /// The files an artifact asked to read, through the same policy gate as
    /// every other tool call (a model-chosen path is untrusted input).
    async fn requested(&self, ctx: &AgentCtx<'_>, paths: &[String]) -> Vec<ReqFile> {
        let (Some(tools), Some(workdir)) = (ctx.tools, ctx.workdir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for path in paths {
            let target = workdir.join(path);
            let (r, lat) = tools
                .call(
                    "implementer",
                    "fs.read",
                    Some(&target),
                    serde_json::json!({"path": path, "max_bytes": 262_144}),
                )
                .await;
            ctx.trace.emit(TraceEvent::ToolCall {
                agent: self.name().to_string(),
                tool: "fs.read".to_string(),
                ok: r.is_ok(),
                latency_ms: lat,
            });
            match r {
                // Whole file: the model asked for it to settle a fact, so a
                // head-only cap would answer the wrong question. The window
                // is the assembler's to choose.
                Ok(o) => out.push(ReqFile {
                    path: path.to_string(),
                    content: o.output,
                }),
                Err(e) => out.push(ReqFile {
                    path: path.to_string(),
                    content: format!("--- {path}\n(unreadable: {e})"),
                }),
            }
        }
        out
    }
    /// The skill bodies an artifact asked for, through the same policy gate as
    /// every other read. A refused or unreadable skill comes back as a line the
    /// model can act on rather than as silence.
    async fn skill_bodies(&self, ctx: &AgentCtx<'_>, names: &[String]) -> Vec<ReqSkill> {
        let Some(tools) = ctx.tools else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for name in names {
            let (r, lat) = tools
                .call(
                    "implementer",
                    "skills.view",
                    None,
                    serde_json::json!({ "name": name }),
                )
                .await;
            ctx.trace.emit(TraceEvent::ToolCall {
                agent: self.name().to_string(),
                tool: "skills.view".to_string(),
                ok: r.is_ok(),
                latency_ms: lat,
            });
            match r {
                Ok(o) => {
                    let v: serde_json::Value = serde_json::from_str(&o.output).unwrap_or_default();
                    let body = v.get("body").and_then(|b| b.as_str()).unwrap_or("");
                    ctx.trace.emit(TraceEvent::SkillOp {
                        agent: self.name().to_string(),
                        op: "view".to_string(),
                        name: name.clone(),
                        ok: true,
                        bytes: body.len() as u64,
                    });
                    out.push(ReqSkill {
                        name: name.clone(),
                        body: body.trim().to_string(),
                    });
                }
                Err(e) => out.push(ReqSkill {
                    name: name.clone(),
                    body: format!("--- {name}\n(unavailable: {e})"),
                }),
            }
        }
        out
    }

    /// Applies artifact `skills[]` through the registry — the same gate as
    /// writes. Under the default `Propose` policy nothing here edits the store;
    /// it writes proposals, and the change record travels with the artifact so
    /// the reviewer sees what happened.
    async fn apply_skills(
        &self,
        ctx: &AgentCtx<'_>,
        data: &serde_json::Value,
    ) -> Vec<serde_json::Value> {
        const MAX: usize = 3;
        let Some(tools) = ctx.tools else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let empty = Vec::new();
        let ops = data
            .get("skills")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
        for op in ops.iter().take(MAX) {
            // The harness states who is proposing and why: a model-supplied
            // `agent` is overwritten rather than trusted.
            let rationale = op
                .get("rationale")
                .and_then(|v| v.as_str())
                .or_else(|| data.get("notes").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            let mut input = op.clone();
            if let Some(obj) = input.as_object_mut() {
                obj.insert("agent".to_string(), serde_json::json!("implementer"));
                obj.insert("rationale".to_string(), serde_json::json!(rationale));
            }
            let (r, lat) = tools
                .call("implementer", "skills.manage", None, input)
                .await;
            ctx.trace.emit(TraceEvent::ToolCall {
                agent: self.name().to_string(),
                tool: "skills.manage".to_string(),
                ok: r.is_ok(),
                latency_ms: lat,
            });
            match r {
                Ok(o) => {
                    let change: serde_json::Value =
                        serde_json::from_str(&o.output).unwrap_or_default();
                    let outcome = change
                        .get("outcome")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    // The event names the effect: a direct change is `apply`, a
                    // proposal is `propose`.
                    let op_name = if outcome == "applied" || outcome == "deleted" {
                        "apply"
                    } else {
                        "propose"
                    };
                    let bytes = change.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                    ctx.trace.emit(TraceEvent::SkillOp {
                        agent: self.name().to_string(),
                        op: op_name.to_string(),
                        name: change
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string(),
                        ok: true,
                        bytes,
                    });
                    out.push(change);
                }
                Err(e) => {
                    // A refusal keeps the attempt's label with ok=false, so the
                    // metrics (which count successes only) cannot read a denial
                    // as a change, while the trace still shows the attempt.
                    let name = op
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    ctx.trace.emit(TraceEvent::SkillOp {
                        agent: self.name().to_string(),
                        op: "propose".to_string(),
                        name: name.clone(),
                        ok: false,
                        bytes: 0,
                    });
                    out.push(serde_json::json!({
                        "outcome": "failed",
                        "name": name,
                        "detail": e.to_string(),
                    }));
                }
            }
        }
        out
    }

    /// Applies artifact `patches[]` through the registry, same gate as writes.
    /// Returns `(results, state)`: every path touched comes back with the file's
    /// text as it is *now*, because the mid-term retrieval is a pre-round
    /// snapshot — a retry that only sees it re-applies an edit that already
    /// landed (measured: duplicate definitions, E0592/E0428).
    async fn apply_patches(
        &self,
        ctx: &AgentCtx<'_>,
        data: &serde_json::Value,
    ) -> (Vec<String>, Vec<serde_json::Value>) {
        let (tools, workdir) = match (ctx.tools, ctx.workdir) {
            (Some(t), Some(w)) => (t, w),
            _ => return (Vec::new(), Vec::new()),
        };
        let mut done = Vec::new();
        let mut state = Vec::new();
        let empty = Vec::new();
        let patches = data
            .get("patches")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
        for p in patches.iter().take(5) {
            let (Some(path), Some(search), Some(replace)) = (
                p.get("path").and_then(|v| v.as_str()),
                p.get("search").and_then(|v| v.as_str()),
                p.get("replace").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            let target = workdir.join(path);
            let input = serde_json::json!({"path": path, "search": search, "replace": replace});
            let (r, lat) = tools
                .call("implementer", "fs.patch", Some(&target), input)
                .await;
            ctx.trace.emit(TraceEvent::ToolCall {
                agent: self.name().to_string(),
                tool: "fs.patch".to_string(),
                ok: r.is_ok(),
                latency_ms: lat,
            });
            match r {
                Ok(o) => {
                    done.push(o.output);
                    // The patch landed: hand the retry the file with its own
                    // edit in it, or it will add the same thing twice.
                    state.push(
                        self.file_state(ctx, tools, workdir, path, replace, "applied")
                            .await,
                    );
                }
                Err(e) => {
                    // Refused: hand it the text the search was supposed to match.
                    state.push(
                        self.file_state(ctx, tools, workdir, path, search, &e.to_string())
                            .await,
                    );
                    done.push(format!("FAILED {path}: {e}"));
                }
            }
        }
        (done, state)
    }

    /// The file's current text, windowed on `anchor`, plus why it is being
    /// handed over ("applied" or the tool's refusal).
    async fn file_state(
        &self,
        ctx: &AgentCtx<'_>,
        tools: &ToolRegistry,
        workdir: &std::path::Path,
        path: &str,
        anchor: &str,
        why: &str,
    ) -> serde_json::Value {
        let target = workdir.join(path);
        // Read generously and window in memory: a head-only read would cut out
        // the very anchor the centre-on-the-symbol rule exists to keep.
        let (r, lat) = tools
            .call(
                "implementer",
                "fs.read",
                Some(&target),
                serde_json::json!({"path": path, "max_bytes": 262_144}),
            )
            .await;
        ctx.trace.emit(TraceEvent::ToolCall {
            agent: self.name().to_string(),
            tool: "fs.read".to_string(),
            ok: r.is_ok(),
            latency_ms: lat,
        });
        let current_content = match r {
            Ok(o) => window_on(&o.output, anchor),
            Err(e) => format!("(unreadable: {e})"),
        };
        serde_json::json!({
            "path": path,
            "why": why,
            "current_content": current_content,
        })
    }

    /// Applies artifact `writes[]` through the registry, so the policy gate
    /// (agent grant + allowlist + root containment) checks every write.
    /// Returns `(results, state)` like `apply_patches`.
    async fn apply_writes(
        &self,
        ctx: &AgentCtx<'_>,
        data: &serde_json::Value,
    ) -> (Vec<String>, Vec<serde_json::Value>) {
        let (tools, workdir) = match (ctx.tools, ctx.workdir) {
            (Some(t), Some(w)) => (t, w),
            _ => return (Vec::new(), Vec::new()),
        };
        let mut done = Vec::new();
        let mut state = Vec::new();
        let empty = Vec::new();
        let writes = data
            .get("writes")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
        for w in writes.iter().take(5) {
            let (Some(path), Some(content)) = (
                w.get("path").and_then(|v| v.as_str()),
                w.get("content").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            let target = workdir.join(path);
            let input = serde_json::json!({"path": path, "content": content});
            let (r, lat) = tools
                .call("implementer", "fs.write", Some(&target), input)
                .await;
            ctx.trace.emit(TraceEvent::ToolCall {
                agent: self.name().to_string(),
                tool: "fs.write".to_string(),
                ok: r.is_ok(),
                latency_ms: lat,
            });
            match r {
                Ok(o) => {
                    done.push(o.output);
                    state.push(
                        self.file_state(ctx, tools, workdir, path, content, "rewritten")
                            .await,
                    );
                }
                Err(e) => done.push(format!("FAILED {path}: {e}")),
            }
        }
        (done, state)
    }
}

#[cfg(test)]
mod tests {
    use super::reask_prompt;

    /// The re-ask marker stops a second `reads` deferral, and it must survive
    /// being appended to any prompt: a marker that silently vanished, or that
    /// reordered the goal text, would leave the deferral loop in place.
    #[test]
    fn the_reask_marker_names_the_files_and_forbids_another_read() {
        let p = reask_prompt("GOAL: write the tool\n--- src/lib.rs\nfn main() {}");
        assert!(p.contains("[RE-ASK]"), "the marker must be present");
        assert!(p.contains("Do not emit `reads` again"));
        // The goal stays first — the model keys on it.
        assert!(p.starts_with("GOAL: write the tool"));
        // The requested file text survives.
        assert!(p.contains("--- src/lib.rs"));
        assert!(!p.contains("[RE-ASK][RE-ASK]"), "no duplication");
    }
}
