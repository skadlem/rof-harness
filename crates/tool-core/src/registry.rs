use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::gate::{GateDecision, PermissionGate};
use crate::text::{bound_text, error_result, MAX_MODEL_CHARS};
use crate::tool::{CallStatus, Tool, ToolCall, ToolDeclaration, ToolResult};

pub struct Registry {
    tools: HashMap<String, Arc<dyn Tool>>,
    gate: Arc<dyn PermissionGate>,
}

impl Registry {
    pub fn new(gate: Arc<dyn PermissionGate>) -> Self {
        Self {
            tools: HashMap::new(),
            gate,
        }
    }
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.definition().name.clone(), tool);
    }
    pub fn resolve(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }
    /// Model-facing declarations, name-sorted for a stable wire order.
    pub fn definitions(&self) -> Vec<ToolDeclaration> {
        let mut out: Vec<ToolDeclaration> = self
            .tools
            .values()
            .map(|t| {
                let d = t.definition();
                ToolDeclaration {
                    name: d.name,
                    description: d.description,
                    schema: d.schema,
                }
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
    /// The one door: gate, then resolve, then strict-validate, then `prepare`.
    /// Every early exit is a bounded error `Result` — never dispatched, never thrown.
    pub fn prepare(&self, agent: &str, call: ToolCall) -> CallStatus {
        match self.gate.check(agent, &call.name, &call.args, None) {
            GateDecision::Deny { reason, .. } => {
                return CallStatus::Result(error_result(format!("denied: {reason}")));
            }
            GateDecision::Ask { prompt, .. } => {
                return CallStatus::Result(error_result(format!(
                    "ask denied (non-interactive, no approver): {prompt}"
                )));
            }
            GateDecision::Allow => {}
        }
        let Some(tool) = self.tools.get(&call.name) else {
            return CallStatus::Result(error_result(format!("unknown tool: {}", call.name)));
        };
        let schema = tool.definition().schema.clone();
        if let Err(e) = validate_strict(&schema, &call.args) {
            return CallStatus::Result(error_result(format!("invalid args: {e}")));
        }
        match tool.prepare(&call) {
            CallStatus::Dispatch(inv) => CallStatus::Dispatch(inv),
            CallStatus::Result(r) => CallStatus::Result(ToolResult {
                content: bound_text(&r.content, MAX_MODEL_CHARS),
                ..r
            }),
        }
    }
}

/// Strict subset of JSON Schema over `serde_json` only: object args required,
/// `required`, `additionalProperties: false`, and per-property `type` with no
/// coercion (`"42"` is not an integer). Unknown keywords are ignored.
fn validate_strict(schema: &Value, args: &Value) -> Result<(), String> {
    let obj = args
        .as_object()
        .ok_or_else(|| "args must be an object".to_string())?;
    let Some(def) = schema.as_object() else {
        return Ok(());
    };
    if let Some(Value::Array(req)) = def.get("required") {
        for r in req {
            if let Value::String(k) = r {
                if !obj.contains_key(k) {
                    return Err(format!("missing required property: {k}"));
                }
            }
        }
    }
    if def.get("additionalProperties") == Some(&Value::Bool(false)) {
        let allowed: Vec<&String> = def
            .get("properties")
            .and_then(|p| p.as_object())
            .map(|p| p.keys().collect())
            .unwrap_or_default();
        for k in obj.keys() {
            if !allowed.contains(&k) {
                return Err(format!("unexpected property: {k}"));
            }
        }
    }
    if let Some(Value::Object(props)) = def.get("properties") {
        for (k, sub) in props {
            if let Some(v) = obj.get(k) {
                check_json_type(k, sub, v)?;
            }
        }
    }
    Ok(())
}

fn check_json_type(prop: &str, sub: &Value, v: &Value) -> Result<(), String> {
    let Some(t) = sub.get("type").and_then(|t| t.as_str()) else {
        return Ok(());
    };
    let ok = match t {
        "string" => v.is_string(),
        "integer" => v.as_i64().is_some() || v.as_u64().is_some(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "null" => v.is_null(),
        _ => true,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("property {prop:?} must be {t}"))
    }
}
