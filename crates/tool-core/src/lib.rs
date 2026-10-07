//! Tool vocabulary + registry gate.

mod gate;
mod registry;
mod text;
mod tool;

pub use gate::{GateDecision, GrantGate, PermissionGate};
pub use registry::Registry;
pub use text::{bound_text, MAX_MODEL_CHARS};
pub use tool::{
    CallStatus, Invocation, Tool, ToolCall, ToolDeclaration, ToolDefinition, ToolError,
    ToolOutcome, ToolResult, TOOL_EDIT, TOOL_EXEC, TOOL_TEST, TOOL_WRITE,
};

#[cfg(test)]
mod tests;
