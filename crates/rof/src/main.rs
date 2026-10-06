//! Minimal headless driver: `rof run ...` wires the frozen siblings once.
//! Prompt shape (goal + file map + named files via the context assembler)
//! lives in `agent_loop::run`; this crate only feeds it `RunConfig` + input.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use agent_budget::{config_for, with_steps, BudgetGuard, Capability};
use agent_event::{AgentEvent, Emitter};
use agent_loop::{Input, LoopState, NoBets, Outcome, Run, RunConfig};
use provider_core::LlmClient;
use provider_openai::{EndpointProfile, OpenAiCompat};
use snapshot::TreeService;
use tokio_util::sync::CancellationToken;
use tool_core::GrantGate;

const USAGE: &str = "usage: rof run --goal TEXT --workdir DIR --model ID [--endpoint URL] [--api-key-env NAME] [--header NAME:VALUE] [--allow-cmd CMD...] [--budget-steps N] [--budget-actions N] [--budget-tokens N] [--max-tokens N] [--context-file PATH]... [--proof-cmd CMD] [--incentives LEVEL] [--bets] [--compaction FRAC] [--dump-events PATH] [--log-path PATH] [--no-log]
--dump-events PATH: JSONL, one event per line (LF); replaces any previous dump at PATH
--log-path PATH: fail-closed WAL (default <workdir>/.rof-events.jsonl)
--no-log: disable the WAL (run without a durable log)
--context-file PATH: pinned context; with neither --budget-tokens nor --budget-steps the run defaults to a 200000-token budget
--compaction FRAC: checkpoint the older context once the estimate crosses FRAC of the token budget (0 < FRAC <= 1). OFF-BY-DEFAULT and UNVALIDATED: the S-1 compaction experiment has not run yet, so leave it unset unless running that experiment.";

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
    /// WAL override (`None` = default inside `--workdir`); `--no-log` wins.
    log_path: Option<String>,
    /// Opt out of the fail-closed WAL.
    no_log: bool,
    proof_cmd: Option<String>,
    incentives: agent_loop::IncentivesLevel,
    bets: bool,
    /// Compaction trigger fraction; `None` = checkpoint off (default).
    compaction: Option<f64>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let a: Vec<&str> = argv.iter().map(String::as_str).collect();
    if a.len() < 2 || a[1] != "run" {
        return Err(USAGE.into());
    }
    let (mut goal, mut workdir, mut model, mut endpoint, mut steps, mut dump) =
        (None, None, None, None, None, None);
    let mut log_path: Option<String> = None;
    let mut no_log = false;
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
    let mut compaction: Option<f64> = None;
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
            "--log-path" => log_path = Some(take(&mut i, inline)?),
            "--no-log" => {
                no_log = true;
            }
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
            "--compaction" => {
                let s = take(&mut i, inline)?;
                let f: f64 = s
                    .parse()
                    .map_err(|_| format!("--compaction needs a fraction in (0, 1], got {s:?}"))?;
                if !f.is_finite() || f <= 0.0 || f > 1.0 {
                    return Err(format!(
                        "--compaction needs a fraction in (0, 1], got {s:?}"
                    ));
                }
                compaction = Some(f);
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
        log_path,
        no_log,
        proof_cmd,
        incentives,
        bets,
        compaction,
    })
}

