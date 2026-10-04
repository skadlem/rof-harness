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

const USAGE: &str = "usage: rof run --goal TEXT --workdir DIR --model ID [--endpoint URL] [--api-key-env NAME] [--header NAME:VALUE] [--allow-cmd CMD...] [--budget-steps N] [--budget-actions N] [--budget-tokens N] [--max-tokens N] [--context-file PATH]... [--proof-cmd CMD] [--incentives LEVEL] [--bets] [--dump-events PATH]
--dump-events PATH: JSONL, one event per line (LF); replaces any previous dump at PATH
--context-file PATH: pinned context; with neither --budget-tokens nor --budget-steps the run defaults to a 200000-token budget";

#[derive(Debug, PartialEq)]
struct Args {
    goal: String,
    workdir: PathBuf,
    model: String,
    endpoint: Option<String>,
    /// Env var holding the API key (default OPENAI_API_KEY; opencode-go uses
    /// GO_KEY). The key itself never appears in argv or logs.
    api_key_env: String,
    /// Extra request headers as NAME:VALUE (e.g. x-opencode-session:<id>).
    headers: Vec<String>,
    allow_cmd: Vec<String>,
    budget_steps: Option<u32>,
    budget_actions: Option<u32>,
    budget_tokens: Option<u64>,
    max_tokens: Option<usize>,
    context_files: Vec<String>,
    dump_events: Option<String>,
    proof_cmd: Option<String>,
    incentives: agent_loop::IncentivesLevel,
    bets: bool,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let a: Vec<&str> = argv.iter().map(String::as_str).collect();
    if a.len() < 2 || a[1] != "run" {
        return Err(USAGE.into());
    }
    let (mut goal, mut workdir, mut model, mut endpoint, mut steps, mut dump) =
        (None, None, None, None, None, None);
    let mut api_key_env = "OPENAI_API_KEY".to_string();
    let mut headers: Vec<String> = Vec::new();
    let mut actions: Option<u32> = None;
    let mut allow = Vec::new();
    let mut context_files = Vec::new();
    let mut max_tokens: Option<usize> = None;
    let mut budget_tokens: Option<u64> = None;
    let mut proof_cmd: Option<String> = None;
    let mut incentives = agent_loop::IncentivesLevel::Full;
    let mut bets = false;
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
            "--context-file" => context_files.push(take(&mut i, inline)?),
            "--max-tokens" => {
                let s = take(&mut i, inline)?;
                max_tokens =
                    Some(s.parse().map_err(|_| {
                        format!("--max-tokens needs a positive integer, got {s:?}")
                    })?);
            }
            "--budget-tokens" => {
                let s = take(&mut i, inline)?;
                budget_tokens =
                    Some(s.parse().map_err(|_| {
                        format!("--budget-tokens needs a positive integer, got {s:?}")
                    })?);
            }
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
            "--budget-actions" => {
                let s = take(&mut i, inline)?;
                let n: u32 = s
                    .parse()
                    .map_err(|_| format!("--budget-actions needs a positive integer, got {s:?}"))?;
                if n == 0 {
                    return Err("--budget-actions must be > 0".into());
                }
                actions = Some(n);
            }
            "--proof-cmd" => {
                proof_cmd = Some(take(&mut i, inline)?);
            }
            "--api-key-env" => {
                api_key_env = take(&mut i, inline)?;
            }
            "--header" => {
                headers.push(take(&mut i, inline)?);
            }
            "--incentives" => {
                incentives = match take(&mut i, inline)?.as_str() {
                    "base" => agent_loop::IncentivesLevel::Base,
                    "contract" => agent_loop::IncentivesLevel::Contract,
                    "full" => agent_loop::IncentivesLevel::Full,
                    other => {
                        return Err(format!(
                            "--incentives needs base|contract|full, got {other:?}"
                        ))
                    }
                };
            }
            "--bets" => {
                bets = true;
            }
            other => return Err(format!("unknown flag {other:?}\n{USAGE}")),
        }
        i += 1;
    }
    Ok(Args {
        goal: goal.ok_or("missing --goal")?,
        context_files,
        max_tokens,
        workdir: PathBuf::from(workdir.ok_or("missing --workdir")?),
        model: model.ok_or("missing --model")?,
        endpoint,
        api_key_env,
        headers,
        allow_cmd: allow,
        budget_steps: steps,
        budget_actions: actions,
        budget_tokens,
        dump_events: dump,
        proof_cmd,
        incentives,
        bets,
    })
}

