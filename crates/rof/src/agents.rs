//! `--agents FILE` (stage 1): topology as data. A JSON file names the roles,
//! which tools each gets, and which role may `spawn` which; the model sees
//! `spawn` / `wait` only if its role has an outgoing spawn edge. Stage 1 is a
//! tree of read-only children sharing the parent's workdir. `worktree`
//! workspaces and `message` edges parse but are rejected at load.
//!
//! Children run as futures polled next to the root's run (`with_children`), so
//! they borrow the provider instead of needing it `'static`.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};

use agent_budget::{config_for, with_steps, BudgetGuard, Capability};
use agent_event::Emitter;
use agent_loop::{Input, LoopState, NoBets, Outcome, Run};
use provider_core::LlmClient;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tool_core::{
    bound_text, Invocation, Tool, ToolDefinition, ToolError, ToolOutcome, MAX_MODEL_CHARS,
};

use crate::cli::{run_config, Args};
use crate::workdir::{exclude_sidecar, resolve_log_path};

const TOOLS: [&str; 6] = ["view", "search", "exec", "write", "edit", "test"];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Limits {
    pub max_depth: usize,
    pub max_agents: usize,
    pub child_budget_tokens: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Role {
    tools: Vec<String>,
    budget_tokens: u64,
    budget_steps: Option<u32>,
    prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Config {
    root: String,
    pub limits: Limits,
    roles: BTreeMap<String, Role>,
    /// Spawn edges `(from, to)`; `message` edges are rejected at load.
    edges: Vec<(String, String)>,
}

pub(crate) fn load(path: &str) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("--agents {path}: {e}"))?;
    parse(&text).map_err(|e| format!("--agents {path}: {e}"))
}

fn str_of<'a>(v: &'a Value, key: &str, what: &str) -> Result<&'a str, String> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{what} needs string \"{key}\""))
}

fn num_of(v: &Value, key: &str, default: Option<u64>) -> Result<Option<u64>, String> {
    match v.get(key) {
        None => Ok(default),
        Some(n) => match n.as_u64().filter(|n| *n > 0) {
            Some(n) => Ok(Some(n)),
            None => Err(format!("\"{key}\" must be a positive integer")),
        },
    }
}

pub(crate) fn parse(text: &str) -> Result<Config, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    let root = str_of(&v, "root", "config")?.to_string();
    let limits = v.get("limits").cloned().unwrap_or(json!({}));
    let limits = Limits {
        max_depth: num_of(&limits, "max_depth", Some(1))?.unwrap_or(1) as usize,
        max_agents: num_of(&limits, "max_agents", Some(4))?.unwrap_or(4) as usize,
        child_budget_tokens: num_of(&limits, "child_budget_tokens", Some(120_000))?.unwrap_or(0),
    };
    if v.get("roles").and_then(|r| r.get(&root)).is_none() {
        return Err(format!("root {root:?} is not a role"));
    }
    let mut roles = BTreeMap::new();
    for (name, r) in v
        .get("roles")
        .and_then(Value::as_object)
        .ok_or("config needs object \"roles\"")?
    {
        let what = format!("role {name:?}");
        let tools: Vec<String> = r
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(format!("{what} needs array \"tools\""))?
            .iter()
            .map(|t| t.as_str().map(str::to_owned))
            .collect::<Option<_>>()
            .ok_or(format!("{what}: tools must be strings"))?;
        if let Some(t) = tools.iter().find(|t| !TOOLS.contains(&t.as_str())) {
            return Err(format!("{what} lists unknown tool {t:?}"));
        }
        match r
            .get("workspace")
            .and_then(Value::as_str)
            .unwrap_or("shared-ro")
        {
            "shared-ro" => {
                // The root works in the real workdir; only children are read-only.
                if name != &root {
                    if let Some(t) = tools.iter().find(|t| *t == "write" || *t == "edit") {
                        return Err(format!("shared-ro {what} lists {t:?}"));
                    }
                }
            }
            "worktree" => {
                return Err(format!(
                    "{what}: workspace \"worktree\" is not implemented in stage 1"
                ))
            }
            w => return Err(format!("{what}: unknown workspace {w:?}")),
        }
        roles.insert(
            name.clone(),
            Role {
                tools,
                budget_tokens: num_of(r, "budget_tokens", Some(40_000))?.unwrap_or(0),
                budget_steps: num_of(r, "budget_steps", None)?.map(|n| n as u32),
                prompt: r.get("prompt").and_then(Value::as_str).map(str::to_owned),
            },
        );
    }
    let mut edges = Vec::new();
    for e in v.get("edges").and_then(Value::as_array).unwrap_or(&vec![]) {
        let (from, to) = (str_of(e, "from", "edge")?, str_of(e, "to", "edge")?);
        for end in [from, to] {
            if !roles.contains_key(end) {
                return Err(format!(
                    "edge {from:?} -> {to:?} names unknown role {end:?}"
                ));
            }
        }
        match e.get("kind").and_then(Value::as_str).unwrap_or("spawn") {
            "spawn" => edges.push((from.to_string(), to.to_string())),
            "message" => return Err("edge kind \"message\" is not implemented in stage 1".into()),
            k => return Err(format!("unknown edge kind {k:?}")),
        }
    }
    Ok(Config {
        root,
        limits,
        roles,
        edges,
    })
}