/// Budget for one run: explicit flags win; context-pinned runs without a
/// token flag get the pilot default; otherwise the library UnattendedBatch
/// config (50k tokens). The step override is `agent_budget::with_steps`
/// (warn clamp + refund re-derive live there, never here); precedence
/// (cfg-over-state) is documented on `agent_loop::run`.
fn budget_for(args: &Args) -> BudgetGuard {
    let mut b = config_for(Capability::UnattendedBatch);
    if let Some(n) = args.budget_steps {
        b = with_steps(b, NonZeroU32::new(n).expect("parse rejects 0"));
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

/// WAL filename for the shipped path: the fail-closed log lives inside
/// `--workdir` so a real run is durable without extra flags.
const DEFAULT_LOG_NAME: &str = ".rof-events.jsonl";

/// Default WAL path for one run: inside `--workdir` (which `main` already
/// verified is a directory, so the open cannot fail on a missing parent).
fn default_log_path(workdir: &Path) -> PathBuf {
    workdir.join(DEFAULT_LOG_NAME)
}

/// Shipped-path WAL wiring: default on (inside `--workdir`), `--log-path`
/// overrides the location, `--no-log` disables it (wins over the override).
fn resolve_log_path(args: &Args) -> Option<PathBuf> {
    if args.no_log {
        return None;
    }
    if let Some(p) = args.log_path.as_deref() {
        return Some(PathBuf::from(p));
    }
    Some(default_log_path(&args.workdir))
}

/// Keep run-record sidecars (WAL + incremental dump) out of the reported
/// patch: `snapshot` baselines `git add -A` mid-run, so a sidecar under
/// `--workdir` would otherwise be committed and diffed into stdout.
/// Appends the workdir-relative path to the copy-local `.git/info/exclude`
/// (never a tracked file); paths outside the workdir cannot be staged.
/// Runs after `ensure()` (stale sidecars are removed before its commit), so
/// the mid-run baselines — not the initial commit — are what this excludes.
fn exclude_sidecar(workdir: &Path, path: &Path) {
    // Never create `.git` here: `snapshot::is_repo` is existence-based, so a
    // bare `.git/info/exclude` would fake a repo and skip `git init`.
    // Callers run this after `ensure()`; a missing `.git` means ensure
    // failed and the loop reports it — exclusion then stays a no-op.
    let git = workdir.join(".git");
    if !git.is_dir() {
        return;
    }
    let Ok(rel) = path.strip_prefix(workdir) else {
        return;
    };
    let rel = rel.to_string_lossy().replace('\\', "/");
    if rel.is_empty() {
        return;
    }
    let exclude = git.join("info/exclude");
    if let Some(dir) = exclude.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let text = std::fs::read_to_string(&exclude).unwrap_or_default();
    if text.lines().any(|l| l == rel) {
        return;
    }
    let mut out = text;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&rel);
    out.push('\n');
    let _ = std::fs::write(&exclude, out);
}

/// One run's loop config from the parsed flags. The compaction checkpoint is
/// opt-in: absent `--compaction` leaves the default disabled config, so the
/// run stays byte-identical. The WAL is the opposite: default on inside
/// `--workdir` (fail-closed on real runs, not only tests).
fn run_config(args: &Args) -> RunConfig {
    let mut cfg = RunConfig {
        goal: args.goal.clone(),
        model: args.model.clone(),
        context_files: args.context_files.clone(),
        max_tokens: args.max_tokens.unwrap_or(8_000),
        incentives: args.incentives,
        proof_cmd: args.proof_cmd.clone(),
        ..RunConfig::default()
    };
    cfg.log_path = resolve_log_path(args);
    if let Some(frac) = args.compaction {
        cfg.compaction.enabled = true;
        cfg.compaction.frac = frac;
    }
    cfg
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

/// Deliverable patch: `Ok` is the full uncut run patch for stdout (never the
/// 8KiB/200-line evidence bound); `Err` means no start HEAD was recorded
/// (unborn HEAD / empty workdir) and the run must not exit 0.
struct RunResult {
    outcome: Outcome,
    patch: Result<String, String>,
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
    // Pre-seeded budget: `run` never rebuilds it (precedence is documented
    // on `agent_loop::run`: cfg-over-state).
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
    // One run, one log / one dump: truncate stale sidecars BEFORE the initial
    // `ensure()` commit so they are never staged into it. The WAL truncate is
    // load-bearing beyond git: `seq` restarts per run while `LogWriter` only
    // appends, so a kept stale log would fail gapless-from-1 validation — the
    // shipped path replaces it, same documented deal as `--dump-events`.
    let log_path = resolve_log_path(args);
    if let Some(p) = log_path.as_deref() {
        let _ = std::fs::remove_file(p);
    }
    if let Some(path) = args.dump_events.as_deref() {
        let _ = std::fs::remove_file(path);
    }
    // Freeze the run-start HEAD `patch_since_start_full` diffs from; the loop's
    // own ensure() is idempotent and reports the same failure as a run error.
    let tree = TreeService::new(&args.workdir);
    let _ = tree.ensure();
    // Git-exclude after `ensure()`, when `.git` is real: mid-run baselines
    // `git add -A`, and without this the live sidecars would be committed
    // and diffed into the reported patch. A failed dump attach falls through
    // to `finalize_dump`, which reports the error post-run exactly as the
    // old single-write path did; the run itself proceeds.
    if let Some(p) = log_path.as_deref() {
        exclude_sidecar(&args.workdir, p);
    }
    if let Some(path) = args.dump_events.as_deref() {
        exclude_sidecar(&args.workdir, Path::new(path));
        let _ = attach_dump(&mut emitter, path);
    }
    let outcome = agent_loop::run(
        &mut state,
        Run {
            provider,
            registry: &reg,
            agent: "agent",
            workdir: &args.workdir,
            emitter: &mut emitter,
            bets: bets_hook,
            cfg: run_config(args),
        },
        vec![Input::User(args.goal.clone())],
        &cancel,
    )
    .await;
    // The per-Dispatch baseline commits absorb what landed, so the index-based
    // `tree.diff()` reports an empty patch; report the whole run instead.
    // Full uncut deliverable: no 8KiB/200-line evidence bound, new-file
    // contents included. `Err` is unborn HEAD / empty workdir (no baseline).
    let patch = tree.patch_since_start_full().map_err(|e| e.to_string());
    // Success exits 0 only when the patch computed: a `Done` run with no
    // baseline becomes a failure outcome, so the existing exit-code path
    // reports non-zero and `main` prints the stderr warning.
    let outcome = match (outcome, &patch) {
        (Outcome::Done, Err(e)) => Outcome::Failed(format!("no baseline: {e}")),
        (outcome, _) => outcome,
    };
    eprintln!("ablation {:?}", state.ablation);
    eprintln!("events {}", summarize(emitter.history()));
    RunResult {
        outcome,
        patch,
        events: emitter.history().to_vec(),
    }
}

/// Incremental `--dump-events` mirror: truncate the previous dump (one run,
/// one dump, same as before), then append each emitted event as one JSON
/// line with flush + fsync before returning, so a kill leaves a valid
/// prefix on disk instead of no file at all. The listener is sync and
/// infallible by construction (every failure is skipped, never panics), so
/// a slow disk cannot hang or break the run; the WAL stays the fail-closed
/// record, this file its best-effort mirror.
fn attach_dump(emitter: &mut Emitter, path: &str) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    if let Some(dir) = Path::new(path).parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let shared = Arc::new(Mutex::new(file));
    emitter.on(move |event| {
        if let Ok(line) = serde_json::to_string(event) {
            if let Ok(mut f) = shared.lock() {
                use std::io::Write as _;
                let mut buf = line.into_bytes();
                buf.push(b'\n');
                let _ = f.write_all(&buf);
                let _ = f.flush();
                let _ = f.sync_all();
            }
        }
    });
    Ok(())
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

/// Post-run `--dump-events` close-out: the incremental listener is the
/// record, so when its line count already equals the lossless history the
/// kill-resilient prefix stands as-is (no truncating rewrite). Any
/// shortfall (attach failed, writes lost) falls back to the full replace
/// write, preserving the old path/flag/error behavior.
fn finalize_dump(path: &str, events: &[AgentEvent]) -> Result<(), String> {
    let complete = std::fs::read_to_string(path)
        .map(|t| t.lines().count() == events.len())
        .unwrap_or(false);
    if complete {
        return Ok(());
    }
    write_dump(path, events)
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
        if let Err(e) = finalize_dump(path, &r.events) {
            eprintln!("cannot write {path}: {e}");
            std::process::exit(2);
        }
    }
    match &r.patch {
        Ok(patch) if !patch.is_empty() => println!("{patch}"),
        Err(e) => eprintln!("warning: no baseline: {e}"),
        _ => {}
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
            "--log-path",
            "/tmp/w.log",
            "--compaction",
            "0.6",
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
                log_path: Some("/tmp/w.log".into()),
                no_log: false,
                bets: false,
                incentives: agent_loop::IncentivesLevel::Full,
                proof_cmd: None,
                compaction: Some(0.6),
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
        assert_eq!((b.log_path, b.no_log), (None, false));
        assert_eq!(b.compaction, None, "absent --compaction stays off");
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
            vec![
                "rof",
                "run",
                "--goal",
                "g",
                "--workdir",
                "/w",
                "--model",
                "m",
                "--compaction",
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
                "--compaction",
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
                "--compaction",
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
                "--compaction",
                "1.5",
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
        /// Mid-run probe: when set, the first `complete` records whether the
        /// dump file already holds JSON lines — proving the dump is
        /// incremental, not a post-run write.
        probe_dump: Option<PathBuf>,
        probe_hit: std::sync::Mutex<bool>,
    }

    impl ScriptClient {
        fn new(resps: Vec<Response>) -> Self {
            Self {
                queue: std::sync::Mutex::new(resps.into()),
                probe_dump: None,
                probe_hit: std::sync::Mutex::new(false),
            }
        }

        fn with_dump_probe(resps: Vec<Response>, dump: PathBuf) -> Self {
            Self {
                queue: std::sync::Mutex::new(resps.into()),
                probe_dump: Some(dump),
                probe_hit: std::sync::Mutex::new(false),
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
            if let Some(dump) = &self.probe_dump {
                if let Ok(text) = std::fs::read_to_string(dump) {
                    if text.lines().count() >= 1
                        && text
                            .lines()
                            .all(|l| serde_json::from_str::<serde_json::Value>(l).is_ok())
                    {
                        *self.probe_hit.lock().unwrap() = true;
                    }
                }
            }
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

    /// One edit call with caller-chosen search/replace (large-fix fixtures).
    fn edit_resp(path: &str, search: &str, replace: &str) -> Response {
        Response {
            message: AssistantMessage {
                content: "editing".into(),
                tool_calls: vec![ToolCallRef {
                    id: "c1".into(),
                    name: "edit".into(),
                    args: serde_json::json!({
                        "path": path,
                        "search": search,
                        "replace": replace,
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

    /// One whole-file write call (new-file deliverable fixtures).
    fn write_resp(path: &str, content: &str) -> Response {
        Response {
            message: AssistantMessage {
                content: "writing".into(),
                tool_calls: vec![ToolCallRef {
                    id: "c1".into(),
                    name: "write".into(),
                    args: serde_json::json!({
                        "path": path,
                        "content": content,
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
            log_path: None,
            no_log: false,
            bets: false,
            incentives: agent_loop::IncentivesLevel::Full,
            proof_cmd: None,
            compaction: None,
        }
    }

    #[test]
    fn compaction_flag_reaches_run_config_off_by_default() {
        let dir = Path::new("/tmp/w");
        let off = run_config(&args_for(dir, None));
        assert!(!off.compaction.enabled, "absent flag leaves compaction off");
        assert_eq!(off.compaction.frac, 0.7, "default frac untouched");
        assert_eq!(off.compaction.keep_tokens, 20_000, "keep stays the default");

        let mut on = args_for(dir, None);
        on.compaction = Some(0.5);
        let cfg = run_config(&on);
        assert!(
            cfg.compaction.enabled,
            "--compaction enables the checkpoint"
        );
        assert_eq!(cfg.compaction.frac, 0.5);
        assert_eq!(cfg.compaction.keep_tokens, 20_000, "keep stays the default");
    }

    #[test]
    fn log_path_defaults_inside_workdir_with_opt_out_and_override() {
        let dir = Path::new("/tmp/w");
        assert_eq!(
            resolve_log_path(&args_for(dir, None)),
            Some(dir.join(".rof-events.jsonl")),
            "shipped path defaults the WAL inside --workdir"
        );
        assert_eq!(
            run_config(&args_for(dir, None)).log_path,
            Some(dir.join(".rof-events.jsonl")),
            "real config carries the default (not RunConfig::default None)"
        );
        let mut over = args_for(dir, None);
        over.log_path = Some("/tmp/custom.jsonl".into());
        assert_eq!(
            run_config(&over).log_path,
            Some(PathBuf::from("/tmp/custom.jsonl"))
        );
        let mut off = args_for(dir, None);
        off.no_log = true;
        assert_eq!(run_config(&off).log_path, None);
        // Opt-out wins over an explicit override.
        off.log_path = Some("/tmp/custom.jsonl".into());
        assert_eq!(run_config(&off).log_path, None);
    }

    #[test]
    fn log_flags_parse_plain_and_eq_forms() {
        let a = parse_args(&argv(&[
            "rof",
            "run",
            "--goal",
            "g",
            "--workdir",
            "/w",
            "--model",
            "m",
            "--log-path",
            "/tmp/w.log",
        ]))
        .unwrap();
        assert_eq!(a.log_path.as_deref(), Some("/tmp/w.log"));
        assert!(!a.no_log);
        let b = parse_args(&argv(&[
            "rof",
            "run",
            "--goal",
            "g",
            "--workdir",
            "/w",
            "--model",
            "m",
            "--no-log",
        ]))
        .unwrap();
        assert!(b.no_log);
        assert_eq!(b.log_path, None);
        let c = parse_args(&argv(&[
            "rof",
            "run",
            "--goal=g",
            "--workdir=/w",
            "--model=m",
            "--log-path=/tmp/eq.log",
        ]))
        .unwrap();
        assert_eq!(c.log_path.as_deref(), Some("/tmp/eq.log"));
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
        // The step override is the single `agent_budget::with_steps`
        // implementation: warn clamps to min(warn, max-1), refunds re-derive.
        assert_eq!(budget_for(&pinned).config().warn_steps.get(), 4);
        assert_eq!(budget_for(&pinned).config().max_refunds, 1);
        assert_eq!(
            budget_for(&pinned).config().warn_steps,
            agent_budget::with_steps(
                agent_budget::config_for(agent_budget::Capability::UnattendedBatch),
                NonZeroU32::new(5).unwrap()
            )
            .warn_steps
        );
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
        let client = ScriptClient::new(vec![tool_resp(), second, text_resp(), text_resp()]);
        // The third response (first declare) is held by the verification nudge
        // (edits made, no passing test); the latch burns on fire, so the
        // fourth response lands Done.
        let r = execute(&client, &args_for(&dir, None)).await;
        assert!(matches!(r.outcome, Outcome::Done), "{:?}", r.outcome);
        assert_eq!(exit_code(&r.outcome), 0);
        assert_eq!(
            std::fs::read_to_string(dir.join("note.txt")).unwrap(),
            "later\n"
        );
        let patch = r.patch.unwrap();
        assert!(
            patch.contains("-hello") && patch.contains("+later"),
            "patch must span every batch, not just the last: {patch}",
        );
    }

    #[tokio::test]
    async fn deliverable_patch_is_full_uncut_large_fix() {
        // Evidence-bound regression: the old `patch_since_start` cut stdout
        // at 8KiB/200 lines with a truncation marker; the deliverable path
        // must carry the whole fix.
        let dir = tmp();
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        let big: String = (0..300)
            .map(|i| format!("line-{i:04}-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n"))
            .collect();
        assert!(
            big.len() > 8 * 1024,
            "fixture must exceed the old byte bound"
        );
        let client = ScriptClient::new(vec![
            edit_resp("note.txt", "hello", &big),
            text_resp(),
            text_resp(),
        ]);
        let r = execute(&client, &args_for(&dir, None)).await;
        assert!(matches!(r.outcome, Outcome::Done), "{:?}", r.outcome);
        let patch = r.patch.unwrap();
        assert!(
            patch.len() > 8 * 1024,
            "deliverable stays uncut: {} bytes",
            patch.len()
        );
        assert!(
            !patch.contains(snapshot::PATCH_TRUNCATED_MARKER),
            "no evidence-bound marker in the deliverable"
        );
        assert!(
            patch.contains("line-0000-") && patch.contains("line-0299-"),
            "head and tail of the large fix survive ({} bytes)",
            patch.len()
        );
    }

    #[tokio::test]
    async fn deliverable_patch_carries_new_file_contents() {
        // Untracked regression: the bounded evidence path emitted stub-only
        // headers for new files; the deliverable must carry their contents.
        let dir = tmp();
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        let client = ScriptClient::new(vec![
            write_resp(
                "brand-new.txt",
                "new-file-contents-marker-7f3a\nsecond line\n",
            ),
            text_resp(),
            text_resp(),
        ]);
        let r = execute(&client, &args_for(&dir, None)).await;
        assert!(matches!(r.outcome, Outcome::Done), "{:?}", r.outcome);
        let patch = r.patch.unwrap();
        assert!(
            patch.contains("brand-new.txt") && patch.contains("new-file-contents-marker-7f3a"),
            "new-file contents reach the stdout patch: {patch}"
        );
    }

    #[tokio::test]
    async fn unborn_head_is_a_no_baseline_failure_not_silent_empty_patch() {
        // Empty workdir: `ensure()` records no start HEAD, so the full-patch
        // fn errs; the run must warn on stderr (the same "no baseline"
        // message `main` prints) and exit non-zero via a failure outcome —
        // never a silent empty patch with exit 0.
        let dir = tmp();
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
        let client = ScriptClient::new(vec![text_resp(), text_resp()]);
        let r = execute(&client, &args_for(&dir, None)).await;
        assert!(r.patch.is_err(), "no baseline means no patch");
        let err = r.patch.unwrap_err();
        assert!(
            err.contains("start HEAD"),
            "snapshot names the cause: {err}"
        );
        assert!(
            matches!(&r.outcome, Outcome::Failed(m) if m.contains("no baseline")),
            "failure outcome carries the stderr hint: {:?}",
            r.outcome
        );
        assert_eq!(exit_code(&r.outcome), 3);
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
    async fn dump_events_exists_mid_run_and_wal_live() {
        let dir = tmp();
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        let dump = dir.join("dump.jsonl");
        let client = ScriptClient::with_dump_probe(vec![tool_resp(), text_resp()], dump.clone());
        let mut args = args_for(&dir, None);
        args.dump_events = Some(dump.to_str().unwrap().into());
        let r = execute(&client, &args).await;
        assert!(
            *client.probe_hit.lock().unwrap(),
            "dump file holds JSON lines before the first provider call returns"
        );
        assert!(!r.events.is_empty(), "fake run must produce events");
        let text = std::fs::read_to_string(&dump).unwrap();
        assert_eq!(
            text.lines().count(),
            r.events.len(),
            "incremental prefix is the complete record post-run"
        );
        assert!(
            dir.join(".rof-events.jsonl").is_file(),
            "default WAL is live on the shipped path"
        );
        let patch = r.patch.unwrap();
        assert!(
            !patch.contains("dump.jsonl") && !patch.contains(".rof-events.jsonl"),
            "sidecars stay out of the reported patch: {patch}",
        );
    }

    #[tokio::test]
    async fn admitted_step_text_answer_is_done() {
        let dir = tmp();
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        let client = ScriptClient::new(vec![text_resp()]);
        let r = execute(&client, &args_for(&dir, Some(1))).await;
        // Admitted-step-wins: the single permitted step ran and the model
        // answered with no further work, so it ends Done, not Halted.
        // (Halted is for runs that still need steps when the budget dies.)
        assert!(matches!(r.outcome, Outcome::Done), "{:?}", r.outcome);
        assert_eq!(exit_code(&r.outcome), 0);
    }
}
