//! `rof eval` over the frozen task slice: winnability pre-flight plus the
//! Docker-engine run and JSON report.

use std::path::{Path, PathBuf};

use crate::cli::{parse_table, resolve_endpoint, Flag};

pub(crate) const EVAL_USAGE: &str = "usage: rof eval --tasks-dir DIR [--ids a,b] [--out-dir DIR] [--report PATH] [--winnability-only] [--lenient-apply] [--agent] [--model ID] [--endpoint URL] [--api-key-env NAME] [--header NAME:VALUE] [--allow-cmd CMD...] [--budget-steps N] [--budget-actions N] [--budget-tokens N] [--max-tokens N] [--keep-thinking] [--in-container]
--lenient-apply: record Lenient patch-apply provenance in the report (default Strict; Strict grades are not comparable with earlier fuzz-lenient numbers)
--agent: run the agent leg per instance instead of the oracle-container leg (fresh workdir, in-process rof-run wiring, harbor-CLI grading). Requires --model ID and --endpoint URL (or OPENAI_BASE_URL); mutually exclusive with --winnability-only. Agent budgets default to the frozen pilot recipe: 300000 tokens / 60 steps / 120 actions; explicit flags win.
--keep-thinking: skip the thinking-off request knob for providers that reject it or when reasoning is wanted (default keeps the DeepSeek thinking-off knob, sent only on compaction-summary calls and the truncation ladder first rung; ordinary requests carry no thinking fragment)
--in-container: with --agent, the agent works inside the task environment container (started before the run; workdir bind-mounted at /app, cpus/memory capped at 2/4096m; agent exec/test calls wrapped with docker exec; the verifier execs the same live container, so installed packages and running services persist to grading). Verdict provenance in this mode is the verifier exit code, never the harbor result.json reward";

#[derive(Debug, PartialEq)]
pub(crate) struct EvalArgs {
    pub tasks_dir: PathBuf,
    pub ids: Vec<String>,
    pub out_dir: PathBuf,
    pub report: Option<PathBuf>,
    pub winnability_only: bool,
    /// Grade provenance: Strict (`git apply` only) or Lenient (with the
    /// `patch --fuzz=5` fallback). Strict by default, so reports are
    /// byte-identical to before the flag existed.
    pub lenient_apply: bool,
    /// Agent leg: per-instance in-process run + harbor grading.
    pub agent: bool,
    /// Agent endpoint URL, resolved at parse (`--endpoint` wins over
    /// `OPENAI_BASE_URL`; absent both is a parse error when `--agent`).
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub api_key_env: Option<String>,
    pub headers: Vec<String>,
    pub allow_cmd: Vec<String>,
    pub budget_steps: Option<u32>,
    pub budget_actions: Option<u32>,
    pub budget_tokens: Option<u64>,
    pub max_tokens: Option<usize>,
    /// Skip the thinking-off request body (`EndpointProfile` with
    /// `thinking_off: None`): for providers that reject the DeepSeek
    /// fragment or when reasoning is wanted. Default false keeps the
    /// current body byte-identical.
    pub keep_thinking: bool,
    /// In-container agent mode: the agent works inside the task
    /// environment container (workdir bind-mounted at /app) and the
    /// verifier execs the same live container, so installed packages and
    /// running services persist to grading. Requires `--agent`; default
    /// false keeps the harbor grade-task path byte-identical.
    pub in_container: bool,
}

#[derive(Default)]
struct EvalBuilder {
    tasks_dir: Option<String>,
    ids: Vec<String>,
    out_dir: Option<String>,
    report: Option<String>,
    winnability_only: bool,
    lenient_apply: bool,
    agent: bool,
    keep_thinking: bool,
    in_container: bool,
    model: Option<String>,
    endpoint: Option<String>,
    api_key_env: Option<String>,
    headers: Vec<String>,
    allow_cmd: Vec<String>,
    budget_steps: Option<u32>,
    budget_actions: Option<u32>,
    budget_tokens: Option<u64>,
    max_tokens: Option<usize>,
}

fn set_tasks_dir(b: &mut EvalBuilder, v: Option<String>) -> Result<(), String> {
    b.tasks_dir = v;
    Ok(())
}

fn set_ids(b: &mut EvalBuilder, v: Option<String>) -> Result<(), String> {
    b.ids = v
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    Ok(())
}

