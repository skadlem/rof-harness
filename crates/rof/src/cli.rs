//! `rof run` flags: table-driven hand-rolled parsing plus the pure mappers
//! from parsed flags to run knobs (budget, `RunConfig`, endpoint).

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Instant;

use agent_budget::{config_for, with_steps, BudgetGuard, Capability};
use agent_loop::RunConfig;

use crate::workdir::resolve_log_path;

pub(crate) const USAGE: &str = "usage: rof run --goal TEXT --workdir DIR (--workdir must be a disposable dir: never /, $HOME, or the harness checkout) --model ID [--endpoint URL] [--api-key-env NAME] [--header NAME:VALUE] [--allow-cmd CMD...] [--budget-steps N] [--budget-actions N] [--budget-tokens N] [--max-tokens N] [--context-file PATH]... [--proof-cmd CMD] [--incentives LEVEL] [--bets] [--compaction FRAC] [--dump-events PATH] [--log-path PATH] [--no-log]
--dump-events PATH: JSONL, one event per line (LF); replaces any previous dump at PATH
--log-path PATH: fail-closed WAL (default <workdir>/.rof-events.jsonl)
--no-log: disable the WAL (run without a durable log)
--context-file PATH: pinned context; with neither --budget-tokens nor --budget-steps the run defaults to a 200000-token budget
--compaction FRAC: checkpoint the older context once the estimate crosses FRAC of the token budget (0 < FRAC <= 1). OFF-BY-DEFAULT and UNVALIDATED: the S-1 compaction experiment has not run yet, so leave it unset unless running that experiment.";

#[derive(Debug, PartialEq)]
pub(crate) struct Args {
    pub goal: String,
    pub workdir: PathBuf,
    pub model: String,
    pub endpoint: Option<String>,
    /// Env var holding the API key (default OPENAI_API_KEY; opencode-go uses
    /// GO_KEY). The key itself never appears in argv or logs.
    pub api_key_env: String,
    /// Extra request headers as NAME:VALUE (e.g. x-opencode-session:<id>).
    pub headers: Vec<String>,
    pub allow_cmd: Vec<String>,
    pub budget_steps: Option<u32>,
    pub budget_actions: Option<u32>,
    pub budget_tokens: Option<u64>,
    pub max_tokens: Option<usize>,
    pub context_files: Vec<String>,
    pub dump_events: Option<String>,
    /// WAL override (`None` = default inside `--workdir`); `--no-log` wins.
    pub log_path: Option<String>,
    /// Opt out of the fail-closed WAL.
    pub no_log: bool,
    pub proof_cmd: Option<String>,
    pub incentives: agent_loop::IncentivesLevel,
    pub bets: bool,
    /// Compaction trigger fraction; `None` = checkpoint off (default).
    pub compaction: Option<f64>,
}

pub(crate) fn parse_args(argv: &[String]) -> Result<Args, String> {
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
pub(crate) fn budget_for(args: &Args) -> BudgetGuard {
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

/// One run's loop config from the parsed flags. The compaction checkpoint is
/// opt-in: absent `--compaction` leaves the default disabled config, so the
/// run stays byte-identical. The WAL is the opposite: default on inside
/// `--workdir` (fail-closed on real runs, not only tests).
pub(crate) fn run_config(args: &Args) -> RunConfig {
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

pub(crate) fn resolve_endpoint(flag: Option<&str>, env: Option<&str>) -> Result<String, String> {
    flag.or(env)
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "no endpoint: pass --endpoint URL or set OPENAI_BASE_URL".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::argv;
    use std::path::Path;

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
    fn compaction_flag_reaches_run_config_off_by_default() {
        use crate::fixtures::args_for;
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
        use crate::fixtures::args_for;
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
}
