//! The skill tools, behind the same `ToolRegistry::call` gate as every other
//! tool. They address skills by *name*, not by path, so they pass no path to
//! the gate: the (agent, tool) grant decides who may call them, and
//! `SkillManager` enforces the rest (name charset, containment under the
//! skills root, write policy). The skills root is deliberately not on
//! `allowed_dirs` — see `ToolRegistry::with_defaults`.

use super::{Tool, ToolError, ToolOutput};
use crate::skills::{SkillError, SkillManager, SkillOp};
use async_trait::async_trait;
use std::sync::Arc;

fn failed(e: SkillError) -> ToolError {
    match e {
        // A policy refusal is a denial, not a failure: the trace and the
        // reviewer should be able to tell them apart.
        SkillError::Denied(m) => ToolError::Denied(m),
        other => ToolError::Failed(other.to_string()),
    }
}

/// `skills.list` — the index: names + descriptions, nothing else. Cheap enough
/// to hand to every agent, which is the point of progressive disclosure.
pub struct SkillsListTool {
    manager: Arc<SkillManager>,
}

impl SkillsListTool {
    pub fn new(manager: Arc<SkillManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for SkillsListTool {
    fn name(&self) -> &'static str {
        "skills.list"
    }
    async fn exec(&self, _input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let scan = self.manager.scan();
        let index = self.manager.index(crate::skills::INDEX_MAX);
        Ok(ToolOutput {
            ok: true,
            output: serde_json::json!({
                "index": index,
                "skills": scan.skills.iter().map(|s| serde_json::json!({
                    "name": s.name,
                    "description": s.description,
                    "source": s.source,
                })).collect::<Vec<_>>(),
                "warnings": scan.warnings,
                "root": self.manager.root().to_string_lossy(),
                "policy": format!("{:?}", self.manager.policy()).to_lowercase(),
            })
            .to_string(),
            error: None,
        })
    }
}

/// `skills.view` — one skill's body, or one of its support files.
pub struct SkillsViewTool {
    manager: Arc<SkillManager>,
}

impl SkillsViewTool {
    pub fn new(manager: Arc<SkillManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for SkillsViewTool {
    fn name(&self) -> &'static str {
        "skills.view"
    }
    async fn exec(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Failed("missing name".to_string()))?;
        let file = input.get("file").and_then(|v| v.as_str());
        let v = self.manager.view(name, file).map_err(failed)?;
        Ok(ToolOutput {
            ok: true,
            output: serde_json::json!({
                "name": v.meta.name,
                "description": v.meta.description,
                "file": v.file,
                "body": v.body,
                "files": v.files,
                "source": v.meta.source,
            })
            .to_string(),
            error: None,
        })
    }
}

/// `skills.manage` — create / patch / write_file / delete, under the write
/// policy. `Propose` (default) validates the op and writes a proposal; nothing
/// reaches the skill store until a human approves it.
pub struct SkillsManageTool {
    manager: Arc<SkillManager>,
}

impl SkillsManageTool {
    pub fn new(manager: Arc<SkillManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for SkillsManageTool {
    fn name(&self) -> &'static str {
        "skills.manage"
    }
    async fn exec(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        // The op is tagged on "op"; the caller adds who proposed it and why.
        // A model-supplied `agent` is ignored: the harness overwrites it before
        // the call, so the record always names the real caller.
        let op: SkillOp = serde_json::from_value(input.clone())
            .map_err(|e| ToolError::Failed(format!("bad skill op: {e}")))?;
        let agent = input
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let rationale = input
            .get("rationale")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let change = self.manager.manage(op, agent, rationale).map_err(failed)?;
        Ok(ToolOutput {
            ok: true,
            output: serde_json::to_string(&change).unwrap_or_default(),
            error: None,
        })
    }
}
