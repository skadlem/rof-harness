use super::{Agent, AgentCtx, AgentOutput};
use crate::llm::LlmReq;
use crate::obs::TraceEvent;
use async_trait::async_trait;

/// Implementer: reads repo state through strict read-only tools, then asks
/// the Executor LLM to produce an implementation artifact (description of
/// the change + notes). v1 never writes; write tools slot in behind the
/// same policy gate later.
pub struct ImplementerAgent<'a> {
    llm: &'a crate::llm::ExecutorService,
}

impl<'a> ImplementerAgent<'a> {
    pub fn new(llm: &'a crate::llm::ExecutorService) -> Self {
        Self { llm }
    }

    async fn gather(&self, ctx: &AgentCtx<'_>) -> Vec<String> {
        let mut seen = Vec::new();
        let (tools, workdir) = match (ctx.tools, ctx.workdir) {
            (Some(t), Some(w)) => (t, w),
            _ => return seen,
        };
        let (list, lat) = tools
            .call(
                "implementer",
                "fs.list",
                Some(workdir),
                serde_json::json!({"path": "."}),
            )
            .await;
        ctx.trace.emit(TraceEvent::ToolCall {
            agent: self.name().to_string(),
            tool: "fs.list".to_string(),
            ok: list.is_ok(),
            latency_ms: lat,
        });
        let entries: Vec<Entry> = list
            .ok()
            .and_then(|o| serde_json::from_str(&o.output).ok())
            .unwrap_or_default();
        for e in entries.into_iter().filter(|e| !e.is_dir).take(5) {
            let name = e.name;
            let target = workdir.join(&name);
            let (r, lat) = tools
                .call(
                    "implementer",
                    "fs.read",
                    Some(&target),
                    serde_json::json!({"path": name, "max_bytes": 4000}),
                )
                .await;
            ctx.trace.emit(TraceEvent::ToolCall {
                agent: self.name().to_string(),
                tool: "fs.read".to_string(),
                ok: r.is_ok(),
                latency_ms: lat,
            });
            if let Ok(out) = r {
                seen.push(format!("--- {name}\n{}", out.output));
            }
        }
        seen
    }
}

#[derive(serde::Deserialize)]
struct Entry {
    name: String,
    #[serde(default)]
    is_dir: bool,
}

#[async_trait]
impl Agent for ImplementerAgent<'_> {
    fn name(&self) -> &'static str {
        "implementer"
    }
    async fn run(&self, ctx: AgentCtx<'_>) -> anyhow::Result<AgentOutput> {
        let files = self.gather(&ctx).await;
        let mut prompt = ctx.view.prompt.clone();
        if !files.is_empty() {
            prompt.push_str("\n\n[REPO SNIPPETS]\n");
            prompt.push_str(&files.join("\n"));
        }
        let req = LlmReq {
            system: "You are an implementer. Output JSON {artifact, notes, patches?: [{path, search, replace}], writes?: [{path, content}]}. Prefer patches (one uniquely-matching hunk each, whitespace-tolerant) for edits to existing files; use writes only to create a file or rewrite most of it. If the context says WRITES REQUIRED: yes, the JSON MUST include a non-empty patches or writes array — prose, reconnaissance notes or plans are not a deliverable and will be rejected. Keep both minimal."
                .to_string(),
            prompt,
            max_tokens: 1200,
        };
        let resp = self
            .llm
            .complete(req)
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
        let data: serde_json::Value =
            crate::llm::parse_lenient(&resp.text).unwrap_or(serde_json::json!({"raw": resp.text}));
        let files_seen = files.len();
        // Patches first, then whole-file writes (a write to the same path wins).
        // Both land in `writes` so the reviewer's WRITES MADE count is unchanged.
        let mut writes = self.apply_patches(&ctx, &data).await;
        writes.extend(self.apply_writes(&ctx, &data).await);
        Ok(AgentOutput {
            summary: format!(
                "artifact drafted from {files_seen} files, {} writes",
                writes.len()
            ),
            data: serde_json::json!({
                "result": data,
                "files_seen": files_seen,
                "writes": writes,
            }),
        })
    }
}

impl ImplementerAgent<'_> {
    /// Applies artifact `patches[]` through the registry, same gate as writes.
    async fn apply_patches(&self, ctx: &AgentCtx<'_>, data: &serde_json::Value) -> Vec<String> {
        let (tools, workdir) = match (ctx.tools, ctx.workdir) {
            (Some(t), Some(w)) => (t, w),
            _ => return Vec::new(),
        };
        let mut done = Vec::new();
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
            done.push(match r {
                Ok(o) => o.output,
                Err(e) => format!("FAILED {path}: {e}"),
            });
        }
        done
    }

    /// Applies artifact `writes[]` through the registry, so the policy gate
    /// (agent grant + allowlist + root containment) checks every write.
    async fn apply_writes(&self, ctx: &AgentCtx<'_>, data: &serde_json::Value) -> Vec<String> {
        let (tools, workdir) = match (ctx.tools, ctx.workdir) {
            (Some(t), Some(w)) => (t, w),
            _ => return Vec::new(),
        };
        let mut done = Vec::new();
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
            done.push(match r {
                Ok(o) => o.output,
                Err(e) => format!("FAILED {path}: {e}"),
            });
        }
        done
    }
}
