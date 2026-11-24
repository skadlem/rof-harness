use crate::*;
use agent_budget::config_for;
use agent_budget::BudgetConfig;
use agent_budget::BudgetGuard;
use agent_budget::Capability;
use agent_event::{AgentEvent, Emitter, UsageReport};
use agent_log::Item;
use agent_log::ItemKind;
use provider_core::AssistantMessage;
use provider_core::StopReason;
use provider_core::ToolCallRef;
use provider_core::Usage;
use provider_core::{LlmClient, LlmError, Request, Response};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use tool_core::ToolResult;
use tool_core::{
    CallStatus as CoreCallStatus, GrantGate, Invocation as CoreInvocation,
    Registry as CoreRegistry, Tool as CoreTool, ToolCall as CoreToolCall,
    ToolDefinition as CoreToolDef, ToolError as CoreToolError, ToolOutcome as CoreToolOutcome,
};

mod budget;
mod checkpoint_sources;
mod completion;
mod fold;
mod proof;
mod request;
mod run;
mod state;
mod verify;

fn assistant(calls: Vec<ToolCallRef>) -> AssistantMessage {
    AssistantMessage {
        content: "thinking".into(),
        tool_calls: calls,
        thinking: None,
    }
}

fn call(id: &str) -> ToolCallRef {
    ToolCallRef {
        id: id.into(),
        name: "edit".into(),
        args: Value::Null,
    }
}

fn result() -> ToolResult {
    ToolResult {
        content: "ok".into(),
        is_error: false,
    }
}

fn event_order(history: &[AgentEvent]) -> Vec<&'static str> {
    history
        .iter()
        .map(|e| match e {
            AgentEvent::TurnStart { .. } => "TurnStart",
            AgentEvent::MessageStart { .. } => "MessageStart",
            AgentEvent::MessageUpdate { .. } => "MessageUpdate",
            AgentEvent::MessageEnd { .. } => "MessageEnd",
            AgentEvent::ToolStart { .. } => "ToolStart",
            AgentEvent::ToolEnd { .. } => "ToolEnd",
            AgentEvent::TurnEnd { .. } => "TurnEnd",
            AgentEvent::Control(_) => "Control",
            _ => "other",
        })
        .collect()
}

// --- multi-tick run() assembly: fakes + tempdir git repo ---

static RUN_N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn run_tmp(name: &str) -> std::path::PathBuf {
    let n = RUN_N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("rof-run-{name}-{n}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// One scripted provider reply: a response, a typed failure, or a hang
/// (for the cancel races). A dry queue is a transport failure.
enum Scripted {
    Respond(Response),
    Fail(LlmError),
    Hang,
}

struct FakeLlm {
    order: Arc<Mutex<Vec<String>>>,
    queue: Mutex<VecDeque<Scripted>>,
    /// Every outgoing request, for shape assertions.
    requests: Arc<Mutex<Vec<Request>>>,
}

#[async_trait::async_trait]
impl LlmClient for FakeLlm {
    async fn complete(&self, _model: &str, req: &Request) -> Result<Response, LlmError> {
        self.order.lock().unwrap().push("model".into());
        self.requests.lock().unwrap().push(req.clone());
        let next = self.queue.lock().unwrap().pop_front();
        match next {
            Some(Scripted::Respond(r)) => Ok(r),
            Some(Scripted::Fail(e)) => Err(e),
            Some(Scripted::Hang) => {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(text_response("never"))
            }
            None => Err(LlmError::Transport("script empty".into())),
        }
    }
}

fn script_resp(calls: Vec<(&str, &str, serde_json::Value)>, stop: StopReason) -> Scripted {
    Scripted::Respond(response(calls, stop))
}

fn response(calls: Vec<(&str, &str, serde_json::Value)>, stop: StopReason) -> Response {
    Response {
        message: AssistantMessage {
            content: "step".into(),
            tool_calls: calls
                .into_iter()
                .map(|(id, name, args)| provider_core::ToolCallRef {
                    id: id.into(),
                    name: name.into(),
                    args,
                })
                .collect(),
            thinking: None,
        },
        stop,
        usage: Usage {
            input: 10,
            output: 5,
            cache_read: 0,
            reasoning: None,
            cost_usd: Some(0.01),
        },
        latency_ms: 0,
        attempts: 1,
        raw_stop_reason: None,
        retry_usage: None,
    }
}

fn text_resp(text: &str) -> Scripted {
    Scripted::Respond(text_response(text))
}

fn text_response(text: &str) -> Response {
    Response {
        message: AssistantMessage {
            content: text.into(),
            tool_calls: vec![],
            thinking: None,
        },
        stop: StopReason::Stop,
        usage: Usage {
            input: 10,
            output: 5,
            cache_read: 0,
            reasoning: None,
            cost_usd: Some(0.01),
        },
        latency_ms: 0,
        attempts: 1,
        raw_stop_reason: None,
        retry_usage: None,
    }
}

struct WriteFile {
    root: std::path::PathBuf,
}

#[async_trait::async_trait]
impl CoreTool for WriteFile {
    fn definition(&self) -> CoreToolDef {
        CoreToolDef {
            name: "write".into(),
            description: "write a file under the temp root".into(),
            schema: serde_json::json!({
                "type": "object",
                "required": ["path", "content"],
                "additionalProperties": false,
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                }
            }),
        }
    }
    fn prepare(&self, call: &CoreToolCall) -> CoreCallStatus {
        CoreCallStatus::Dispatch(CoreInvocation {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            args: call.args.clone(),
        })
    }
    async fn execute(
        &self,
        inv: CoreInvocation,
        _cancel: CancellationToken,
    ) -> Result<CoreToolOutcome, CoreToolError> {
        let path = inv.args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let content = inv
            .args
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if path.is_empty() {
            return Err(CoreToolError::Failed("bad args".into()));
        }
        std::fs::write(self.root.join(path), content)
            .map_err(|e| CoreToolError::Failed(e.to_string()))?;
        Ok(CoreToolOutcome {
            content: format!("wrote {path}"),
            truncated: false,
            success: true,
        })
    }
}

struct Boom;

#[async_trait::async_trait]
impl CoreTool for Boom {
    fn definition(&self) -> CoreToolDef {
        CoreToolDef {
            name: "boom".into(),
            description: "always fails".into(),
            schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }),
        }
    }
    fn prepare(&self, call: &CoreToolCall) -> CoreCallStatus {
        CoreCallStatus::Dispatch(CoreInvocation {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            args: call.args.clone(),
        })
    }
    async fn execute(
        &self,
        _inv: CoreInvocation,
        _cancel: CancellationToken,
    ) -> Result<CoreToolOutcome, CoreToolError> {
        Err(CoreToolError::Failed("boom went off".into()))
    }
}