struct Req {
    id: String,
    role: String,
    brief: String,
    depth: usize,
    done: watch::Sender<Option<String>>,
}

struct State {
    spawned: usize,
    pool: u64,
    /// child id -> (parent id, result channel)
    children: HashMap<String, (String, watch::Receiver<Option<String>>)>,
}

struct Shared {
    cfg: Config,
    tx: mpsc::UnboundedSender<Req>,
    state: Mutex<State>,
}

/// One session's agent tree: the shared counters plus the spawn-request queue
/// that `with_children` drains.
pub(crate) struct Tree {
    shared: Arc<Shared>,
    rx: mpsc::UnboundedReceiver<Req>,
}

impl Tree {
    pub(crate) fn new(cfg: &Config) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let cfg = cfg.clone();
        let pool = cfg.limits.child_budget_tokens;
        let state = Mutex::new(State {
            spawned: 0,
            pool,
            children: HashMap::new(),
        });
        Self {
            shared: Arc::new(Shared { cfg, tx, state }),
            rx,
        }
    }

    pub(crate) fn register_root(&self, reg: &mut tool_core::Registry) {
        let root = self.shared.cfg.root.clone();
        for t in self.shared.agent_tools("root", &root, 0) {
            reg.register(t);
        }
    }
}

impl Shared {
    /// `spawn` + `wait` for an agent whose role has a spawn edge and whose
    /// depth is below `max_depth`; empty otherwise.
    fn agent_tools(self: &Arc<Self>, id: &str, role: &str, depth: usize) -> Vec<Arc<dyn Tool>> {
        if depth >= self.cfg.limits.max_depth || !self.cfg.edges.iter().any(|(f, _)| f == role) {
            return Vec::new();
        }
        let (shared, id, role) = (self.clone(), id.to_string(), role.to_string());
        vec![
            Arc::new(Spawn {
                shared: shared.clone(),
                id: id.clone(),
                role,
                depth,
            }),
            Arc::new(Wait { shared, id }),
        ]
    }
}

fn definition(name: &str, description: &str, props: Value, required: &[&str]) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        schema: json!({
            "type": "object",
            "properties": props,
            "required": required,
            "additionalProperties": false,
        }),
    }
}

fn ok(content: String) -> Result<ToolOutcome, ToolError> {
    Ok(ToolOutcome {
        content,
        truncated: false,
        success: true,
    })
}

