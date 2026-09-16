use super::{Agent, AgentCtx, AgentOutput};
use crate::context::window_on;
use crate::llm::LlmReq;
use crate::obs::TraceEvent;
use crate::tools::ToolRegistry;
use async_trait::async_trait;

/// The implementer's contract. `reads` is the one way it can *look something
/// up* before writing: emit `{"reads": [...]}` and nothing else, get those files
/// in one bounded extra turn. Without it a model that needs a struct's real
/// field list has to invent one (measured: E0559/E0063 on every arm).
const IMPLEMENTER_SYSTEM: &str = "You are an implementer. Output JSON {artifact, notes, patches?: [{path, search, replace}], writes?: [{path, content}], reads?: [path]}. Prefer patches (one uniquely-matching hunk each, whitespace-tolerant) for edits to existing files; use writes only to create a file or rewrite most of it. A patch `search` MUST be text you have read from that file in this session — if you have not read it, read it first, because a search string you did not see is a guess and the patch is refused; never replace an existing file with a stub or a fragment of it. If you need a fact to be right (a struct's exact fields, an existing helper's signature, an import path), output {\"reads\": [\"path\", ...]} with no patches and no writes: you will get those files and be asked again. Never invent a field list. If the context says WRITES REQUIRED: yes, the JSON MUST include a non-empty patches or writes array — prose, reconnaissance notes or plans are not a deliverable and will be rejected. Keep both minimal.";

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
}

#[async_trait]
impl Agent for ImplementerAgent<'_> {
    fn name(&self) -> &'static str {
        "implementer"
    }
    async fn run(&self, ctx: AgentCtx<'_>) -> anyhow::Result<AgentOutput> {
        let map = ctx.workdir.map(Self::file_map).unwrap_or_default();
        let mut prompt = ctx.view.prompt.clone();
        if !map.is_empty() {
            prompt.push_str("\n\n[REPO FILES]\n");
            prompt.push_str(&map);
        }
        let mut data = self.ask(&ctx, &prompt).await?;
        // A model that must not guess asks for the files it needs ("reads").
        // One extra turn, never more: the request is only honored when the
        // artifact proposed no change at all, and a second request is ignored
        // because by then the requested files are already in the prompt.
        let wanted = read_requests(&data);
        if !wanted.is_empty() && data.get("patches").is_none() && data.get("writes").is_none() {
            let got = self.requested(&ctx, &wanted).await;
            if !got.is_empty() {
                prompt.push_str("\n\n[REQUESTED FILES]\n");
                prompt.push_str(&got.join("\n"));
                data = self.ask(&ctx, &prompt).await?;
            }
        }
        let files_seen = map.lines().count();
        // Patches first, then whole-file writes (a write to the same path wins).
        // Both land in `writes` so the reviewer's WRITES MADE count is unchanged.
        // `file_state` carries the file text each touched path has *now*.
        let (mut writes, mut state) = self.apply_patches(&ctx, &data).await;
        let (w2, s2) = self.apply_writes(&ctx, &data).await;
        writes.extend(w2);
        state.extend(s2);
        Ok(AgentOutput {
            summary: format!(
                "artifact drafted from {files_seen} files, {} writes, {} files re-read",
                writes.len(),
                state.len()
            ),
            data: serde_json::json!({
                "result": data,
                "files_seen": files_seen,
                "writes": writes,
                "file_state": state,
            }),
        })
    }
}

/// Paths an artifact asked to see before it writes.
fn read_requests(data: &serde_json::Value) -> Vec<String> {
    const MAX: usize = 3;
    data.get("reads")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .take(MAX)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

impl ImplementerAgent<'_> {
    /// One Executor call, traced.
    async fn ask(&self, ctx: &AgentCtx<'_>, prompt: &str) -> anyhow::Result<serde_json::Value> {
        let resp = self
            .llm
            .complete(LlmReq {
                system: IMPLEMENTER_SYSTEM.to_string(),
                prompt: prompt.to_string(),
                max_tokens: 1200,
            })
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        ctx.trace.emit(TraceEvent::ModelCall {
            agent: self.name().to_string(),
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
    async fn requested(&self, ctx: &AgentCtx<'_>, paths: &[String]) -> Vec<String> {
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
                // Whole file, like a goal-named one: the model asked for it to
                // settle a fact, so a head-only cap would answer the wrong
                // question.
                Ok(o) => out.push(format!("--- {path}\n{}", window_on(&o.output, ""))),
                Err(e) => out.push(format!("--- {path}\n(unreadable: {e})")),
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
