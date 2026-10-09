use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tool_core::{
    CallStatus, Invocation, Tool, ToolCall, ToolDefinition, ToolError, ToolOutcome, TOOL_EDIT,
};

use crate::common::{apply_hunk, dispatch, parse_args, path_err, EDIT_FILE_CAP, EDIT_REPLACE_CAP};
use crate::policy::{resolve_under, Policy};
use crate::runner::check_syntax;

pub struct EditTool {
    policy: Arc<Policy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    search: String,
    replace: String,
}

fn edit_schema() -> Value {
    json!({
        "type": "object",
        "required": ["path", "search", "replace"],
        "additionalProperties": false,
        "properties": {
            "path": {"type": "string", "maxLength": 4096},
            "search": {"type": "string", "maxLength": 65536},
            "replace": {"type": "string", "maxLength": 262144}
        }
    })
}

#[async_trait]
impl Tool for EditTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: TOOL_EDIT.to_string(),
            description: "replace one hunk in a file; match must be unique".to_string(),
            schema: edit_schema(),
        }
    }
    fn prepare(&self, call: &ToolCall) -> CallStatus {
        dispatch(call)
    }
    async fn execute(
        &self,
        inv: Invocation,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let args: EditArgs = parse_args(&inv.args)?;
        if args.replace.len() > EDIT_REPLACE_CAP {
            return Err(ToolError::Failed("replace over 256KB cap".to_string()));
        }
        let p = resolve_under(
            &self.policy.root,
            &args.path,
            true,
            &self.policy.denied_globs,
        )
        .map_err(path_err)?;
        if p.is_dir() {
            return Err(ToolError::Failed(
                "refusing to patch a directory".to_string(),
            ));
        }
        // Metadata check BEFORE the read: read_to_string loads the whole file
        // first, and the 512KB cap exists to bound that load.
        let size = std::fs::metadata(&p)
            .map_err(|e| ToolError::Failed(e.to_string()))?
            .len();
        if size > EDIT_FILE_CAP as u64 {
            return Err(ToolError::Failed("file over 512KB cap".to_string()));
        }
        let original = std::fs::read_to_string(&p).map_err(|e| ToolError::Failed(e.to_string()))?;
        let updated =
            apply_hunk(&original, &args.search, &args.replace).map_err(ToolError::Failed)?;
        if let Some(argv) = self.policy.syntax_cmd.clone() {
            if !argv.is_empty() {
                check_syntax(&self.policy, &updated, &cancel).await?;
            }
        }
        std::fs::write(&p, updated).map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(ToolOutcome {
            content: format!("patched {}", args.path),
            truncated: false,
            success: true,
        })
    }
}

pub fn edit_tool(policy: Arc<Policy>) -> EditTool {
    EditTool { policy }
}