struct Spawn {
    shared: Arc<Shared>,
    id: String,
    role: String,
    depth: usize,
}

#[async_trait::async_trait]
impl Tool for Spawn {
    fn definition(&self) -> ToolDefinition {
        definition(
            "spawn",
            "Start a child agent of the given role on a brief; returns its id immediately. Collect the answer with wait.",
            json!({"role": {"type": "string"}, "brief": {"type": "string"}}),
            &["role", "brief"],
        )
    }
    async fn execute(
        &self,
        inv: Invocation,
        _: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let s = &self.shared;
        let role = inv.args["role"].as_str().unwrap_or_default();
        let brief = inv.args["brief"].as_str().unwrap_or_default();
        if !s
            .cfg
            .edges
            .iter()
            .any(|(f, t)| *f == self.role && t == role)
        {
            return Err(ToolError::Denied(format!(
                "no spawn edge from {} to {role}",
                self.role
            )));
        }
        let budget = s.cfg.roles[role].budget_tokens;
        let (done, rx) = watch::channel(None);
        let id = {
            let mut st = s.state.lock().unwrap();
            if st.spawned >= s.cfg.limits.max_agents {
                return Err(ToolError::Failed(format!(
                    "max_agents {} reached",
                    s.cfg.limits.max_agents
                )));
            }
            if st.pool < budget {
                return Err(ToolError::Failed(format!(
                    "child budget pool has {} tokens, role {role} needs {budget}",
                    st.pool
                )));
            }
            // shortcut: unused child tokens are not returned to the pool, upgrade when runs routinely leave most of a child budget unspent.
            st.pool -= budget;
            st.spawned += 1;
            let id = format!("a{}", st.spawned);
            st.children.insert(id.clone(), (self.id.clone(), rx));
            id
        };
        let req = Req {
            id: id.clone(),
            role: role.into(),
            brief: brief.into(),
            depth: self.depth + 1,
            done,
        };
        s.tx.send(req)
            .map_err(|_| ToolError::Failed("agent runner is gone".into()))?;
        ok(format!("spawned role={role} id={id}"))
    }
}

struct Wait {
    shared: Arc<Shared>,
    id: String,
}