struct FakeRead;

#[async_trait::async_trait]
impl CoreTool for FakeRead {
    fn definition(&self) -> CoreToolDef {
        CoreToolDef {
            name: "read".into(),
            description: "read-only probe with a fresh observation each call".into(),
            schema: serde_json::json!({
                "type": "object",
                "required": ["n"],
                "additionalProperties": false,
                "properties": {"n": {"type": "integer"}}
            }),
        }
    }
    fn prepare(&self, call: &CoreToolCall) -> CoreCallStatus {
        CoreCallStatus::Dispatch(CoreInvocation {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            args: call.args.clone(),
        })
    }
    async fn execute(
        &self,
        inv: CoreInvocation,
        _cancel: CancellationToken,
    ) -> Result<CoreToolOutcome, CoreToolError> {
        let n = inv.args.get("n").and_then(|v| v.as_i64()).unwrap_or(-1);
        Ok(CoreToolOutcome {
            content: format!("read {n}"),
            truncated: false,
            success: true,
        })
    }
}

fn run_registry(root: &std::path::Path) -> CoreRegistry {
    let mut r = CoreRegistry::new(Arc::new(GrantGate::new(
        [(
            "agent".to_string(),
            vec!["write".to_string(), "boom".to_string(), "read".to_string()],
        )]
        .into(),
    )));
    r.register(Arc::new(WriteFile {
        root: root.to_path_buf(),
    }));
    r.register(Arc::new(Boom));
    r.register(Arc::new(FakeRead));
    r
}

fn run_kinds(items: &[Item]) -> Vec<&'static str> {
    items
        .iter()
        .map(|i| match &i.kind {
            ItemKind::Header { .. } => "Header",
            ItemKind::TurnStart { .. } => "TurnStart",
            ItemKind::Input { .. } => "Input",
            ItemKind::Assistant { .. } => "Assistant",
            ItemKind::Attempt { .. } => "Attempt",
            ItemKind::ToolCall { .. } => "ToolCall",
            ItemKind::ToolResult { .. } => "ToolResult",
            ItemKind::TurnEnd { .. } => "TurnEnd",
        })
        .collect()
}

struct RecBets {
    order: Arc<Mutex<Vec<String>>>,
}

impl BetsHook for RecBets {
    fn on_post_batch(
        &self,
        _claim: &bets::Claim,
        _hunks: &[(String, bool)],
    ) -> bets::CommitVerdict {
        self.order.lock().unwrap().push("bets".into());
        bets::CommitVerdict::Committed
    }
}

// --- compaction checkpoint (default off) ---

fn compaction(frac: f64, keep_tokens: usize) -> context::CompactionConfig {
    context::CompactionConfig {
        enabled: true,
        frac,
        keep_tokens,
        context_capacity_tokens: Some(2000),
        ..context::CompactionConfig::default()
    }
}

