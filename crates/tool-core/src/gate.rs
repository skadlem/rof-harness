use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub enum GateDecision {
    Allow,
    Deny { reason: String, fatal: bool },
    Ask { prompt: String, display: String },
}

pub trait PermissionGate: Send + Sync {
    fn check(&self, agent: &str, tool: &str, args: &Value, path: Option<&Path>) -> GateDecision;
}

/// Default non-interactive gate: grant matrix only. Unknown agent or
/// ungranted tool is denied; `Ask` is never emitted (see `Registry::prepare`,
/// which fails any `Ask` closed since there is no answerer here).
pub struct GrantGate {
    grants: HashMap<String, Vec<String>>,
}

impl GrantGate {
    pub fn new(grants: HashMap<String, Vec<String>>) -> Self {
        Self { grants }
    }
}

impl PermissionGate for GrantGate {
    fn check(&self, agent: &str, tool: &str, _args: &Value, _path: Option<&Path>) -> GateDecision {
        match self.grants.get(agent) {
            None => GateDecision::Deny {
                reason: format!("unknown agent: {agent}"),
                fatal: false,
            },
            Some(tools) if tools.iter().any(|t| t.as_str() == tool) => GateDecision::Allow,
            Some(_) => GateDecision::Deny {
                reason: format!("tool not granted to {agent}: {tool}"),
                fatal: false,
            },
        }
    }
}