/// Budget for one run: explicit flags win; context-pinned runs without a
/// token flag get the pilot default; otherwise the library UnattendedBatch
/// config (50k tokens).
fn budget_for(args: &Args) -> BudgetGuard {
    let mut b = config_for(Capability::UnattendedBatch);
    if let Some(n) = args.budget_steps {
        b.max_steps = NonZeroU32::new(n).expect("parse rejects 0");
        b.warn_steps = b
            .warn_steps
            .min(NonZeroU32::new(n.saturating_sub(1).max(1)).expect("max(1) is non-zero"));
        b.max_refunds = n / 4;
    }
    if let Some(n) = args.budget_actions {
        b.actions_per_trial = n;
    }
    match args.budget_tokens {
        Some(t) => b.max_tokens = t,
        // chosen-to-validate: pinned-prefix floor ~4-5k tok/req x 20-30
        // requests + reasoning completions; validated-or-revised at next pilot.
        None if args.budget_steps.is_none() && !args.context_files.is_empty() => {
            b.max_tokens = 200_000
        }
        None => {}
    }
    BudgetGuard::new(b, Instant::now())
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

/// Opt-in Bet B/A gate (`--bets`): uniform all-or-nothing verdict on the
/// batch's proof flags (Bet B); with `--proof-cmd` the flags are per-hunk
/// incremental proofs and [`bets::gate_batch_commit`] keeps the proven
/// leading prefix (Bet A). Read-only batches carry no hunks — nothing to
/// gate, so they commit. Bet C (candidate racing) stays flagged: unbuilt.
struct Gate {
    incremental: bool,
}

impl agent_loop::BetsHook for Gate {
    fn on_post_batch(&self, claim: &bets::Claim, hunks: &[(String, bool)]) -> bets::CommitVerdict {
        if hunks.is_empty() {
            return bets::CommitVerdict::Committed;
        }
        if self.incremental {
            bets::gate_batch_commit(claim, hunks)
        } else {
            bets::gate_commit(claim, hunks.iter().all(|(_, ok)| *ok))
        }
    }
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
    state.budget = budget_for(args);
    let gate = Gate {
        incremental: args.proof_cmd.is_some(),
    };
    let bets_hook: &dyn agent_loop::BetsHook = if args.bets { &gate } else { &NoBets };
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
    // Freeze the run-start HEAD `patch_since_start` diffs from; the loop's own
    // ensure() is idempotent and reports the same failure as a run error.
    let tree = TreeService::new(&args.workdir);
    let _ = tree.ensure();
    let outcome = agent_loop::run(
        &mut state,
        Run {
            provider,
            registry: &reg,
            agent: "agent",
            workdir: &args.workdir,
            emitter: &mut emitter,
            bets: bets_hook,
            cfg: RunConfig {
                goal: args.goal.clone(),
                model: args.model.clone(),
                context_files: args.context_files.clone(),
                max_tokens: args.max_tokens.unwrap_or(8_000),
                incentives: args.incentives,
                proof_cmd: args.proof_cmd.clone(),
                ..RunConfig::default()
            },
        },
        vec![Input::User(args.goal.clone())],
        &cancel,
    )
    .await;
    // The per-Dispatch baseline commits absorb what landed, so the index-based
    // `tree.diff()` reports an empty patch; report the whole run instead.
    let patch = tree
        .patch_since_start()
        .ok()
        .map(|(_, p)| p.text)
        .unwrap_or_default();
    eprintln!("ablation {:?}", state.ablation);
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
    let mut provider =
        OpenAiCompat::new(&endpoint, EndpointProfile::default()).with_key_env(&args.api_key_env);
    for h in &args.headers {
        if let Some((name, value)) = h.split_once(':') {
            provider = provider.with_header(name, value);
        } else {
            eprintln!("--header needs NAME:VALUE, got {h:?}");
            std::process::exit(2);
        }
    }
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
            "--budget-actions",
            "9",
            "--context-file",
            "app/a.py",
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
                api_key_env: "OPENAI_API_KEY".into(),
                headers: Vec::new(),
                allow_cmd: vec!["cargo test".into(), "true".into()],
                budget_steps: Some(7),
                budget_actions: Some(9),
                budget_tokens: None,
                context_files: vec!["app/a.py".into()],
                max_tokens: None,
                dump_events: Some("/tmp/ev.json".into()),
                bets: false,
                incentives: agent_loop::IncentivesLevel::Full,
                proof_cmd: None,
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
            (b.endpoint, b.budget_steps, b.budget_actions, b.dump_events),
            (None, None, None, None)
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
            vec![
                "rof",
                "run",
                "--goal",
                "g",
                "--workdir",
                "/w",
                "--model",
                "m",
                "--budget-actions",
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
                "--budget-actions",
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
            retry_usage: None,
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
            retry_usage: None,
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
            api_key_env: "OPENAI_API_KEY".into(),
            headers: Vec::new(),
            allow_cmd: vec![],
            budget_steps: steps,
            budget_actions: None,
            budget_tokens: None,
            context_files: Vec::new(),
            max_tokens: None,
            dump_events: None,
            bets: false,
            incentives: agent_loop::IncentivesLevel::Full,
            proof_cmd: None,
        }
    }

    #[test]
    fn context_pins_default_budget_and_flags_override() {
        let dir = Path::new("/tmp/w");
        assert_eq!(budget_for(&args_for(dir, None)).config().max_tokens, 50_000);
        let mut pinned = args_for(dir, None);
        pinned.context_files = vec!["a.py".into()];
        assert_eq!(budget_for(&pinned).config().max_tokens, 200_000);
        // Explicit flags win over the pilot default.
        pinned.budget_tokens = Some(7);
        assert_eq!(budget_for(&pinned).config().max_tokens, 7);
        // A step budget also suppresses the token default.
        pinned.budget_tokens = None;
        pinned.budget_steps = Some(5);
        assert_eq!(budget_for(&pinned).config().max_tokens, 50_000);
        assert_eq!(budget_for(&pinned).config().max_steps.get(), 5);
        // --budget-actions overrides the trial action cap.
        pinned.budget_actions = Some(9);
        assert_eq!(budget_for(&pinned).config().actions_per_trial, 9);
        assert_eq!(
            budget_for(&args_for(dir, None)).config().actions_per_trial,
            30
        );
    }

    #[tokio::test]
    async fn headless_run_edits_then_done() {
        let dir = tmp();
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        // Two dispatched batches: the second batch's per-Dispatch baseline
        // absorbs the first, which is what made the old post-run diff empty.
        let mut second = tool_resp();
        second.message.tool_calls[0].id = "c2".into();
        second.message.tool_calls[0].args =
            serde_json::json!({"path": "note.txt", "search": "bye", "replace": "later"});
        let client = ScriptClient::new(vec![tool_resp(), second, text_resp()]);
        let r = execute(&client, &args_for(&dir, None)).await;
        assert!(matches!(r.outcome, Outcome::Done), "{:?}", r.outcome);
        assert_eq!(exit_code(&r.outcome), 0);
        assert_eq!(
            std::fs::read_to_string(dir.join("note.txt")).unwrap(),
            "later\n"
        );
        assert!(
            r.patch.contains("-hello") && r.patch.contains("+later"),
            "patch must span every batch, not just the last: {}",
            r.patch
        );
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