fn set_out_dir(b: &mut EvalBuilder, v: Option<String>) -> Result<(), String> {
    b.out_dir = v;
    Ok(())
}

fn set_report(b: &mut EvalBuilder, v: Option<String>) -> Result<(), String> {
    b.report = v;
    Ok(())
}

fn set_winnability_only(b: &mut EvalBuilder, _: Option<String>) -> Result<(), String> {
    b.winnability_only = true;
    Ok(())
}

fn set_lenient_apply(b: &mut EvalBuilder, _: Option<String>) -> Result<(), String> {
    b.lenient_apply = true;
    Ok(())
}

fn set_agent(b: &mut EvalBuilder, _: Option<String>) -> Result<(), String> {
    b.agent = true;
    Ok(())
}

fn set_model(b: &mut EvalBuilder, v: Option<String>) -> Result<(), String> {
    b.model = v;
    Ok(())
}

fn set_endpoint(b: &mut EvalBuilder, v: Option<String>) -> Result<(), String> {
    b.endpoint = v;
    Ok(())
}

fn set_api_key_env(b: &mut EvalBuilder, v: Option<String>) -> Result<(), String> {
    b.api_key_env = v;
    Ok(())
}

fn set_header(b: &mut EvalBuilder, v: Option<String>) -> Result<(), String> {
    // Parse-time rejection of a colon-less header (`rof run` fails later in
    // main; the eval agent leg gates at parse so a bad flag never starts a
    // slice spend).
    if v.as_deref().and_then(|h| h.split_once(':')).is_some() {
        b.headers.push(v.expect("colon check passed above"));
        Ok(())
    } else {
        Err(match v {
            Some(v) => format!("--header needs NAME:VALUE, got {v:?}"),
            None => "--header needs a value".into(),
        })
    }
}

fn set_allow_cmd(b: &mut EvalBuilder, v: Option<String>) -> Result<(), String> {
    if let Some(v) = v {
        b.allow_cmd.push(v);
    }
    Ok(())
}

fn set_budget_tokens(b: &mut EvalBuilder, v: Option<String>) -> Result<(), String> {
    let s = v.unwrap_or_default();
    b.budget_tokens = Some(
        s.parse()
            .map_err(|_| format!("--budget-tokens needs a positive integer, got {s:?}"))?,
    );
    Ok(())
}

fn set_budget_steps(b: &mut EvalBuilder, v: Option<String>) -> Result<(), String> {
    let s = v.unwrap_or_default();
    let n: u32 = s
        .parse()
        .map_err(|_| format!("--budget-steps needs a positive integer, got {s:?}"))?;
    if n == 0 {
        return Err("--budget-steps must be > 0".into());
    }
    b.budget_steps = Some(n);
    Ok(())
}

fn set_budget_actions(b: &mut EvalBuilder, v: Option<String>) -> Result<(), String> {
    let s = v.unwrap_or_default();
    let n: u32 = s
        .parse()
        .map_err(|_| format!("--budget-actions needs a positive integer, got {s:?}"))?;
    if n == 0 {
        return Err("--budget-actions must be > 0".into());
    }
    b.budget_actions = Some(n);
    Ok(())
}

fn set_max_tokens(b: &mut EvalBuilder, v: Option<String>) -> Result<(), String> {
    let s = v.unwrap_or_default();
    b.max_tokens = Some(
        s.parse()
            .map_err(|_| format!("--max-tokens needs a positive integer, got {s:?}"))?,
    );
    Ok(())
}

fn set_keep_thinking(b: &mut EvalBuilder, _: Option<String>) -> Result<(), String> {
    b.keep_thinking = true;
    Ok(())
}

fn set_in_container(b: &mut EvalBuilder, _: Option<String>) -> Result<(), String> {
    b.in_container = true;
    Ok(())
}