#[async_trait::async_trait]
impl Tool for Wait {
    fn definition(&self) -> ToolDefinition {
        definition(
            "wait",
            "Block until the given child agents finish; returns each one's final answer or failure reason, labelled by id.",
            json!({"ids": {"type": "array", "items": {"type": "string"}}}),
            &["ids"],
        )
    }
    async fn execute(
        &self,
        inv: Invocation,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let ids: Vec<&str> = inv.args["ids"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut rxs = Vec::new();
        {
            let st = self.shared.state.lock().unwrap();
            for id in &ids {
                match st.children.get(*id) {
                    Some((parent, rx)) if *parent == self.id => rxs.push((*id, rx.clone())),
                    _ => {
                        return Err(ToolError::Failed(format!(
                            "unknown id {id}: not a child of {}",
                            self.id
                        )))
                    }
                }
            }
        }
        let mut out = String::new();
        for (id, mut rx) in rxs {
            let got = tokio::select! {
                r = rx.wait_for(|v| v.is_some()) => r.map(|v| v.clone().unwrap_or_default()),
                _ = cancel.cancelled() => return Err(ToolError::Failed("cancelled".into())),
            };
            let text = got.map_err(|_| ToolError::Failed(format!("agent {id} was stopped")))?;
            out.push_str(&format!("[{id}]\n{text}\n\n"));
        }
        ok(bound_text(out.trim_end(), MAX_MODEL_CHARS))
    }
}

/// Run `run` (the root's loop) while polling every requested child next to it.
/// Children still running when the root finishes are dropped.
pub(crate) async fn with_children<P: LlmClient, F: Future>(
    tree: Option<&mut Tree>,
    provider: &P,
    args: &Args,
    policy: &Arc<tools_std::Policy>,
    run: F,
) -> F::Output {
    let Some(tree) = tree else { return run.await };
    let shared = tree.shared.clone();
    let rx = &mut tree.rx;
    let mut running: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = Vec::new();
    let drive = std::future::poll_fn(|cx| {
        while let Poll::Ready(Some(req)) = rx.poll_recv(cx) {
            running.push(Box::pin(run_child(
                &shared,
                provider,
                args,
                policy.clone(),
                req,
            )));
        }
        running.retain_mut(|f| f.as_mut().poll(cx).is_pending());
        Poll::<()>::Pending
    });
    tokio::select! {
        biased;
        out = run => out,
        () = drive => unreachable!("the driver never completes"),
    }
}

/// The role's tools (same policy/workdir/exec wrap as the parent's), plus
/// `spawn`/`wait` when the role may spawn below `max_depth`.
fn role_tools(
    shared: &Arc<Shared>,
    args: &Args,
    policy: Arc<tools_std::Policy>,
    id: &str,
    role: &str,
    depth: usize,
) -> Result<Vec<Arc<dyn Tool>>, String> {
    let mut tools = crate::tools_for(args, policy)?;
    tools.retain(|t| shared.cfg.roles[role].tools.contains(&t.definition().name));
    tools.extend(shared.agent_tools(id, role, depth));
    Ok(tools)
}

async fn run_child<P: LlmClient>(
    shared: &Arc<Shared>,
    provider: &P,
    args: &Args,
    policy: Arc<tools_std::Policy>,
    req: Req,
) {
    let role = &shared.cfg.roles[&req.role];
    let tools = match role_tools(shared, args, policy, &req.id, &req.role, req.depth) {
        Ok(t) => t,
        Err(e) => return finish(&req, 0, format!("failed: cannot configure tools: {e}")),
    };
    let registry = crate::registry_from(tools, &[]);

    let mut b = config_for(Capability::UnattendedBatch);
    b.max_wallclock = Duration::from_secs(3600);
    b.max_tokens = role.budget_tokens;
    if let Some(n) = role.budget_steps.and_then(std::num::NonZeroU32::new) {
        b = with_steps(b, n);
    }
    let mut state = LoopState::new();
    state.budget = BudgetGuard::new(b, Instant::now());

    let mut cfg = run_config(args);
    cfg.goal = req.brief.clone();
    cfg.proof_cmd = None;
    cfg.completion_check = None;
    cfg.history_read = false;
    cfg.deliverables = Vec::new();
    cfg.log_path = resolve_log_path(args).map(|p| {
        let mut name = p.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{}", req.id));
        let log = p.with_file_name(name);
        exclude_sidecar(&args.workdir, &log);
        log
    });
    let input = match &role.prompt {
        Some(p) => format!("{p}\n\n{}", req.brief),
        None => req.brief.clone(),
    };
    let mut emitter = Emitter::new();
    let outcome = agent_loop::run(
        &mut state,
        Run {
            provider,
            registry: &registry,
            agent: "agent",
            workdir: &args.workdir,
            emitter: &mut emitter,
            bets: &NoBets,
            cfg,
        },
        vec![Input::User(input)],
        &CancellationToken::new(),
    )
    .await;
    let text = match outcome {
        Outcome::Done => crate::latest_assistant_text(&emitter).unwrap_or_default(),
        o => format!("failed: {o:?}"),
    };
    finish(&req, state.budget.counters().tokens, text);
}

fn finish(req: &Req, tokens: u64, text: String) {
    let outcome = if text.starts_with("failed: ") {
        "failed"
    } else {
        "done"
    };
    eprintln!(
        "agent {} role={} tokens={tokens} outcome={outcome}",
        req.id, req.role
    );
    let _ = req.done.send(Some(text));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{args_for, text_resp, tmp, usage, ScriptClient};
    use provider_core::{AssistantMessage, Response, StopReason, ToolCallRef};

    const CFG: &str = r#"{"root":"lead","limits":{"max_depth":1,"max_agents":2,"child_budget_tokens":100000},
      "roles":{"lead":{"tools":["view","write"]},
               "scout":{"tools":["view","search"],"budget_tokens":40000,"prompt":"be brief"}},
      "edges":[{"from":"lead","to":"scout","kind":"spawn"}]}"#;

