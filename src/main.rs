use rof::config::AppConfig;
use rof::engine::{ModelRouter, Orchestrator, Role, Session};
use rof::eval::{EvalSuite, EvaluationRunner};
use rof::llm::{ContextService, ExecutorService, LlmClient, OpenRouterClient, StubClient};
use rof::obs::TraceSink;
use rof::tools::ToolRegistry;
use std::path::PathBuf;
use std::sync::Arc;

struct Setup {
    cfg: AppConfig,
    trace: Arc<TraceSink>,
    context: ContextService,
    executor: ExecutorService,
    registry: ToolRegistry,
    root: PathBuf,
}

/// Precedence: defaults < config file (--config or ROF_CONFIG) < env vars.
/// Env wins so a one-off A/B does not require editing a checked-in config.
fn load_config(cli_path: Option<&str>) -> anyhow::Result<AppConfig> {
    let path = cli_path
        .map(str::to_string)
        .or_else(|| std::env::var("ROF_CONFIG").ok())
        .filter(|p| !p.trim().is_empty());
    let mut cfg = match path {
        Some(p) => {
            let cfg = AppConfig::load(std::path::Path::new(p.trim()))?;
            eprintln!("config: {}", p.trim());
            cfg
        }
        None => AppConfig::default(),
    };
    apply_env(&mut cfg);
    Ok(cfg)
}

/// Pull the `--config <path>` flag out of the argv tail.
fn config_arg(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--config" {
            return it.next().cloned();
        }
        if let Some(rest) = a.strip_prefix("--config=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Pull `--jobs <n>` out of the argv tail; None when absent or unparseable.
fn jobs_arg(args: &[String]) -> Option<usize> {
    num_arg(args, "--jobs")
}

/// Pull `--limit <n>` (first N suite tasks) out of the argv tail.
fn limit_arg(args: &[String]) -> Option<usize> {
    num_arg(args, "--limit")
}

fn num_arg(args: &[String], flag: &str) -> Option<usize> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().and_then(|v| v.trim().parse::<usize>().ok());
        }
        if let Some(rest) = a.strip_prefix(&format!("{flag}=")) {
            return rest.trim().parse::<usize>().ok();
        }
    }
    None
}