const EVAL_FLAGS: &[Flag<EvalBuilder>] = &[
    Flag {
        name: "--tasks-dir",
        takes_value: true,
        set: set_tasks_dir,
    },
    Flag {
        name: "--ids",
        takes_value: true,
        set: set_ids,
    },
    Flag {
        name: "--out-dir",
        takes_value: true,
        set: set_out_dir,
    },
    Flag {
        name: "--report",
        takes_value: true,
        set: set_report,
    },
    Flag {
        name: "--winnability-only",
        takes_value: false,
        set: set_winnability_only,
    },
    Flag {
        name: "--lenient-apply",
        takes_value: false,
        set: set_lenient_apply,
    },
    Flag {
        name: "--agent",
        takes_value: false,
        set: set_agent,
    },
    Flag {
        name: "--model",
        takes_value: true,
        set: set_model,
    },
    Flag {
        name: "--endpoint",
        takes_value: true,
        set: set_endpoint,
    },
    Flag {
        name: "--api-key-env",
        takes_value: true,
        set: set_api_key_env,
    },
    Flag {
        name: "--header",
        takes_value: true,
        set: set_header,
    },
    Flag {
        name: "--allow-cmd",
        takes_value: true,
        set: set_allow_cmd,
    },
    Flag {
        name: "--budget-steps",
        takes_value: true,
        set: set_budget_steps,
    },
    Flag {
        name: "--budget-actions",
        takes_value: true,
        set: set_budget_actions,
    },
    Flag {
        name: "--budget-tokens",
        takes_value: true,
        set: set_budget_tokens,
    },
    Flag {
        name: "--max-tokens",
        takes_value: true,
        set: set_max_tokens,
    },
    Flag {
        name: "--keep-thinking",
        takes_value: false,
        set: set_keep_thinking,
    },
    Flag {
        name: "--in-container",
        takes_value: false,
        set: set_in_container,
    },
];

pub(crate) fn parse_eval(argv: &[String]) -> Result<EvalArgs, String> {
    let b = parse_table(
        argv,
        "eval",
        EVAL_USAGE,
        |_| EVAL_USAGE.into(),
        EVAL_FLAGS,
        EvalBuilder::default(),
    )?;
    if b.agent && b.winnability_only {
        return Err("--agent and --winnability-only are mutually exclusive".into());
    }
    if b.in_container && !b.agent {
        return Err("--in-container requires --agent".into());
    }
    // Agent mode requires --model; the Option stays for the default path.
    let model = if b.agent {
        Some(b.model.ok_or("--agent requires --model ID")?)
    } else {
        b.model
    };
    // Agent endpoint resolution reuses the run semantics exactly
    // (`cli::resolve_endpoint`: flag wins, then OPENAI_BASE_URL, then a
    // parse error naming both).
    let endpoint = if b.agent {
        Some(resolve_endpoint(
            b.endpoint.as_deref(),
            std::env::var("OPENAI_BASE_URL").ok().as_deref(),
        )?)
    } else {
        b.endpoint
    };
    Ok(EvalArgs {
        tasks_dir: PathBuf::from(b.tasks_dir.ok_or("missing --tasks-dir")?),
        ids: b.ids,
        out_dir: PathBuf::from(b.out_dir.unwrap_or_else(|| "/tmp/rof-eval-out".into())),
        report: b.report.map(PathBuf::from),
        winnability_only: b.winnability_only,
        lenient_apply: b.lenient_apply,
        agent: b.agent,
        model,
        endpoint,
        api_key_env: b.api_key_env,
        headers: b.headers,
        allow_cmd: b.allow_cmd,
        budget_steps: b.budget_steps,
        budget_actions: b.budget_actions,
        budget_tokens: b.budget_tokens,
        max_tokens: b.max_tokens,
        keep_thinking: b.keep_thinking,
        in_container: b.in_container,
    })
}

/// Which patch applier grades this eval: Strict by default (byte-identical
/// reports to before the flag existed), Lenient with `--lenient-apply`.
pub(crate) fn apply_mode_for(args: &EvalArgs) -> eval::ApplyMode {
    if args.lenient_apply {
        eval::ApplyMode::Lenient
    } else {
        eval::ApplyMode::Strict
    }
}

fn eval_winnability_caps() -> eval::ToolCaps {
    eval::ToolCaps {
        can_create: true,
        can_patch: true,
        exec_prefixes: Vec::new(),
    }
}

fn eval_verdict_from_workdir(workdir: &Path) -> eval::Verdict {
    for name in ["reward.txt", "reward.json"] {
        let p = workdir.join(name);
        if p.is_file() {
            if let Ok(v) = eval::check_tb_reward(&p) {
                return v;
            }
        }
    }
    eval::Verdict::ErrorNoReport
}

