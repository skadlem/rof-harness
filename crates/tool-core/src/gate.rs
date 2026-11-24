use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum GateDecision {
    Allow,
    Deny { reason: String },
}

pub trait PermissionGate: Send + Sync {
    fn check(&self, agent: &str, tool: &str, args: &Value) -> GateDecision;
}

/// Default non-interactive gate: grant matrix only. Unknown agent or
/// ungranted tool is denied.
pub struct GrantGate {
    grants: HashMap<String, Vec<String>>,
}

impl GrantGate {
    pub fn new(grants: HashMap<String, Vec<String>>) -> Self {
        Self { grants }
    }
}

/// Exact name, or a trailing-`*` prefix grant (`mcp__*`: tool names learned
/// only after an MCP handshake).
fn granted(grant: &str, tool: &str) -> bool {
    grant == tool || grant.strip_suffix('*').is_some_and(|p| tool.starts_with(p))
}

impl PermissionGate for GrantGate {
    fn check(&self, agent: &str, tool: &str, _args: &Value) -> GateDecision {
        match self.grants.get(agent) {
            None => GateDecision::Deny {
                reason: format!("unknown agent: {agent}"),
            },
            Some(tools) if tools.iter().any(|t| granted(t, tool)) => GateDecision::Allow,
            Some(_) => GateDecision::Deny {
                reason: format!("tool not granted to {agent}: {tool}"),
            },
        }
    }
}
