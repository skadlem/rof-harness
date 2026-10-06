use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tool_core::{CallStatus, Invocation, Tool, ToolCall, ToolDefinition, ToolError, ToolOutcome};

use crate::common::{cap_chars, dispatch, parse_args, path_err, OUT_CAP, VIEW_CAP};
use crate::policy::{resolve_under, Policy};

/// `max_bytes` narrows the read, never widens it: the output bound is
/// `min(max_bytes, VIEW_CAP)`.
pub(crate) fn view_read_cap(max_bytes: Option<u64>) -> usize {
    max_bytes.unwrap_or(VIEW_CAP as u64).min(VIEW_CAP as u64) as usize
}

pub struct ViewTool {
    policy: Arc<Policy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewArgs {
    path: String,
    #[serde(default)]
    max_bytes: Option<u64>,
    /// 1-indexed first line to read; absent keeps the whole-file byte-capped
    /// read (and is therefore the only path `max_bytes` had before).
    #[serde(default)]
    offset: Option<usize>,
}

fn view_schema() -> Value {
    json!({
        "type": "object",
        "required": ["path"],
        "additionalProperties": false,
        "properties": {
            "path": {"type": "string", "maxLength": 4096},
            "max_bytes": {"type": "integer"},
            "offset": {"type": "integer"}
        }
    })
}

/// Page of whole lines from 1-indexed `offset`: lines that fit the `cap`-byte
/// read cap and `OUT_CAP` chars, plus an exact continuation notice naming the
/// next offset. Only a first line that alone exceeds a budget is cut (the
/// notice then says so instead of promising an exact line boundary).
pub(crate) fn view_page(
    all: &[&str],
    offset: usize,
    cap: usize,
) -> Result<(String, bool), ToolError> {
    let start = offset.max(1) - 1;
    let total = all.len();
    if start >= total {
        return Err(ToolError::Failed(format!(
            "offset {offset} beyond end of file ({total} lines total)"
        )));
    }
    let mut shown: Vec<&str> = Vec::new();
    let (mut bytes, mut chars) = (0usize, 0usize);
    for line in &all[start..] {
        let sep = usize::from(!shown.is_empty());
        if bytes + line.len() + sep > cap || chars + line.chars().count() + sep > OUT_CAP {
            break;
        }
        bytes += line.len() + sep;
        chars += line.chars().count() + sep;
        shown.push(line);
    }
    if shown.is_empty() {
        let (head, _) = cap_chars(all[start].to_string(), cap.min(OUT_CAP));
        return Ok((
            format!(
                "{head}\n[Line {} of {total} exceeds the read cap and was truncated. Use offset={} to continue.]",
                start + 1,
                start + 2
            ),
            true,
        ));
    }
    let body = shown.join("\n");
    if start + shown.len() < total {
        return Ok((
            format!(
                "{body}\n[Showing lines {}-{} of {total}. Use offset={} to continue.]",
                start + 1,
                start + shown.len(),
                start + shown.len() + 1
            ),
            true,
        ));
    }
    Ok((body, false))
}

#[async_trait]
impl Tool for ViewTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "view".to_string(),
            description: "read a file under the tool root, byte-capped; optional 1-indexed offset pages by line".to_string(),
            schema: view_schema(),
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
        let args: ViewArgs = parse_args(&inv.args)?;
        let p = resolve_under(
            &self.policy.root,
            &args.path,
            false,
            &self.policy.denied_globs,
        )
        .map_err(path_err)?;
        let data = std::fs::read(&p).map_err(|e| ToolError::Failed(e.to_string()))?;
        let cap = view_read_cap(args.max_bytes);
        let Some(offset) = args.offset else {
            // No offset: the byte-capped whole-file read, unchanged.
            let n = data.len().min(cap);
            let (content, truncated) =
                cap_chars(String::from_utf8_lossy(&data[..n]).to_string(), OUT_CAP);
            return Ok(ToolOutcome {
                truncated: truncated || data.len() > n,
                content,
                success: true,
            });
        };
        let text = String::from_utf8_lossy(&data);
        let lines: Vec<&str> = text.lines().collect();
        let (content, truncated) = view_page(&lines, offset, cap)?;
        Ok(ToolOutcome {
            content,
            truncated,
            success: true,
        })
    }
}

pub fn view_tool(policy: Arc<Policy>) -> ViewTool {
    ViewTool { policy }
}