pub(crate) async fn run_eval(args: &EvalArgs) -> i32 {
    let instances = match eval::load_tb_slice(&args.tasks_dir, &args.ids) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cannot load slice: {e}");
            return 2;
        }
    };
    let _excluded = eval::preflight(&instances);
    let caps = eval_winnability_caps();
    if args.agent {
        return crate::eval_agent::run_agent_leg(args, &instances, &caps).await;
    }
    let engine = eval::DockerEngine::new();
    let mut reports = Vec::new();
    for inst in &instances {
        let task_dir = args.tasks_dir.join(&inst.id);
        for bad in eval::check_winnability(&task_dir, &caps) {
            eprintln!("unwinnable {}: {bad}", inst.id);
        }
        if args.winnability_only {
            continue;
        }
        let work = args.out_dir.join(&inst.id);
        if std::fs::create_dir_all(&work).is_err() {
            eprintln!("cannot create {}", work.display());
            return 2;
        }
        let start = std::time::Instant::now();
        let verdict = match eval::Engine::run_instance(&engine, inst, &work) {
            Ok(outcome) => {
                if let Some(hit) = outcome.reward {
                    if hit {
                        eval::Verdict::Resolved
                    } else {
                        eval::Verdict::Unresolved
                    }
                } else {
                    eval_verdict_from_workdir(&work)
                }
            }
            Err(e) => {
                eprintln!("infra {}: {e}", inst.id);
                eval::Verdict::InfraFailure
            }
        };
        let wall = start.elapsed().as_secs();
        let events_path = work.join("events.jsonl");
        let _ = std::fs::write(&events_path, "");
        match eval::instance_report_with_mode(
            &inst.id,
            verdict,
            &events_path,
            wall,
            "",
            apply_mode_for(args),
            None,
        ) {
            Ok(r) => reports.push(r),
            Err(e) => {
                eprintln!("cannot report {}: {e}", inst.id);
                return 2;
            }
        }
    }
    finish_report(args, reports)
}

