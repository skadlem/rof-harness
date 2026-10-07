use super::*;
use async_trait::async_trait;
use serde_json::json;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

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
            success: true,
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

#[test]
fn tool_name_consts_match_definitions() {
    // Pinned to the tools-std `definition()` names (tool-core cannot
    // depend on tools-std, so the literals below mirror them byte for
    // byte); any rename must update both sides together.
    assert_eq!(TOOL_EDIT, "edit");
    assert_eq!(TOOL_WRITE, "write");
    assert_eq!(TOOL_TEST, "test");
    assert_eq!(TOOL_EXEC, "exec");
}