/// A scripted summary response: `input` is what the totals read.
fn summary_resp(text: &str, stop: StopReason, input: u64) -> Scripted {
    let mut r = text_response(text);
    r.stop = stop;
    r.usage.input = input;
    r.usage.cost_usd = Some(0.02);
    Scripted::Respond(r)
}

/// One scripted run (plus optional subsequent turns on the same `LoopState`);
/// hands back everything the checkpoint assertions need.
async fn run_script(
    name: &str,
    queue: Vec<Scripted>,
    cfg: RunConfig,
    budget_tokens: u64,
    followups: Vec<&str>,
    goal: &str,
) -> (Outcome, Arc<Mutex<Vec<Request>>>, LoopState, Emitter) {
    let root = run_tmp(name);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let client = FakeLlm {
        order: Arc::new(Mutex::new(Vec::new())),
        requests: requests.clone(),
        queue: Mutex::new(VecDeque::from(queue)),
    };
    let registry = run_registry(&root);
    let mut state = LoopState::new();
    state.budget = BudgetGuard::new(
        BudgetConfig {
            max_tokens: budget_tokens,
            ..config_for(Capability::UnattendedBatch)
        },
        Instant::now(),
    );
    let mut emitter = Emitter::new();
    let cancel = CancellationToken::new();
    let mut outcome = run(
        &mut state,
        Run {
            provider: &client,
            registry: &registry,
            agent: "agent",
            workdir: &root,
            emitter: &mut emitter,
            bets: &NoBets,
            cfg: cfg.clone(),
        },
        vec![Input::User(goal.into())],
        &cancel,
    )
    .await;
    for f in followups {
        state.reset_for_input();
        outcome = run(
            &mut state,
            Run {
                provider: &client,
                registry: &registry,
                agent: "agent",
                workdir: &root,
                emitter: &mut emitter,
                bets: &NoBets,
                cfg: cfg.clone(),
            },
            vec![Input::User(f.into())],
            &cancel,
        )
        .await;
    }
    let _ = std::fs::remove_dir_all(&root);
    (outcome, requests, state, emitter)
}

fn last_totals(emitter: &Emitter) -> UsageReport {
    emitter
        .history()
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::TurnEnd { usage_totals, .. } => Some(usage_totals.clone()),
            _ => None,
        })
        .expect("a TurnEnd frame")
}

/// One write round billed `input` prompt tokens: the estimate (anchor +
/// tail chars/4) crosses `budget_tokens * frac` at the next step head.
fn write_round(calls: Vec<(&str, &str, serde_json::Value)>, content: &str, input: u64) -> Scripted {
    let mut r = response(calls, StopReason::ToolUse);
    r.message.content = content.into();
    r.usage.input = input;
    Scripted::Respond(r)
}

/// Full event order including the run/turn/error frames `event_order` filters out.
fn full_event_order(history: &[AgentEvent]) -> Vec<&'static str> {
    history
        .iter()
        .map(|e| match e {
            AgentEvent::RunStart { .. } => "RunStart",
            AgentEvent::RunEnd { .. } => "RunEnd",
            AgentEvent::TurnStart { .. } => "TurnStart",
            AgentEvent::TurnEnd { .. } => "TurnEnd",
            AgentEvent::MessageStart { .. } => "MessageStart",
            AgentEvent::MessageUpdate { .. } => "MessageUpdate",
            AgentEvent::MessageEnd { .. } => "MessageEnd",
            AgentEvent::ToolStart { .. } => "ToolStart",
            AgentEvent::ToolEnd { .. } => "ToolEnd",
            AgentEvent::Control(_) => "Control",
            AgentEvent::Error { .. } => "Error",
        })
        .collect()
}

/// Bet A gate (mirror of rof's `Gate`): keep the proven leading prefix.
struct GateBatch;

impl BetsHook for GateBatch {
    fn on_post_batch(&self, claim: &bets::Claim, hunks: &[(String, bool)]) -> bets::CommitVerdict {
        bets::gate_batch_commit(claim, hunks)
    }
}

/// One successful tool round through the claim seam, mirroring run():
/// Dispatch registers name+args, the ok result lands, edits count here
/// (run() owns the counter in production).
fn ok_round(s: &mut LoopState, id: &str, name: &str, args: Value) {
    let outcome = s.step_claim(
        assistant(vec![ToolCallRef {
            id: id.into(),
            name: name.into(),
            args,
        }]),
        StopReason::ToolUse,
        &RunConfig::default(),
    );
    assert!(matches!(outcome, ClaimOutcome::Dispatch(_)));
    assert!(s.record_tool_result(id, result()));
    if name == "edit" || name == "write" {
        s.edits += 1;
    }
}

fn declare(s: &mut LoopState) -> ClaimOutcome {
    s.step_claim(assistant(vec![]), StopReason::Stop, &RunConfig::default())
}
