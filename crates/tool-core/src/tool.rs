use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use tokio_util::sync::CancellationToken;

use crate::text::error_result;

/// Canonical tool names; tool-core owns the vocabulary. tools-std
/// `definition()` names and agent-loop matching use these, never literals.
pub const TOOL_EDIT: &str = "edit";
pub const TOOL_WRITE: &str = "write";
pub const TOOL_TEST: &str = "test";
pub const TOOL_EXEC: &str = "exec";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// Declaration-only view: no executables, derived PartialEq for comparisons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDeclaration {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone)]
pub struct Invocation {
    pub call_id: String,
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub content: String,
    pub truncated: bool,
    #[serde(default)]
    pub success: bool,
}

#[derive(Debug, Clone)]
pub enum ToolError {
    Denied(String),
    Failed(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolError::Denied(m) => write!(f, "denied: {m}"),
            ToolError::Failed(m) => write!(f, "failed: {m}"),
        }
    }
}

impl std::error::Error for ToolError {}

/// `Err` becomes model-visible error content; tools never crash the run.
impl From<ToolError> for ToolResult {
    fn from(e: ToolError) -> Self {
        error_result(e.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

/// Decision-step outcome: always an answer for the model, never a throw.
#[derive(Debug, Clone)]
pub enum CallStatus {
    Dispatch(Invocation),
    Result(ToolResult),
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    fn prepare(&self, call: &ToolCall) -> CallStatus;
    async fn execute(
        &self,
        inv: Invocation,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError>;
}