/// Shared report tail: JSON to `--report` (prettied) or stdout. Extracted so
/// the agent leg writes the exact same bytes through the same code.
pub(crate) fn finish_report(args: &EvalArgs, reports: Vec<eval::InstanceReport>) -> i32 {
    let report = eval::RunReport { instances: reports };
    if let Some(path) = args.report.as_deref() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(path, serde_json::to_string_pretty(&report).unwrap()) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("cannot write {}: {e}", path.display());
                return 2;
            }
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::argv;

    #[test]
    fn cli_eval_parses_tasks_dir_and_report() {
        let a = parse_eval(&argv(&[
            "rof",
            "eval",
            "--tasks-dir",
            "/tmp/tasks",
            "--ids",
            "aaa,bbb",
            "--out-dir",
            "/tmp/eval-out",
            "--report",
            "/tmp/eval-out/report.json",
        ]))
        .unwrap();
        assert_eq!(a.tasks_dir, PathBuf::from("/tmp/tasks"));
        assert_eq!(a.ids, vec!["aaa".to_string(), "bbb".to_string()]);
        assert_eq!(a.report, Some(PathBuf::from("/tmp/eval-out/report.json")));
        assert!(!a.winnability_only);
        assert!(!a.lenient_apply, "Strict provenance by default");
    }

    #[test]
    fn cli_eval_parses_keep_thinking() {
        let a = parse_eval(&argv(&[
            "rof",
            "eval",
            "--tasks-dir",
            "/tmp/tasks",
            "--agent",
            "--model",
            "m",
            "--endpoint",
            "https://e",
            "--keep-thinking",
        ]))
        .unwrap();
        assert!(a.keep_thinking);
        assert!(
            !parse_eval(&argv(&["rof", "eval", "--tasks-dir", "/tmp/tasks"]))
                .unwrap()
                .keep_thinking,
            "default keeps the thinking-off body"
        );
    }

    #[test]
    fn in_container_requires_agent_and_parses_with_it() {
        // Alone (no --agent): parse error.
        assert!(parse_eval(&argv(&[
            "rof",
            "eval",
            "--tasks-dir",
            "/tmp/t",
            "--in-container"
        ]))
        .is_err());
        // With --agent: parses, flag set.
        let a = parse_eval(&argv(&[
            "rof",
            "eval",
            "--tasks-dir",
            "/tmp/t",
            "--agent",
            "--model",
            "m",
            "--endpoint",
            "http://x",
            "--in-container",
        ]))
        .unwrap();
        assert!(a.in_container);
        // Default off: the harbor grade-task path stays byte-identical.
        let plain = parse_eval(&argv(&[
            "rof",
            "eval",
            "--tasks-dir",
            "/tmp/t",
            "--agent",
            "--model",
            "m",
            "--endpoint",
            "http://x",
        ]))
        .unwrap();
        assert!(!plain.in_container);
    }

    #[test]
    fn cli_eval_parses_lenient_apply() {
        let a = parse_eval(&argv(&[
            "rof",
            "eval",
            "--tasks-dir",
            "/tmp/tasks",
            "--lenient-apply",
        ]))
        .unwrap();
        assert!(a.lenient_apply);
        assert_eq!(apply_mode_for(&a), eval::ApplyMode::Lenient);
        let strict = parse_eval(&argv(&["rof", "eval", "--tasks-dir", "/tmp/tasks"])).unwrap();
        assert_eq!(apply_mode_for(&strict), eval::ApplyMode::Strict);
    }

    #[test]
    fn eval_report_provenance_matches_mode() {
        // instance_report_with_mode is what run_eval calls: Strict default is
        // the old instance_report shape, Lenient says Lenient in JSON.
        let dir = std::env::temp_dir().join(format!("rof-eval-mode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let events = dir.join("events.jsonl");
        std::fs::write(
            &events,
            "{\"type\":\"RunEnd\",\"outcome\":\"Passed\",\"messages\":[]}\n",
        )
        .unwrap();
        for (mode, want) in [
            (eval::ApplyMode::Strict, "Strict"),
            (eval::ApplyMode::Lenient, "Lenient"),
        ] {
            let r = eval::instance_report_with_mode(
                "t1",
                eval::Verdict::Resolved,
                &events,
                1,
                "",
                mode,
                None,
            )
            .unwrap();
            let v = serde_json::to_value(&r).unwrap();
            assert_eq!(v["apply_mode"], want);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn eval_winnability_only_reports_violations_without_docker() {
        let root = std::env::temp_dir().join(format!("rof-eval-test-{}", std::process::id()));
        let tasks = root.join("tasks");
        std::fs::create_dir_all(tasks.join("aaa").join("solution")).unwrap();
        std::fs::create_dir_all(tasks.join("aaa").join("tests")).unwrap();
        std::fs::write(
            tasks.join("aaa").join("task.toml"),
            "image = \"img\"\ntimeout = 60\n",
        )
        .unwrap();
        std::fs::write(tasks.join("aaa").join("instruction.md"), "# aaa\nDo it.\n").unwrap();
        std::fs::write(
            tasks.join("aaa").join("solution").join("solve.sh"),
            "#!/bin/sh\necho ok\n",
        )
        .unwrap();
        std::fs::write(
            tasks.join("aaa").join("tests").join("test.sh"),
            "#!/bin/sh\nexit 0\n",
        )
        .unwrap();
        let args = EvalArgs {
            tasks_dir: tasks.clone(),
            ids: vec![],
            out_dir: root.join("out"),
            report: Some(root.join("out").join("report.json")),
            winnability_only: true,
            lenient_apply: false,
            agent: false,
            endpoint: None,
            model: None,
            api_key_env: None,
            headers: Vec::new(),
            allow_cmd: Vec::new(),
            budget_steps: None,
            budget_actions: None,
            budget_tokens: None,
            max_tokens: None,
            keep_thinking: false,
            in_container: false,
        };
        let code = run_eval(&args).await;
        assert_eq!(code, 0);
        let raw = std::fs::read_to_string(args.report.unwrap()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(v.get("instances").and_then(|x| x.as_array()).is_some());
        std::fs::remove_dir_all(&root).ok();
    }

    /// Process env is process-wide: serialize the OPENAI_BASE_URL tests.
    static EVAL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn eval_min_agent() -> Vec<String> {
        argv(&[
            "rof",
            "eval",
            "--tasks-dir",
            "/tmp/t",
            "--agent",
            "--model",
            "m",
        ])
    }

    #[test]
    fn eval_agent_flags_parse() {
        let a = parse_eval(&argv(&[
            "rof",
            "eval",
            "--tasks-dir",
            "/tmp/t",
            "--agent",
            "--model",
            "m",
            "--endpoint",
            "https://e/v1",
            "--api-key-env",
            "K_ENV",
            "--header",
            "A:b",
            "--allow-cmd",
            "cargo test",
        ]))
        .unwrap();
        assert!(a.agent);
        assert_eq!(a.model.as_deref(), Some("m"));
        assert_eq!(a.endpoint.as_deref(), Some("https://e/v1"));
        assert_eq!(a.api_key_env.as_deref(), Some("K_ENV"));
        assert_eq!(a.headers, vec!["A:b".to_string()]);
        assert_eq!(a.allow_cmd, vec!["cargo test".to_string()]);
        // --endpoint=eq form parses identically.
        let b = parse_eval(&argv(&[
            "rof",
            "eval",
            "--tasks-dir=/tmp/t",
            "--agent",
            "--model=m2",
            "--endpoint=http://x",
        ]))
        .unwrap();
        assert_eq!(b.model.as_deref(), Some("m2"));
    }

    #[test]
    fn eval_agent_parse_errors() {
        let _guard = EVAL_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var("OPENAI_BASE_URL").ok();
        std::env::remove_var("OPENAI_BASE_URL");

        // --agent without --model is a parse error (exit 2 shape).
        assert!(parse_eval(&argv(&["rof", "eval", "--tasks-dir", "/tmp/t", "--agent",])).is_err());
        // --agent and --winnability-only are exclusive.
        assert!(parse_eval(&argv(&[
            "rof",
            "eval",
            "--tasks-dir",
            "/tmp/t",
            "--agent",
            "--winnability-only",
        ]))
        .is_err());
        // Endpoint: neither flag nor env is a parse error naming both.
        let missing_endpoint = parse_eval(&eval_min_agent()).err().map(|e| e.to_string());
        let names_both = missing_endpoint
            .as_deref()
            .map(|e| e.contains("--endpoint") && e.contains("OPENAI_BASE_URL"))
            .unwrap_or(false);
        assert!(
            names_both,
            "error names both resolutions: {missing_endpoint:?}"
        );
        // Env fills an absent flag; an explicit flag wins.
        std::env::set_var("OPENAI_BASE_URL", "http://envbase/v1");
        assert_eq!(
            parse_eval(&eval_min_agent()).unwrap().endpoint.as_deref(),
            Some("http://envbase/v1")
        );
        let flag_wins = parse_eval(&argv(&[
            "rof",
            "eval",
            "--tasks-dir",
            "/tmp/t",
            "--agent",
            "--model=m",
            "--endpoint=http://flag/v1",
        ]))
        .unwrap();
        assert_eq!(flag_wins.endpoint.as_deref(), Some("http://flag/v1"));

        // A colon-less header is rejected at parse (`rof run` fails later;
        // the agent leg gates first so bad flags never start a spend).
        assert!(parse_eval(&argv(&[
            "rof",
            "eval",
            "--tasks-dir",
            "/tmp/t",
            "--agent",
            "--model=m",
            "--endpoint=http://x",
            "--header",
            "NO-COLON",
        ]))
        .is_err());

        // Budget flags parse as integers and reject 0/garbage like rof run.
        let budgets = parse_eval(&argv(&[
            "rof",
            "eval",
            "--tasks-dir",
            "/tmp/t",
            "--agent",
            "--model=m",
            "--endpoint=http://x",
            "--budget-steps",
            "9",
            "--budget-actions",
            "11",
            "--budget-tokens",
            "42",
            "--max-tokens",
            "77",
        ]))
        .unwrap();
        assert_eq!(
            (
                budgets.budget_steps,
                budgets.budget_actions,
                budgets.budget_tokens,
                budgets.max_tokens
            ),
            (Some(9), Some(11), Some(42), Some(77))
        );
        for bad in [
            vec!["--budget-steps", "0"],
            vec!["--budget-steps", "lots"],
            vec!["--budget-actions", "0"],
            vec!["--budget-actions", "lots"],
            vec!["--budget-tokens", "lots"],
        ] {
            let mut words = eval_min_agent();
            words.push("--endpoint=http://x".into());
            words.extend(bad.iter().map(|s| s.to_string()));
            assert!(parse_eval(&words).is_err(), "bad budget must fail: {bad:?}");
        }

        // Restore caller env.
        match saved {
            Some(v) => std::env::set_var("OPENAI_BASE_URL", v),
            None => std::env::remove_var("OPENAI_BASE_URL"),
        }
    }
}
