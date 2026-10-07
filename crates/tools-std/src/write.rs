use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tool_core::{
    CallStatus, Invocation, Tool, ToolCall, ToolDefinition, ToolError, ToolOutcome, TOOL_WRITE,
};

use crate::common::{dispatch, parse_args, path_err, EDIT_FILE_CAP, EDIT_REPLACE_CAP};
use crate::policy::{resolve_under, symlink_safe, Policy, ToolPathError};
use crate::runner::check_syntax;

pub struct WriteTool {
    policy: Arc<Policy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
}

fn write_schema() -> Value {
    json!({
        "type": "object",
        "required": ["path", "content"],
        "additionalProperties": false,
        "properties": {
            "path": {"type": "string", "maxLength": 4096},
            "content": {"type": "string", "maxLength": 262144}
        }
    })
}

/// Create missing parents one level at a time, re-jailing each level before
/// descending: `create_dir_all` would happily mkdir through a symlink that
/// points out of the root.
fn create_parent_dirs(root: &Path, path: &str) -> Result<(), ToolError> {
    let mut cur = root.to_path_buf();
    for c in Path::new(path)
        .parent()
        .unwrap_or(Path::new(""))
        .components()
    {
        cur.push(c);
        if cur.symlink_metadata().is_err() {
            std::fs::create_dir(&cur)
                .map_err(|e| ToolError::Failed(format!("cannot create {}: {e}", cur.display())))?;
        }
        symlink_safe(root, &cur, true).map_err(path_err)?;
    }
    Ok(())
}

fn resolve_for_write(policy: &Policy, path: &str) -> Result<PathBuf, ToolError> {
    match resolve_under(&policy.root, path, true, &policy.denied_globs) {
        Err(ToolPathError::MissingParent(_)) => {
            create_parent_dirs(&policy.root, path)?;
            resolve_under(&policy.root, path, true, &policy.denied_globs).map_err(path_err)
        }
        other => other.map_err(path_err),
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: TOOL_WRITE.to_string(),
            description: "create or overwrite a whole file".to_string(),
            schema: write_schema(),
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
        let args: WriteArgs = parse_args(&inv.args)?;
        if args.content.len() > EDIT_REPLACE_CAP {
            return Err(ToolError::Failed("content over 256KB cap".to_string()));
        }
        let p = resolve_for_write(&self.policy, &args.path)?;
        if p.is_dir() {
            return Err(ToolError::Failed(
                "refusing to write a directory".to_string(),
            ));
        }
        // Same guard as edit: never clobber a big file with a small stub.
        if std::fs::metadata(&p)
            .map(|m| m.len() > EDIT_FILE_CAP as u64)
            .unwrap_or(false)
        {
            return Err(ToolError::Failed("file over 512KB cap".to_string()));
        }
        if let Some(argv) = self.policy.syntax_cmd.clone() {
            if !argv.is_empty() {
                check_syntax(&self.policy, &args.content, &cancel).await?;
            }
        }
        // New files are created with `create_new`: the create is atomic, so
        // a file that appears between the checks above and the open fails
        // instead of being truncated. An existing path keeps the plain
        // overwrite (TOCTOU remains: a symlink swapped in after `resolve`
        // is still followed; closing that needs O_NOFOLLOW/dirfd handling
        // the std portables do not offer).
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
        {
            Ok(mut f) => {
                use std::io::Write as _;
                f.write_all(args.content.as_bytes())
                    .map_err(|e| ToolError::Failed(e.to_string()))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                std::fs::write(&p, &args.content).map_err(|e| ToolError::Failed(e.to_string()))?;
            }
            Err(e) => return Err(ToolError::Failed(e.to_string())),
        }
        Ok(ToolOutcome {
            content: format!("wrote {}", args.path),
            truncated: false,
            success: true,
        })
    }
}

pub fn write_tool(policy: Arc<Policy>) -> WriteTool {
    WriteTool { policy }
}
