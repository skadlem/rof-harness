//! Minimal headless driver: `rof run ...` wires the frozen siblings once.
//! Prompt shape (goal + file map + named files via the context assembler)
//! lives in `agent_loop::run`; this crate only feeds it `RunConfig` + input.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use agent_budget::{config_for, BudgetGuard, Capability};
use agent_event::{AgentEvent, Emitter};
use agent_loop::{Input, LoopState, NoBets, Outcome, Run, RunConfig};
use provider_core::LlmClient;
use provider_openai::{EndpointProfile, OpenAiCompat};
use snapshot::TreeService;
use tokio_util::sync::CancellationToken;
use tool_core::GrantGate;

const USAGE: &str = "usage: rof run --goal TEXT --workdir DIR --model ID [--endpoint URL] [--allow-cmd CMD...] [--budget-steps N] [--dump-events PATH]
--dump-events PATH: JSONL, one event per line (LF); replaces any previous dump at PATH";

#[derive(Debug, PartialEq)]
struct Args {
    goal: String,
    workdir: PathBuf,
    model: String,
    endpoint: Option<String>,
    allow_cmd: Vec<String>,
    budget_steps: Option<u32>,
    dump_events: Option<String>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let a: Vec<&str> = argv.iter().map(String::as_str).collect();
    if a.len() < 2 || a[1] != "run" {
        return Err(USAGE.into());
    }
    let (mut goal, mut workdir, mut model, mut endpoint, mut steps, mut dump) =
        (None, None, None, None, None, None);
    let mut allow = Vec::new();
    let mut i = 2;
    while i < a.len() {
        let flag = a[i];
        let (k, inline) = match flag.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (flag, None),
        };
        let take = |i: &mut usize, inline: Option<String>| -> Result<String, String> {
            if let Some(v) = inline {
                return Ok(v);
            }
            *i += 1;
            a.get(*i)
                .map(|s| (*s).to_string())
                .ok_or_else(|| format!("{k} needs a value"))
        };
        match k {
            "--goal" => goal = Some(take(&mut i, inline)?),
            "--workdir" => workdir = Some(take(&mut i, inline)?),
            "--model" => model = Some(take(&mut i, inline)?),
            "--endpoint" => endpoint = Some(take(&mut i, inline)?),
            "--dump-events" => dump = Some(take(&mut i, inline)?),
            "--allow-cmd" => allow.push(take(&mut i, inline)?),
            "--budget-steps" => {
                let s = take(&mut i, inline)?;
                let n: u32 = s
                    .parse()
                    .map_err(|_| format!("--budget-steps needs a positive integer, got {s:?}"))?;
                if n == 0 {
                    return Err("--budget-steps must be > 0".into());
                }
                steps = Some(n);
            }
            other => return Err(format!("unknown flag {other:?}\n{USAGE}")),
        }
        i += 1;
    }
    Ok(Args {
        goal: goal.ok_or("missing --goal")?,
        workdir: PathBuf::from(workdir.ok_or("missing --workdir")?),
        model: model.ok_or("missing --model")?,
        endpoint,
        allow_cmd: allow,
        budget_steps: steps,
        dump_events: dump,
    })
}

fn resolve_endpoint(flag: Option<&str>, env: Option<&str>) -> Result<String, String> {
    flag.or(env)
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "no endpoint: pass --endpoint URL or set OPENAI_BASE_URL".into())
}

fn exit_code(outcome: &Outcome) -> i32 {
    match outcome {
        Outcome::Done => 0,
        _ => 3,
    }
}

const NAMES: [&str; 11] = [
    "RunStart",
    "RunEnd",
    "TurnStart",
    "TurnEnd",
    "MessageStart",
    "MessageUpdate",
    "MessageEnd",
    "ToolStart",
    "ToolEnd",
    "Control",
    "Error",
];