/// Pull `--report <path>`: write the suite report as JSON next to the trace,
/// so per-task verdicts survive the terminal they were printed to.
fn report_arg(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--report" {
            return it.next().cloned();
        }
        if let Some(rest) = a.strip_prefix("--report=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Env overrides. Seeded empty strings are ignored so `VAR= rof ...` is a no-op.
fn apply_env(cfg: &mut AppConfig) {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    if let Some(m) = get("ROF_CTX_MODEL") {
        cfg.routing.context_model = m;
    }
    if let Some(m) = get("ROF_EXEC_MODEL") {
        cfg.routing.executor_model = m;
    }
    if let Some(m) = get("ROF_EXEC_FALLBACK") {
        cfg.routing.executor_fallback = Some(m);
    }
    // Opt-in allowlists (comma separated; empty = deny all).
    if let Some(cmds) = get("ROF_ALLOW_CMDS") {
        cfg.permissions.allowed_commands = split_list(&cmds);
    }
    if let Some(hosts) = get("ROF_ALLOW_HOSTS") {
        cfg.permissions.allowed_hosts = split_list(&hosts);
    }
    if let Some(t) = get("ROF_MAX_TASK_TOKENS") {
        if let Ok(n) = t.trim().parse::<u64>() {
            cfg.max_tokens_per_task = n;
        }
    }
    if let Some(r) = get("ROF_MAX_ROUNDS") {
        if let Ok(n) = r.trim().parse::<u32>() {
            cfg.max_review_rounds = n;
        }
    }
    // Planner A/B lever: "skip" treats every goal as a single task.
    if let Some(p) = get("ROF_PLANNER") {
        if matches!(p.trim(), "skip" | "always") {
            cfg.planner = p.trim().to_string();
        }
    }
    if let Some(l) = get("ROF_COST_LAMBDA") {
        if let Ok(v) = l.trim().parse::<f64>() {
            cfg.cost_lambda = v;
        }
    }
    // Analysis-only goals set this to "no"; coding goals leave it at "yes".
    if let Some(w) = get("ROF_EXPECT_WRITES") {
        cfg.expect_writes = matches!(w.trim().to_ascii_lowercase().as_str(), "yes" | "true" | "1");
    }
    // Suite fan-out: --jobs N wins, then ROF_JOBS, then ROF_MAX_PARALLEL_TASKS.
    if let Some(j) = get("ROF_MAX_PARALLEL_TASKS") {
        if let Ok(n) = j.trim().parse::<usize>() {
            cfg.max_parallel_tasks = n.max(1);
        }
    }
    if let Some(j) = get("ROF_JOBS") {
        if let Ok(n) = j.trim().parse::<usize>() {
            cfg.max_parallel_tasks = n.max(1);
        }
    }
    // Task copies: keep them on a disk-backed path for suite runs whose
    // checks build (each copy grows its own target/). Deleting them is `rm -rf`.
    if let Some(r) = get("ROF_TASK_ROOT") {
        cfg.task_root = Some(std::path::PathBuf::from(r.trim()));
    }
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

fn setup(cfg: AppConfig) -> anyhow::Result<Setup> {
    let mut cfg = cfg;
    let root = match std::env::var("ROF_WORKDIR") {
        Ok(w) if !w.trim().is_empty() => PathBuf::from(w.trim()),
        _ => std::env::current_dir()?,
    };
    let root = root.canonicalize()?;
    // Default policy denies everything; anchor the allowlist to the workdir.
    cfg.permissions.allowed_dirs = vec![root.clone()];
    let router = ModelRouter::from_config(&cfg.routing);
    let trace = match std::env::var("ROF_TRACE") {
        Ok(p) if !p.trim().is_empty() => TraceSink::with_file(std::path::Path::new(p.trim()))?,
        _ => TraceSink::new(),
    };
    let trace = Arc::new(trace);

    // Direct provider endpoint first (your own keys), then OpenRouter,
    // then offline stub. Agents only see Context/Executor services.
    let real: Option<Arc<dyn LlmClient>> = OpenRouterClient::from_compat_env()
        .map(|c| Arc::new(c) as Arc<dyn LlmClient>)
        .or_else(|| OpenRouterClient::from_env().map(|c| Arc::new(c) as Arc<dyn LlmClient>));
    let (ctx_model, _) = router.resolve(Role::Context);
    let (exec_model, exec_fb) = router.resolve(Role::Executor);
    let (context, executor) = match &real {
        Some(c) => {
            let via = std::env::var("ROF_CHAT_BASE").unwrap_or("openrouter".to_string());
            println!("llm: {via} (ctx={ctx_model}, exec={exec_model})");
            (
                ContextService::new(c.clone(), ctx_model.to_string()),
                ExecutorService::new(
                    c.clone(),
                    exec_model.to_string(),
                    exec_fb.map(str::to_string),
                ),
            )
        }
        None => {
            println!("llm: stub (set OR_TOKEN for live models)");
            (
                ContextService::new(Arc::new(StubClient), "context-stub".to_string()),
                ExecutorService::new(Arc::new(StubClient), "executor-stub".to_string(), None),
            )
        }
    };

    let registry = ToolRegistry::with_defaults(root.clone(), cfg.permissions.clone());
    Ok(Setup {
        cfg,
        trace,
        context,
        executor,
        registry,
        root,
    })
}

fn print_report(trace: &TraceSink) {
    let mut report = rof::eval::EvalReport::default();
    for ev in trace.events() {
        report.fold(&ev);
    }
    println!(
        "trace events: {} | tool accuracy: {:.0}% | tokens in/out: {}/{} | cache-hit input: {:.0}% | latency: {}ms",
        trace.len(),
        report.tool_accuracy() * 100.0,
        report.est_input_tokens,
        report.est_output_tokens,
        report.cache_hit_rate() * 100.0,
        report.total_latency_ms,
    );
    println!(
        "cost: est ${:.5} (table) | provider-reported: {} | success {:.0}% | utility {:.3}",
        report.est_cost_usd,
        match report.cost_usd {
            Some(c) => format!("${c:.5} ({} calls)", report.cost_samples),
            None => "unreported".to_string(),
        },
        report.success_rate() * 100.0,
        report.utility(),
    );
    if !report.tokens_by_agent.is_empty() {
        let parts: Vec<String> = report
            .tokens_by_agent
            .iter()
            .map(|(a, t)| format!("{a}={t}"))
            .collect();
        println!("input tokens by agent: {}", parts.join(" "));
    }
    if report.retried_calls > 0 || report.model_errors > 0 {
        println!(
            "reliability: {} retried calls (max {} attempts), {} model errors",
            report.retried_calls, report.max_attempts, report.model_errors
        );
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cfg_path = config_arg(&args[2.min(args.len())..]);
    match args.get(1).map(String::as_str) {
        // Effective config as canonical JSON: save it, diff it, A/B it.
        Some("config") => {
            let cfg = load_config(cfg_path.as_deref())?;
            eprintln!(
                "# effective config (defaults < file < env). Save with: rof config > cfg.json"
            );
            eprintln!("# allowed_dirs is anchored to the workdir at run time.");
            println!("{}", cfg.to_json());
            Ok(())
        }
        Some("eval") => {
            let file = args
                .get(2)
                .cloned()
                .unwrap_or("eval/suites/sample.json".to_string());
            let mut suite = EvalSuite::load(std::path::Path::new(&file))?;
            // A baseline run is the first N tasks: cheaper than a subset suite
            // file that drifts from the real one.
            if let Some(n) = limit_arg(&args[2.min(args.len())..]) {
                suite.tasks.truncate(n);
            }
            let mut cfg = load_config(cfg_path.as_deref())?;
            if let Some(j) = jobs_arg(&args[2.min(args.len())..]) {
                cfg.max_parallel_tasks = j.max(1);
            }
            let jobs = cfg.max_parallel_tasks.max(1);
            let s = setup(cfg)?;
            let runner =
                EvaluationRunner::new(s.trace.clone(), s.cfg, s.context, s.executor, s.root);
            println!(
                "suite: {} ({} tasks, jobs={})",
                suite.name,
                suite.tasks.len(),
                jobs
            );
            let rep = runner.run_suite_with_jobs(&suite, jobs).await;
            for t in &rep.tasks {
                println!(
                    "  [{}] {} rounds={} {}",
                    if t.matched { "OK" } else { "MISMATCH" },
                    t.name,
                    t.rounds,
                    if t.matched { "" } else { &t.feedback }
                );
            }
            println!(
                "matched {}/{} ({:.0}%) | tool accuracy: {:.0}% | tokens in/out {}/{} | cache-hit {:.0}% | utility {:.3}",
                rep.matched(),
                rep.tasks.len(),
                rep.success_rate() * 100.0,
                rep.aggregate.tool_accuracy() * 100.0,
                rep.aggregate.est_input_tokens,
                rep.aggregate.est_output_tokens,
                rep.aggregate.cache_hit_rate() * 100.0,
                rep.aggregate.utility(),
            );
            println!(
                "cost: est ${:.5} (table) | provider-reported {}",
                rep.aggregate.est_cost_usd,
                match rep.aggregate.cost_usd {
                    Some(c) => format!("${c:.5} ({} calls)", rep.aggregate.cost_samples),
                    None => "unreported".to_string(),
                }
            );
            if !rep.aggregate.tokens_by_agent.is_empty() {
                let parts: Vec<String> = rep
                    .aggregate
                    .tokens_by_agent
                    .iter()
                    .map(|(a, t)| format!("{a}={t}"))
                    .collect();
                println!("input tokens by agent: {}", parts.join(" "));
            }
            if rep.aggregate.budget_aborts > 0 {
                println!("budget aborts: {}", rep.aggregate.budget_aborts);
            }
            if rep.aggregate.retried_calls > 0 || rep.aggregate.model_errors > 0 {
                println!(
                    "reliability: {} retried calls (max {} attempts), {} model errors",
                    rep.aggregate.retried_calls,
                    rep.aggregate.max_attempts,
                    rep.aggregate.model_errors
                );
            }
            if rep.aggregate.model_errors > 0 {
                println!("per-agent input tokens: see trace (model errors present)");
            }
            if let Some(path) = report_arg(&args[2.min(args.len())..]) {
                // The report is the durable half of a run: the terminal scrolls,
                // a JSON file can be diffed against the next baseline.
                std::fs::write(&path, serde_json::to_string_pretty(&rep)?)?;
                println!("report: {path}");
            }
            Ok(())
        }
        Some("run") => {
            let goal = args
                .get(2..)
                .map(|a| a.join(" "))
                .filter(|g| !g.is_empty())
                .unwrap_or("demo goal".to_string());
            run_goal(&goal, cfg_path.as_deref()).await
        }
        _ => {
            let goal = args
                .get(1..)
                .map(|a| a.join(" "))
                .filter(|g| !g.is_empty())
                .unwrap_or("demo goal".to_string());
            run_goal(&goal, cfg_path.as_deref()).await
        }
    }
}

async fn run_goal(goal: &str, config_path: Option<&str>) -> anyhow::Result<()> {
    let s = setup(load_config(config_path)?)?;
    // Accept the same exact-allowlist check(s) in run mode.
    let checks: Vec<String> = std::env::var("ROF_CHECK")
        .map(|c| {
            c.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let expect_writes = s.cfg.expect_writes;
    let orch = Orchestrator::new(s.cfg, s.trace.clone(), s.context, s.executor);
    let out = orch
        .run_loop(
            &Session::new(goal.to_string())
                .with_checks(checks)
                .expecting_writes(expect_writes),
            &s.registry,
            &s.root,
        )
        .await;
    println!("goal: {goal}");
    println!("result: {}", serde_json::to_string_pretty(&out)?);
    print_report(&s.trace);
    Ok(())
}
