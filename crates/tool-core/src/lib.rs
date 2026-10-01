//! Tool vocabulary + registry gate. See research/crate-tool-core.md.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Cap applied to every string handed to the model at the loop boundary.
pub const MAX_MODEL_CHARS: usize = 40_000;

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

/// Head+tail bound with omission marker; total never exceeds `max` (chars).
pub fn bound_text(s: &str, max: usize) -> String {
    const MARKER: &str = "\n...[omitted]...\n";
    let n = s.chars().count();
    if n <= max {
        return s.to_owned();
    }
    let m = MARKER.chars().count();
    if max <= m {
        return s.chars().take(max).collect();
    }
    let rest = max - m;
    let head = rest / 2 + rest % 2;
    let tail = rest / 2;
    let h: String = s.chars().take(head).collect();
    let t: String = s.chars().skip(n - tail).collect();
    format!("{h}{MARKER}{t}")
}

fn error_result(content: String) -> ToolResult {
    ToolResult {
        content: bound_text(&content, MAX_MODEL_CHARS),
        is_error: true,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    struct FakeTool {
        def: ToolDefinition,
        log: Option<Arc<Mutex<Vec<String>>>>,
    }

    impl FakeTool {
        fn new(name: &str, schema: Value) -> Self {
            Self {
                def: ToolDefinition {
                    name: name.into(),
                    description: format!("{name} tool"),
                    schema,
                },
                log: None,
            }
        }
        fn logged(mut self, log: Arc<Mutex<Vec<String>>>) -> Self {
            self.log = Some(log);
            self
        }
    }

    #[async_trait]
    impl Tool for FakeTool {
        fn definition(&self) -> ToolDefinition {
            self.def.clone()
        }
        fn prepare(&self, call: &ToolCall) -> CallStatus {
            if let Some(log) = &self.log {
                log.lock().unwrap().push(call.call_id.clone());
            }
            CallStatus::Dispatch(Invocation {
                call_id: call.call_id.clone(),
                name: call.name.clone(),
                args: call.args.clone(),
            })
        }
        async fn execute(
            &self,
            inv: Invocation,
            _cancel: CancellationToken,
        ) -> Result<ToolOutcome, ToolError> {
            Ok(ToolOutcome {
                content: inv.call_id,
                truncated: false,
            })
        }
    }

    struct AllowAll;
    impl PermissionGate for AllowAll {
        fn check(&self, _: &str, _: &str, _: &Value, _: Option<&Path>) -> GateDecision {
            GateDecision::Allow
        }
    }
    struct DenyAll;
    impl PermissionGate for DenyAll {
        fn check(&self, _: &str, _: &str, _: &Value, _: Option<&Path>) -> GateDecision {
            GateDecision::Deny {
                reason: "no".into(),
                fatal: false,
            }
        }
    }
    struct AskAll;
    impl PermissionGate for AskAll {
        fn check(&self, _: &str, _: &str, _: &Value, _: Option<&Path>) -> GateDecision {
            GateDecision::Ask {
                prompt: "ok?".into(),
                display: "ok?".into(),
            }
        }
    }

    fn obj_schema() -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "additionalProperties": false,
            "properties": {
                "path": {"type": "string"},
                "n": {"type": "integer"}
            }
        })
    }

    fn call(id: &str, name: &str, args: Value) -> ToolCall {
        ToolCall {
            call_id: id.into(),
            name: name.into(),
            args,
        }
    }

    fn reg(gate: Arc<dyn PermissionGate>, tools: Vec<FakeTool>) -> Registry {
        let mut r = Registry::new(gate);
        for t in tools {
            r.register(Arc::new(t));
        }
        r
    }

    #[test]
    fn prepare_runs_in_source_order() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let r = reg(
            Arc::new(AllowAll),
            vec![
                FakeTool::new("a", json!({})).logged(log.clone()),
                FakeTool::new("b", json!({})).logged(log.clone()),
            ],
        );
        for (id, name) in [("1", "a"), ("2", "b"), ("3", "a")] {
            assert!(matches!(
                r.prepare("agent", call(id, name, json!({}))),
                CallStatus::Dispatch(_)
            ));
        }
        assert_eq!(*log.lock().unwrap(), vec!["1", "2", "3"]);
    }

    #[test]
    fn gate_deny_is_error_result() {
        let r = reg(Arc::new(DenyAll), vec![FakeTool::new("a", json!({}))]);
        match r.prepare("agent", call("1", "a", json!({}))) {
            CallStatus::Result(res) => assert!(res.is_error),
            CallStatus::Dispatch(_) => panic!("denied call must not dispatch"),
        }
    }

    #[test]
    fn gate_ask_fails_closed_without_answerer() {
        let r = reg(Arc::new(AskAll), vec![FakeTool::new("a", json!({}))]);
        match r.prepare("agent", call("1", "a", json!({}))) {
            CallStatus::Result(res) => {
                assert!(res.is_error);
                assert!(res.content.contains("non-interactive"));
            }
            CallStatus::Dispatch(_) => panic!("ask must fail closed"),
        }
    }

    #[test]
    fn unknown_tool_is_error_result() {
        let r = reg(Arc::new(AllowAll), vec![]);
        match r.prepare("agent", call("1", "nope", json!({}))) {
            CallStatus::Result(res) => {
                assert!(res.is_error);
                assert!(res.content.contains("nope"));
            }
            CallStatus::Dispatch(_) => panic!("unknown tool must not dispatch"),
        }
    }

    #[test]
    fn strict_rejects_coercion_extra_and_missing() {
        let r = reg(Arc::new(AllowAll), vec![FakeTool::new("a", obj_schema())]);
        // "42" for integer: no coercion
        assert!(matches!(
            r.prepare("agent", call("1", "a", json!({"path": "f", "n": "42"}))),
            CallStatus::Result(_)
        ));
        // extra property with additionalProperties: false
        assert!(matches!(
            r.prepare("agent", call("2", "a", json!({"path": "f", "zzz": 1}))),
            CallStatus::Result(_)
        ));
        // missing required
        assert!(matches!(
            r.prepare("agent", call("3", "a", json!({}))),
            CallStatus::Result(_)
        ));
        // non-object args
        assert!(matches!(
            r.prepare("agent", call("4", "a", json!("just text"))),
            CallStatus::Result(_)
        ));
        // valid dispatches
        assert!(matches!(
            r.prepare("agent", call("5", "a", json!({"path": "f", "n": 42}))),
            CallStatus::Dispatch(_)
        ));
    }

    #[test]
    fn bound_text_truncates_head_tail_with_marker() {
        let s: String = (0..100).map(|_| "x").collect();
        let b = bound_text(&s, 20);
        assert!(b.chars().count() <= 20);
        assert!(b.contains("[omitted]"));
        assert!(b.starts_with("xx"));
        assert!(b.ends_with("x"));
        assert_eq!(bound_text("short", 20), "short");
    }

    #[test]
    fn grant_gate_denies_unknown_agent_and_tool() {
        let g = GrantGate::new([("coder".to_string(), vec!["a".to_string()])].into());
        assert!(matches!(
            g.check("coder", "a", &json!({}), None),
            GateDecision::Allow
        ));
        assert!(matches!(
            g.check("coder", "b", &json!({}), None),
            GateDecision::Deny { .. }
        ));
        assert!(matches!(
            g.check("stranger", "a", &json!({}), None),
            GateDecision::Deny { .. }
        ));
    }

    #[test]
    fn tool_error_converts_to_bounded_error_result() {
        let r = ToolResult::from(ToolError::Denied("x".into()));
        assert!(r.is_error && r.content.contains("denied"));
        let big = "y".repeat(MAX_MODEL_CHARS + 10);
        let r2 = ToolResult::from(ToolError::Failed(big));
        assert!(r2.content.chars().count() <= MAX_MODEL_CHARS);
    }
}