fn summarize(events: &[AgentEvent]) -> String {
    let mut counts = [0usize; 11];
    for e in events {
        counts[match e {
            AgentEvent::RunStart { .. } => 0,
            AgentEvent::RunEnd { .. } => 1,
            AgentEvent::TurnStart { .. } => 2,
            AgentEvent::TurnEnd { .. } => 3,
            AgentEvent::MessageStart { .. } => 4,
            AgentEvent::MessageUpdate { .. } => 5,
            AgentEvent::MessageEnd { .. } => 6,
            AgentEvent::ToolStart { .. } => 7,
            AgentEvent::ToolEnd { .. } => 8,
            AgentEvent::Control(_) => 9,
            AgentEvent::Error { .. } => 10,
        }] += 1;
    }
    NAMES
        .iter()
        .zip(counts)
        .filter(|(_, n)| *n > 0)
        .map(|(name, n)| format!("{name}={n}"))
        .collect::<Vec<_>>()
        .join(" ")
}

struct RunResult {
    outcome: Outcome,
    patch: String,
    events: Vec<AgentEvent>,
}

async fn execute<P: LlmClient>(provider: &P, args: &Args) -> RunResult {
    let policy = Arc::new(tools_std::Policy {
        root: args.workdir.clone(),
        // --allow-cmd entries are binary prefixes (no shell: `&&`/`|` are
        // literal args, so exact-string matching would allow nothing useful).
        allowed_commands: Vec::new(),
        allowed_prefixes: args.allow_cmd.clone(),
        syntax_cmd: None,
        denied_globs: tools_std::default_denied_globs(),
    });
    let mut reg = tool_core::Registry::new(Arc::new(GrantGate::new(HashMap::from([(
        "agent".to_string(),
        ["view", "search", "edit", "write", "exec", "test"]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
    )]))));
    reg.register(Arc::new(tools_std::view_tool(policy.clone())));
    reg.register(Arc::new(tools_std::search_tool(policy.clone())));
    reg.register(Arc::new(tools_std::edit_tool(policy.clone())));
    reg.register(Arc::new(tools_std::write_tool(policy.clone())));
    reg.register(Arc::new(tools_std::exec_tool(policy.clone())));
    reg.register(Arc::new(tools_std::test_tool(policy)));
    let mut state = LoopState::new();
    if let Some(n) = args.budget_steps {
        let mut b = config_for(Capability::UnattendedBatch);
        b.max_steps = NonZeroU32::new(n).expect("parse rejects 0");
        b.warn_steps = b
            .warn_steps
            .min(NonZeroU32::new(n.saturating_sub(1).max(1)).expect("max(1) is non-zero"));
        b.max_refunds = n / 4;
        state.budget = BudgetGuard::new(b, Instant::now());
    }
    let mut emitter = Emitter::new();
    let cancel = CancellationToken::new();
    // First Ctrl-C cancels the run; the loop reports Outcome::Cancelled.
    tokio::spawn({
        let cancel = cancel.clone();
        async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancel.cancel();
            }
        }
    });
    let outcome = agent_loop::run(
        &mut state,
        Run {
            provider,
            registry: &reg,
            agent: "agent",
            workdir: &args.workdir,
            emitter: &mut emitter,
            bets: &NoBets as &dyn agent_loop::BetsHook,
            cfg: RunConfig {
                goal: args.goal.clone(),
                model: args.model.clone(),
                ..RunConfig::default()
            },
        },
        vec![Input::User(args.goal.clone())],
        &cancel,
    )
    .await;
    let tree = TreeService::new(&args.workdir);
    let patch = tree
        .diff()
        .ok()
        .and_then(|d| tree.patch(&d).ok())
        .map(|p| p.text)
        .unwrap_or_default();
    eprintln!("events {}", summarize(emitter.history()));
    RunResult {
        outcome,
        patch,
        events: emitter.history().to_vec(),
    }
}

/// One run's events as JSONL (one object per line, LF) via `trace::TraceSink`.
/// The sink only appends, so a dump replaces the previous file: one run, one dump.
fn write_dump(path: &str, events: &[AgentEvent]) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    let sink = trace::TraceSink::with_file(Path::new(path)).map_err(|e| e.to_string())?;
    for e in events {
        sink.emit(e.clone());
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if !args.workdir.is_dir() {
        eprintln!("workdir is not a directory: {}", args.workdir.display());
        std::process::exit(2);
    }
    let endpoint = match resolve_endpoint(
        args.endpoint.as_deref(),
        std::env::var("OPENAI_BASE_URL").ok().as_deref(),
    ) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let provider = OpenAiCompat::new(&endpoint, EndpointProfile::default());
    let r = execute(&provider, &args).await;
    if let Some(path) = args.dump_events.as_deref() {
        if let Err(e) = write_dump(path, &r.events) {
            eprintln!("cannot write {path}: {e}");
            std::process::exit(2);
        }
    }
    if !r.patch.is_empty() {
        println!("{}", r.patch);
    }
    std::process::exit(exit_code(&r.outcome));
}