    fn calls(cs: &[(&str, &str, Value)]) -> Response {
        Response {
            message: AssistantMessage {
                content: String::new(),
                tool_calls: cs
                    .iter()
                    .map(|(id, name, args)| ToolCallRef {
                        id: (*id).into(),
                        name: (*name).into(),
                        args: args.clone(),
                    })
                    .collect(),
                thinking: None,
            },
            stop: StopReason::ToolUse,
            usage: usage(),
            latency_ms: 0,
            attempts: 1,
            raw_stop_reason: None,
            retry_usage: None,
        }
    }

    fn spawn(id: &str, role: &str) -> Response {
        calls(&[(id, "spawn", json!({"role": role, "brief": "look around"}))])
    }

    fn wait(ids: &[&str]) -> Response {
        calls(&[("w", "wait", json!({ "ids": ids }))])
    }

    fn text(s: &str) -> Response {
        let mut r = text_resp();
        r.message.content = s.into();
        r
    }

    /// Children and the root share one provider, so a single script would
    /// race on scheduling: route each request to its agent's own script (a
    /// child's first user message is the role prompt or brief).
    struct Routed {
        root: ScriptClient,
        child: ScriptClient,
    }

    #[async_trait::async_trait]
    impl LlmClient for Routed {
        async fn complete(
            &self,
            model: &str,
            req: &provider_core::Request,
        ) -> Result<Response, provider_core::LlmError> {
            let first = req.messages.iter().find(|m| m.role == "user");
            let is_child = first.is_some_and(|m| m.content.contains("look around"));
            let c = if is_child { &self.child } else { &self.root };
            c.complete(model, req).await
        }
    }

    /// Runs the root with the given agents JSON; returns the client (all
    /// requests seen) and the tool results the root observed.
    async fn session(
        cfg: &str,
        budget: u64,
        root: Vec<Response>,
        child: Vec<Response>,
    ) -> (Routed, Vec<String>) {
        let dir = tmp();
        let mut args = args_for(&dir, None);
        args.agents = Some(parse(cfg).unwrap());
        args.budget_tokens = Some(budget);
        args.no_log = true;
        let client = Routed {
            root: ScriptClient::new(root),
            child: ScriptClient::new(child),
        };
        let r = crate::execute(&client, &args).await;
        let results = r
            .events
            .iter()
            .filter_map(|e| match e {
                agent_event::AgentEvent::ToolEnd { result, .. } => Some(format!("{result:?}")),
                _ => None,
            })
            .collect();
        (client, results)
    }

    #[tokio::test]
    async fn scout_answer_comes_back_through_wait() {
        let (client, results) = session(
            CFG,
            200_000,
            vec![spawn("s", "scout"), wait(&["a1"]), text_resp()],
            vec![text("scout found X")],
        )
        .await;
        assert!(results[0].contains("id=a1"), "{results:?}");
        assert!(results[1].contains("scout found X"), "{results:?}");
        let seen = client.child.seen.lock().unwrap();
        assert!(seen
            .iter()
            .flatten()
            .any(|m| m.role == "user" && m.content.contains("be brief")));
    }

    #[tokio::test]
    async fn spawn_without_edge_is_refused() {
        let cfg = CFG.replace(
            "\"kind\":\"spawn\"}",
            "\"kind\":\"spawn\"},{\"from\":\"scout\",\"to\":\"scout\"}",
        );
        let (_, results) =
            session(&cfg, 200_000, vec![spawn("s", "lead"), text_resp()], vec![]).await;
        assert!(
            results[0].contains("no spawn edge from lead to lead"),
            "{results:?}"
        );
    }

