use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tool_core::{CallStatus, Invocation, Tool, ToolCall, ToolDefinition, ToolError, ToolOutcome};

use crate::common::{cap_chars, dispatch, parse_args, path_err, OUT_CAP, READ_FILE_CAP};
use crate::policy::{resolve_under, Policy};

pub struct SearchTool {
    policy: Arc<Policy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    max_results: Option<u64>,
}

fn search_schema() -> Value {
    json!({
        "type": "object",
        "required": ["pattern"],
        "additionalProperties": false,
        "properties": {
            "pattern": {"type": "string", "maxLength": 4096},
            "path": {"type": "string", "maxLength": 4096},
            "max_results": {"type": "integer"}
        }
    })
}

fn search_dir(
    dir: &Path,
    root: &Path,
    pat: &str,
    hits: &mut Vec<String>,
    limit: usize,
    skipped: &mut usize,
) {
    if hits.len() >= limit {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if hits.len() >= limit {
            break;
        }
        if e.file_name() == ".git" {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() {
            search_dir(&p, root, pat, hits, limit, skipped);
        } else if ft.is_file() {
            search_file(&p, root, pat, hits, limit, skipped);
        }
    }
}

/// Collect `pat` hits from one file as `rel:line: text`, stopping at `limit`.
/// A direct file search must not share `search_dir`'s sibling scan: siblings
/// would fill `limit` first and silently drop this file's lines.
/// ponytail: literal contains only, no regex; add regex when a task needs it.
fn search_file(
    p: &Path,
    root: &Path,
    pat: &str,
    hits: &mut Vec<String>,
    limit: usize,
    skipped: &mut usize,
) {
    // Size check BEFORE the read: std::fs::read loads the whole file, and an
    // allowlisted exec can mint multi-GB files. Skip (never fail the whole
    // scan); the caller appends one honesty note when anything was skipped.
    match std::fs::metadata(p) {
        Ok(m) if m.len() > READ_FILE_CAP => {
            *skipped += 1;
            return;
        }
        Err(_) => return,
        Ok(_) => {}
    }
    let Ok(data) = std::fs::read(p) else {
        return;
    };
    let rel = p
        .strip_prefix(root)
        .map(|r| r.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_string_lossy().into_owned());
    for (i, line) in String::from_utf8_lossy(&data).lines().enumerate() {
        if line.contains(pat) {
            hits.push(format!("{}:{}: {}", rel, i + 1, line));
            if hits.len() >= limit {
                break;
            }
        }
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "search".to_string(),
            description: "literal substring search over files under the tool root".to_string(),
            schema: search_schema(),
        }
    }
    fn prepare(&self, call: &ToolCall) -> CallStatus {
        dispatch(call)
    }
    async fn execute(
        &self,
        inv: Invocation,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let args: SearchArgs = parse_args(&inv.args)?;
        let base = resolve_under(
            &self.policy.root,
            args.path.as_deref().unwrap_or("."),
            false,
            &self.policy.denied_globs,
        )
        .map_err(path_err)?;
        let limit = args.max_results.unwrap_or(50).clamp(1, 200) as usize;
        let mut hits = Vec::new();
        let mut skipped = 0usize;
        if base.is_file() {
            search_file(
                &base,
                &self.policy.root,
                &args.pattern,
                &mut hits,
                limit,
                &mut skipped,
            );
        } else {
            search_dir(
                &base,
                &self.policy.root,
                &args.pattern,
                &mut hits,
                limit,
                &mut skipped,
            );
        }
        // Fail-loud honesty: skipped oversized files are never silently
        // dropped — exactly one final note line reports the count.
        if skipped > 0 {
            hits.push(format!("[{skipped} file(s) over 64MB skipped]"));
        }
        let (content, truncated) = cap_chars(hits.join("\n"), OUT_CAP);
        Ok(ToolOutcome {
            content,
            truncated,
            success: true,
        })
    }
}

pub fn search_tool(policy: Arc<Policy>) -> SearchTool {
    SearchTool { policy }
}