#[cfg(test)]
mod tests {
    use super::*;
    use provider_core::{AssistantMessage, Response, StopReason, ToolCallRef, Usage};
    use std::collections::VecDeque;
    use std::path::Path;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn cli_full_and_eq_forms() {
        let a = parse_args(&argv(&[
            "rof",
            "run",
            "--goal",
            "ship it",
            "--workdir",
            "/tmp/w",
            "--model",
            "m",
            "--endpoint",
            "http://x",
            "--allow-cmd",
            "cargo test",
            "--allow-cmd",
            "true",
            "--budget-steps",
            "7",
            "--dump-events",
            "/tmp/ev.json",
        ]))
        .unwrap();
        assert_eq!(
            a,
            Args {
                goal: "ship it".into(),
                workdir: PathBuf::from("/tmp/w"),
                model: "m".into(),
                endpoint: Some("http://x".into()),
                allow_cmd: vec!["cargo test".into(), "true".into()],
                budget_steps: Some(7),
                dump_events: Some("/tmp/ev.json".into()),
            }
        );
        let b = parse_args(&argv(&[
            "rof",
            "run",
            "--goal=ship it",
            "--workdir=/tmp/w",
            "--model=m",
        ]))
        .unwrap();
        assert_eq!(b.allow_cmd, Vec::<String>::new());
        assert_eq!(
            (b.endpoint, b.budget_steps, b.dump_events),
            (None, None, None)
        );
    }

    #[test]
    fn cli_rejects() {
        for words in [
            vec!["rof"],
            vec!["rof", "eval"],
            vec!["rof", "run", "--goal", "g", "--workdir", "/w"],
            vec!["rof", "run", "--goal", "g", "--model", "m"],
            vec!["rof", "run", "--workdir", "/w", "--model", "m"],
            vec![
                "rof",
                "run",
                "--goal",
                "g",
                "--workdir",
                "/w",
                "--model",
                "m",
                "--bogus",
            ],
            vec![
                "rof",
                "run",
                "--goal",
                "g",
                "--workdir",
                "/w",
                "--model",
                "m",
                "--budget-steps",
                "0",
            ],
            vec![
                "rof",
                "run",
                "--goal",
                "g",
                "--workdir",
                "/w",
                "--model",
                "m",
                "--budget-steps",
                "many",
            ],
            vec!["rof", "run", "--goal"],
        ] {
            assert!(parse_args(&argv(&words)).is_err(), "{words:?}");
        }
    }

    #[test]
    fn endpoint_flag_wins_empty_is_missing() {
        assert_eq!(
            resolve_endpoint(Some("http://flag"), Some("http://env")).as_deref(),
            Ok("http://flag")
        );
        assert_eq!(
            resolve_endpoint(None, Some("http://env")).as_deref(),
            Ok("http://env")
        );
        assert!(resolve_endpoint(None, None).is_err());
        assert!(resolve_endpoint(Some("  "), None).is_err());
    }

    #[test]
    fn exit_and_summary_pinned() {
        assert_eq!(exit_code(&Outcome::Done), 0);
        for o in [
            Outcome::Halted("steps".into()),
            Outcome::Cancelled,
            Outcome::Failed("e".into()),
        ] {
            assert_eq!(exit_code(&o), 3);
        }
        let evs = vec![
            AgentEvent::RunStart {
                run_id: 0,
                goal: "g".into(),
            },
            AgentEvent::TurnStart { turn: 1 },
            AgentEvent::TurnStart { turn: 1 },
            AgentEvent::Error {
                error: agent_event::AgentError {
                    code: "E".into(),
                    message: "m".into(),
                },
            },
        ];
        assert_eq!(summarize(&evs), "RunStart=1 TurnStart=2 Error=1");
        assert_eq!(summarize(&[]), "");
    }