    #[tokio::test]
    async fn child_registry_is_read_only_and_cannot_spawn() {
        let dir = tmp();
        let mut args = args_for(&dir, None);
        args.agents = Some(parse(CFG).unwrap());
        let tree = Tree::new(args.agents.as_ref().unwrap());
        let tools = |id: &str, role: &str, depth| -> Vec<String> {
            let policy = Arc::new(crate::policy_for(&args));
            let t = role_tools(&tree.shared, &args, policy, id, role, depth).unwrap();
            t.iter().map(|t| t.definition().name).collect()
        };
        let child = tools("a1", "scout", 1);
        assert!(
            child
                .iter()
                .all(|n| n != "write" && n != "edit" && n != "spawn" && n != "wait"),
            "{child:?}"
        );
        assert!(child.contains(&"view".to_string()));
        assert!(tools("root", "lead", 0).contains(&"spawn".to_string()));
    }

    #[tokio::test]
    async fn pool_exhaustion_refuses_and_startup_checks_pool() {
        // Pool 100k, scout 40k each: third would need 120k, but max_agents is 2,
        // so lift max_agents to isolate the pool.
        let cfg = CFG.replace("\"max_agents\":2", "\"max_agents\":9");
        let (_, results) = session(
            &cfg,
            200_000,
            vec![
                spawn("s1", "scout"),
                spawn("s2", "scout"),
                spawn("s3", "scout"),
                text_resp(),
            ],
            vec![text("x"), text("y")],
        )
        .await;
        assert!(
            results[2].contains("child budget pool has 20000"),
            "{results:?}"
        );

        let dir = tmp();
        let mut args = args_for(&dir, None);
        args.agents = Some(parse(CFG).unwrap());
        args.budget_tokens = Some(100_000);
        let e = crate::cli::budget_for(&args).err().unwrap();
        assert!(e.contains("must be below"), "{e}");
    }

    #[tokio::test]
    async fn max_agents_is_enforced() {
        let cfg = CFG.replace("\"max_agents\":2", "\"max_agents\":1");
        let (_, results) = session(
            &cfg,
            200_000,
            vec![spawn("s1", "scout"), spawn("s2", "scout"), text_resp()],
            vec![text("x")],
        )
        .await;
        assert!(results[1].contains("max_agents 1 reached"), "{results:?}");
    }

    #[tokio::test]
    async fn wait_on_unknown_id_is_an_error() {
        let (_, results) = session(CFG, 200_000, vec![wait(&["nope"]), text_resp()], vec![]).await;
        assert!(results[0].contains("unknown id nope"), "{results:?}");
    }

    #[test]
    fn bad_configs_are_rejected_with_a_reason() {
        let bad = |from: &str, to: &str, want: &str| {
            let e = parse(&CFG.replace(from, to)).unwrap_err();
            assert!(e.contains(want), "{e}");
        };
        bad(
            "\"root\":\"lead\"",
            "\"root\":\"nobody\"",
            "root \"nobody\" is not a role",
        );
        bad(
            "\"to\":\"scout\"",
            "\"to\":\"ghost\"",
            "unknown role \"ghost\"",
        );
        bad(
            "\"tools\":[\"view\",\"search\"]",
            "\"tools\":[\"view\",\"fly\"]",
            "unknown tool \"fly\"",
        );
        bad(
            "\"budget_tokens\":40000",
            "\"workspace\":\"worktree\"",
            "not implemented in stage 1",
        );
        bad(
            "\"kind\":\"spawn\"",
            "\"kind\":\"message\"",
            "not implemented in stage 1",
        );
        bad(
            "\"tools\":[\"view\",\"search\"]",
            "\"tools\":[\"view\",\"write\"]",
            "shared-ro",
        );
    }
}