    struct ScriptClient {
        queue: std::sync::Mutex<VecDeque<Response>>,
    }

    impl ScriptClient {
        fn new(resps: Vec<Response>) -> Self {
            Self {
                queue: std::sync::Mutex::new(resps.into()),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmClient for ScriptClient {
        async fn complete(
            &self,
            _model: &str,
            _req: &provider_core::Request,
        ) -> Result<Response, provider_core::LlmError> {
            self.queue
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(provider_core::LlmError::Transport("script empty".into()))
        }
        fn capabilities(&self, _model: &str) -> provider_core::Capabilities {
            provider_core::Capabilities {}
        }
        async fn resolve_key(
            &self,
            _provider: &str,
        ) -> Result<provider_core::Credentials, provider_core::LlmError> {
            Ok(provider_core::Credentials {
                api_key: String::new(),
            })
        }
    }

    fn usage() -> Usage {
        Usage {
            input: 1,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            reasoning: None,
            cost_usd: None,
        }
    }

    fn tool_resp() -> Response {
        Response {
            message: AssistantMessage {
                content: "editing".into(),
                tool_calls: vec![ToolCallRef {
                    id: "c1".into(),
                    name: "edit".into(),
                    args: serde_json::json!({
                        "path": "note.txt",
                        "search": "hello",
                        "replace": "bye",
                    }),
                }],
                thinking: None,
            },
            stop: StopReason::ToolUse,
            usage: usage(),
            latency_ms: 0,
            attempts: 1,
            raw_stop_reason: None,
        }
    }

    fn text_resp() -> Response {
        Response {
            message: AssistantMessage {
                content: "all good".into(),
                tool_calls: vec![],
                thinking: None,
            },
            stop: StopReason::Stop,
            usage: usage(),
            latency_ms: 0,
            attempts: 1,
            raw_stop_reason: None,
        }
    }

    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn tmp() -> PathBuf {
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("rof-test-{n}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn args_for(dir: &Path, steps: Option<u32>) -> Args {
        Args {
            goal: "update the note".into(),
            workdir: dir.to_path_buf(),
            model: "fake".into(),
            endpoint: None,
            allow_cmd: vec![],
            budget_steps: steps,
            dump_events: None,
        }
    }

    #[tokio::test]
    async fn headless_run_edits_then_done() {
        let dir = tmp();
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        let client = ScriptClient::new(vec![tool_resp(), text_resp()]);
        let r = execute(&client, &args_for(&dir, None)).await;
        assert!(matches!(r.outcome, Outcome::Done), "{:?}", r.outcome);
        assert_eq!(exit_code(&r.outcome), 0);
        assert_eq!(
            std::fs::read_to_string(dir.join("note.txt")).unwrap(),
            "bye\n"
        );
        assert!(r.patch.contains("note.txt"), "final patch must name it");
    }

    #[tokio::test]
    async fn dump_events_is_one_json_object_per_line() {
        let dir = tmp();
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        let client = ScriptClient::new(vec![tool_resp(), text_resp()]);
        let r = execute(&client, &args_for(&dir, None)).await;
        assert!(!r.events.is_empty(), "fake run must produce events");
        let path = dir.join("dump.jsonl");
        let p = path.to_str().unwrap();
        write_dump(p, &r.events).unwrap();
        // Second dump to the same path replaces, never appends.
        write_dump(p, &r.events).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'), "file ends with LF");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), r.events.len(), "N events, N lines");
        for line in &lines {
            let v: serde_json::Value =
                serde_json::from_str(line).expect("every line incl. the last parses alone");
            assert!(v.is_object(), "line is a JSON object: {line}");
        }
    }

    #[tokio::test]
    async fn budget_halt_exits_3() {
        let dir = tmp();
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        let client = ScriptClient::new(vec![text_resp()]);
        let r = execute(&client, &args_for(&dir, Some(1))).await;
        assert!(matches!(r.outcome, Outcome::Halted(_)), "{:?}", r.outcome);
        assert_eq!(exit_code(&r.outcome), 3);
    }
}
