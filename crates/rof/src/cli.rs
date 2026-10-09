//! `rof run` flags: table-driven hand-rolled parsing plus the pure mappers
//! from parsed flags to run knobs (budget, `RunConfig`, endpoint).

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use agent_budget::{config_for, with_steps, BudgetGuard, Capability};
use agent_loop::RunConfig;
use provider_openai::EndpointProfile;

use crate::workdir::resolve_log_path;

pub(crate) const USAGE: &str = "usage: rof run --goal TEXT --workdir DIR (--workdir must be a disposable dir: never /, $HOME, or the harness checkout) --model ID [--endpoint URL] [--api-key-env NAME] [--header NAME:VALUE] [--allow-cmd CMD...] [--budget-steps N] [--budget-actions N] [--budget-tokens N] [--max-tokens N] [--context-file PATH]... [--proof-cmd CMD] [--incentives LEVEL] [--bets] [--compaction FRAC] [--dump-events PATH] [--log-path PATH] [--no-log] [--keep-thinking] [--thinking-keep N] [--collapse-hysteresis N] [--allow-dirty-workdir] [--pass-env NAME]...
--dump-events PATH: JSONL, one event per line (LF); replaces any previous dump at PATH
--log-path PATH: fail-closed WAL (default <workdir>/.rof-events.jsonl)
--no-log: disable the WAL (run without a durable log)
--keep-thinking: skip the thinking-off request knob for providers that reject it or when reasoning is wanted (default keeps the DeepSeek thinking-off knob, sent only on compaction-summary calls and the truncation ladder first rung; ordinary requests carry no thinking fragment)
--context-file PATH: pinned context; with neither --budget-tokens nor --budget-steps the run defaults to a 200000-token budget
--compaction FRAC: checkpoint the older context once the estimate crosses FRAC of the token budget (0 < FRAC <= 1). OFF-BY-DEFAULT and UNVALIDATED: the S-1 compaction experiment has not run yet, so leave it unset unless running that experiment.
--thinking-keep N: echoed-reasoning rows kept per assistant message (flag wins over THINKING_KEEP; unset leaves the loop default)
--collapse-hysteresis N: collapse-boundary hysteresis rows (flag wins over COLLAPSE_HYSTERESIS; unset leaves the loop default)
--allow-dirty-workdir: run inside a dirty git workdir (default: refuse; allowed dirt appears in the reported patch)
--pass-env NAME: forward env var to exec/test children (repeatable; provider-key-shaped names stay withheld at spawn)
exit codes: 0 done, 2 usage/parse, 3 run failure (halt/cancel/provider), 4 missing credentials, 5 log failure, 6 snapshot failure, 7 empty input";

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
    /// Send no thinking-off knob in the request body (`EndpointProfile`
    /// with `thinking_off: None`): for providers that reject the
    /// DeepSeek-specific fragment or when reasoning is wanted. Default
    /// false keeps the current body byte-identical.
    pub keep_thinking: bool,
    pub proof_cmd: Option<String>,
    pub incentives: agent_loop::IncentivesLevel,
    pub bets: bool,
    /// Compaction trigger fraction; `None` = checkpoint off (default).
    pub compaction: Option<f64>,
    /// Fold knobs: `None` = leave the loop default (`RunConfig::default`).
    /// Flags win over `THINKING_KEEP` / `COLLAPSE_HYSTERESIS`, resolved once
    /// at the edge by [`apply_env_defaults`]; the loop never reads the env.
    pub thinking_keep: Option<usize>,
    pub collapse_hysteresis: Option<usize>,
    /// Opt in to running inside a dirty git workdir (pre-existing changes
    /// then appear in the reported patch).
    pub allow_dirty_workdir: bool,
    /// Extra env names forwarded to `exec`/`test` children on top of the
    /// fixed pass-through set (repeatable).
    pub pass_env: Vec<String>,
    /// Exec-wrap argv prepended at spawn by the tool policy (in-container
    /// eval: `docker exec <container>` so exec/test calls land in the task
    /// environment container); `None` keeps host-side exec.
    pub(crate) exec_wrap: Option<Vec<String>>,
}

/// One flag definition: the long name, whether it takes a value, and the
/// setter that applies the value (`None` for bare bool flags) to the
/// builder. Table-driven so each flag is defined once; [`parse_table`] owns
/// `--flag value` / `--flag=value` splitting, missing-value errors, and
/// unknown-flag errors for both subcommands. No new dependency: the parser
/// stays hand-rolled.
pub(crate) struct Flag<B> {
    pub name: &'static str,
    pub takes_value: bool,
    pub set: fn(&mut B, Option<String>) -> Result<(), String>,
}

pub(crate) fn parse_table<B>(
    argv: &[String],
    cmd: &str,
    usage: &str,
    unknown: impl Fn(&str) -> String,
    table: &[Flag<B>],
    mut b: B,
) -> Result<B, String> {
    let a: Vec<&str> = argv.iter().map(String::as_str).collect();
    if a.len() < 2 || a[1] != cmd {
        return Err(usage.into());
    }
    let mut i = 2;
    while i < a.len() {
        let flag = a[i];
        let (k, inline) = match flag.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (flag, None),
        };
        let Some(def) = table.iter().find(|f| f.name == k) else {
            return Err(unknown(k));
        };
        let v = if def.takes_value {
            Some(match inline {
                Some(v) => v,
                None => {
                    i += 1;
                    a.get(i)
                        .map(|s| (*s).to_string())
                        .ok_or_else(|| format!("{k} needs a value"))?
                }
            })
        } else {
            None
        };
        (def.set)(&mut b, v)?;
        i += 1;
    }
    Ok(b)
}

#[derive(Default)]
struct RunBuilder {
    goal: Option<String>,
    workdir: Option<String>,
    model: Option<String>,
    endpoint: Option<String>,
    api_key_env: Option<String>,
    headers: Vec<String>,
    allow_cmd: Vec<String>,
    budget_steps: Option<u32>,
    budget_actions: Option<u32>,
    budget_tokens: Option<u64>,
    max_tokens: Option<usize>,
    context_files: Vec<String>,
    dump_events: Option<String>,
    log_path: Option<String>,
    no_log: bool,
    keep_thinking: bool,
    proof_cmd: Option<String>,
    incentives: Option<agent_loop::IncentivesLevel>,
    bets: bool,
    compaction: Option<f64>,
    thinking_keep: Option<usize>,
    collapse_hysteresis: Option<usize>,
    allow_dirty_workdir: bool,
    pass_env: Vec<String>,
}

fn set_goal(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.goal = v;
    Ok(())
}

fn set_workdir(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.workdir = v;
    Ok(())
}

fn set_model(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.model = v;
    Ok(())
}

fn set_endpoint(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.endpoint = v;
    Ok(())
}

fn set_dump_events(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.dump_events = v;
    Ok(())
}

fn set_log_path(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.log_path = v;
    Ok(())
}

fn set_no_log(b: &mut RunBuilder, _: Option<String>) -> Result<(), String> {
    b.no_log = true;
    Ok(())
}

fn set_keep_thinking(b: &mut RunBuilder, _: Option<String>) -> Result<(), String> {
    b.keep_thinking = true;
    Ok(())
}

fn set_allow_cmd(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    if let Some(v) = v {
        b.allow_cmd.push(v);
    }
    Ok(())
}

fn set_context_file(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    if let Some(v) = v {
        b.context_files.push(v);
    }
    Ok(())
}

fn set_max_tokens(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    let s = v.unwrap_or_default();
    b.max_tokens = Some(
        s.parse()
            .map_err(|_| format!("--max-tokens needs a positive integer, got {s:?}"))?,
    );
    Ok(())
}

fn set_budget_tokens(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    let s = v.unwrap_or_default();
    b.budget_tokens = Some(
        s.parse()
            .map_err(|_| format!("--budget-tokens needs a positive integer, got {s:?}"))?,
    );
    Ok(())
}

fn set_budget_steps(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
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

fn set_budget_actions(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
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

fn set_proof_cmd(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.proof_cmd = v;
    Ok(())
}

fn set_api_key_env(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.api_key_env = v;
    Ok(())
}

fn set_header(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    if let Some(v) = v {
        b.headers.push(v);
    }
    Ok(())
}

fn set_incentives(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.incentives = Some(match v.as_deref() {
        Some("base") => agent_loop::IncentivesLevel::Base,
        Some("contract") => agent_loop::IncentivesLevel::Contract,
        Some("full") => agent_loop::IncentivesLevel::Full,
        other => {
            let other = other.unwrap_or_default();
            return Err(format!(
                "--incentives needs base|contract|full, got {other:?}"
            ));
        }
    });
    Ok(())
}

fn set_bets(b: &mut RunBuilder, _: Option<String>) -> Result<(), String> {
    b.bets = true;
    Ok(())
}

fn set_compaction(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    let s = v.unwrap_or_default();
    let f: f64 = s
        .parse()
        .map_err(|_| format!("--compaction needs a fraction in (0, 1], got {s:?}"))?;
    if !f.is_finite() || f <= 0.0 || f > 1.0 {
        return Err(format!(
            "--compaction needs a fraction in (0, 1], got {s:?}"
        ));
    }
    b.compaction = Some(f);
    Ok(())
}

fn set_thinking_keep(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    let s = v.unwrap_or_default();
    b.thinking_keep = Some(
        s.parse()
            .map_err(|_| format!("--thinking-keep needs a positive integer, got {s:?}"))?,
    );
    Ok(())
}

fn set_collapse_hysteresis(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    let s = v.unwrap_or_default();
    b.collapse_hysteresis = Some(
        s.parse()
            .map_err(|_| format!("--collapse-hysteresis needs a positive integer, got {s:?}"))?,
    );
    Ok(())
}

fn set_allow_dirty_workdir(b: &mut RunBuilder, _: Option<String>) -> Result<(), String> {
    b.allow_dirty_workdir = true;
    Ok(())
}

fn set_pass_env(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    if let Some(v) = v {
        b.pass_env.push(v);
    }
    Ok(())
}

const RUN_FLAGS: &[Flag<RunBuilder>] = &[
    Flag {
        name: "--goal",
        takes_value: true,
        set: set_goal,
    },
    Flag {
        name: "--workdir",
        takes_value: true,
        set: set_workdir,
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
        name: "--dump-events",
        takes_value: true,
        set: set_dump_events,
    },
    Flag {
        name: "--log-path",
        takes_value: true,
        set: set_log_path,
    },
    Flag {
        name: "--no-log",
        takes_value: false,
        set: set_no_log,
    },
    Flag {
        name: "--keep-thinking",
        takes_value: false,
        set: set_keep_thinking,
    },
    Flag {
        name: "--allow-cmd",
        takes_value: true,
        set: set_allow_cmd,
    },
    Flag {
        name: "--context-file",
        takes_value: true,
        set: set_context_file,
    },
    Flag {
        name: "--max-tokens",
        takes_value: true,
        set: set_max_tokens,
    },
    Flag {
        name: "--budget-tokens",
        takes_value: true,
        set: set_budget_tokens,
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
        name: "--proof-cmd",
        takes_value: true,
        set: set_proof_cmd,
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
        name: "--incentives",
        takes_value: true,
        set: set_incentives,
    },
    Flag {
        name: "--bets",
        takes_value: false,
        set: set_bets,
    },
    Flag {
        name: "--compaction",
        takes_value: true,
        set: set_compaction,
    },
    Flag {
        name: "--thinking-keep",
        takes_value: true,
        set: set_thinking_keep,
    },
    Flag {
        name: "--collapse-hysteresis",
        takes_value: true,
        set: set_collapse_hysteresis,
    },
    Flag {
        name: "--allow-dirty-workdir",
        takes_value: false,
        set: set_allow_dirty_workdir,
    },
    Flag {
        name: "--pass-env",
        takes_value: true,
        set: set_pass_env,
    },
];

pub(crate) fn parse_args(argv: &[String]) -> Result<Args, String> {
    let b = parse_table(
        argv,
        "run",
        USAGE,
        |k| format!("unknown flag {k:?}\n{USAGE}"),
        RUN_FLAGS,
        RunBuilder::default(),
    )?;
    Ok(Args {
        goal: b.goal.ok_or("missing --goal")?,
        context_files: b.context_files,
        max_tokens: b.max_tokens,
        workdir: PathBuf::from(b.workdir.ok_or("missing --workdir")?),
        model: b.model.ok_or("missing --model")?,
        endpoint: b.endpoint,
        api_key_env: b.api_key_env.unwrap_or_else(|| "OPENAI_API_KEY".into()),
        headers: b.headers,
        allow_cmd: b.allow_cmd,
        budget_steps: b.budget_steps,
        budget_actions: b.budget_actions,
        budget_tokens: b.budget_tokens,
        dump_events: b.dump_events,
        log_path: b.log_path,
        no_log: b.no_log,
        keep_thinking: b.keep_thinking,
        proof_cmd: b.proof_cmd,
        incentives: b.incentives.unwrap_or(agent_loop::IncentivesLevel::Full),
        bets: b.bets,
        compaction: b.compaction,
        thinking_keep: b.thinking_keep,
        collapse_hysteresis: b.collapse_hysteresis,
        allow_dirty_workdir: b.allow_dirty_workdir,
        pass_env: b.pass_env,
        exec_wrap: None,
    })
}

/// Edge env resolution for the fold knobs: `THINKING_KEEP` /
/// `COLLAPSE_HYSTERESIS` fill the knob only when the matching flag is
/// absent (flag wins); unset leaves `None` and [`run_config`] falls back to
/// the loop default. A present-but-unparsable value is a usage error, never
/// a silent default. Runs once in `main` before anything reads the config.
pub(crate) fn apply_env_defaults(args: &mut Args) -> Result<(), String> {
    if args.thinking_keep.is_none() {
        args.thinking_keep = read_env_usize("THINKING_KEEP")?;
    }
    if args.collapse_hysteresis.is_none() {
        args.collapse_hysteresis = read_env_usize("COLLAPSE_HYSTERESIS")?;
    }
    Ok(())
}

fn read_env_usize(name: &str) -> Result<Option<usize>, String> {
    match std::env::var(name) {
        Err(_) => Ok(None),
        Ok(raw) if raw.trim().is_empty() => Ok(None),
        Ok(raw) => raw
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| format!("{name} needs a positive integer, got {raw:?}")),
    }
}

/// Budget for one run: explicit flags win; context-pinned runs without a
/// token flag get the pilot default; otherwise the library UnattendedBatch
/// config (50k tokens). The step override is `agent_budget::with_steps`
/// (warn clamp + refund re-derive live there, never here); precedence
/// (cfg-over-state) is documented on `agent_loop::run`.
pub(crate) fn budget_for(args: &Args) -> BudgetGuard {
    let mut b = config_for(Capability::UnattendedBatch);
    if args.budget_steps.is_some() || args.budget_actions.is_some() || args.budget_tokens.is_some()
    {
        // Explicit recipe flags own the whole budget. The preset wall
        // (900s) is the measured guard for the 20-step default batch, not
        // part of a flagged recipe: v0 sweep cells (207-816s) all died on
        // the token cap, so tokens/steps/actions are the operative budget
        // — and one 60-step cell wall-halted at 24% of its 300k tokens
        // (300s exec timeouts burn the 900s wall in three builds). The
        // LongTask 3600s allowance stays as the runaway guard: it cannot
        // bind at the measured slowest pace (55s/step x 60 = 3300s).
        b.max_wallclock = Duration::from_secs(3600);
    }
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

/// Request profile for one run: `keep_thinking` skips the DeepSeek-specific
/// thinking-off body (for providers that reject the fragment, or when
/// reasoning is wanted); the default profile stays byte-identical. Shared
/// by `rof run` (main) and the eval agent leg so the knob has one impl.
pub(crate) fn endpoint_profile(keep_thinking: bool) -> EndpointProfile {
    if keep_thinking {
        EndpointProfile {
            emission_threshold_chars: 12_000,
            thinking_off: None,
        }
    } else {
        EndpointProfile::default()
    }
}

/// One run's loop config from the parsed flags. The compaction checkpoint is
/// opt-in: absent `--compaction` leaves the default disabled config, so the
/// run stays byte-identical. The WAL is the opposite: default on inside
/// `--workdir` (fail-closed on real runs, not only tests). The fold knobs
/// (`thinking_keep`, `collapse_hysteresis`) are flag-or-env resolved at the
/// edge by [`apply_env_defaults`]; unset falls back to the loop default here,
/// so the loop itself never reads the environment.
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
    let dflt = RunConfig::default();
    cfg.thinking_keep = args.thinking_keep.unwrap_or(dflt.thinking_keep);
    cfg.collapse_hysteresis = args.collapse_hysteresis.unwrap_or(dflt.collapse_hysteresis);
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
    use crate::fixtures::{args_for, argv};
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
                keep_thinking: false,
                bets: false,
                incentives: agent_loop::IncentivesLevel::Full,
                proof_cmd: None,
                compaction: Some(0.6),
                thinking_keep: None,
                collapse_hysteresis: None,
                allow_dirty_workdir: false,
                pass_env: Vec::new(),
                exec_wrap: None,
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
        assert_eq!(
            (b.thinking_keep, b.collapse_hysteresis),
            (None, None),
            "absent fold flags leave the loop defaults"
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

    /// Process env is process-wide: the fold-knob env tests mutate it, so
    /// they serialize on this lock. No other test reads these two vars.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn run_min(extra: &[&str]) -> Vec<String> {
        let mut words = vec![
            "rof",
            "run",
            "--goal",
            "g",
            "--workdir",
            "/w",
            "--model",
            "m",
        ];
        words.extend(extra);
        argv(&words)
    }

    #[test]
    fn fold_knobs_parse_both_forms() {
        let a = parse_args(&run_min(&[
            "--thinking-keep",
            "5",
            "--collapse-hysteresis=3",
        ]))
        .unwrap();
        assert_eq!((a.thinking_keep, a.collapse_hysteresis), (Some(5), Some(3)));
    }

    #[test]
    fn fold_knobs_reject_non_numeric() {
        for extra in [
            vec!["--thinking-keep", "many"],
            vec!["--thinking-keep"],
            vec!["--collapse-hysteresis", "many"],
            vec!["--collapse-hysteresis"],
            vec!["--collapse-hysteresis", "-1"],
        ] {
            assert!(parse_args(&run_min(&extra)).is_err(), "{extra:?}");
        }
    }

    #[test]
    fn fold_knobs_env_fills_flag_wins_malformed_errors() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("THINKING_KEEP", "7");
        std::env::set_var("COLLAPSE_HYSTERESIS", "4");
        // Env fills absent flags.
        let mut a = parse_args(&run_min(&[])).unwrap();
        apply_env_defaults(&mut a).unwrap();
        assert_eq!((a.thinking_keep, a.collapse_hysteresis), (Some(7), Some(4)));
        // Flag wins over env.
        let mut b = parse_args(&run_min(&["--thinking-keep", "1"])).unwrap();
        apply_env_defaults(&mut b).unwrap();
        assert_eq!((b.thinking_keep, b.collapse_hysteresis), (Some(1), Some(4)));
        // Present-but-unparsable is a usage error, never a silent default.
        std::env::set_var("THINKING_KEEP", "many");
        let mut c = parse_args(&run_min(&[])).unwrap();
        assert!(apply_env_defaults(&mut c).is_err());
        std::env::remove_var("THINKING_KEEP");
        std::env::remove_var("COLLAPSE_HYSTERESIS");
    }

    #[test]
    fn fold_knobs_reach_run_config_with_loop_defaults() {
        use crate::fixtures::args_for;
        let dir = Path::new("/tmp/w");
        let dflt = RunConfig::default();
        let cfg = run_config(&args_for(dir, None));
        assert_eq!(cfg.thinking_keep, dflt.thinking_keep);
        assert_eq!(cfg.collapse_hysteresis, dflt.collapse_hysteresis);
        let mut a = args_for(dir, None);
        a.thinking_keep = Some(9);
        a.collapse_hysteresis = Some(6);
        let cfg = run_config(&a);
        assert_eq!((cfg.thinking_keep, cfg.collapse_hysteresis), (9, 6));
    }

    #[test]
    fn allow_dirty_workdir_parses_opt_in() {
        let a = parse_args(&run_min(&["--allow-dirty-workdir"])).unwrap();
        assert!(a.allow_dirty_workdir);
        let b = parse_args(&run_min(&[])).unwrap();
        assert!(!b.allow_dirty_workdir, "dirty repos refused by default");
    }

    #[test]
    fn pass_env_parses_repeatable_and_rejects_bare() {
        let a = parse_args(&run_min(&["--pass-env", "FOO", "--pass-env=BAR"])).unwrap();
        assert_eq!(a.pass_env, vec!["FOO".to_string(), "BAR".to_string()]);
        assert!(parse_args(&run_min(&["--pass-env"])).is_err());
        assert_eq!(
            parse_args(&run_min(&[])).unwrap().pass_env,
            Vec::<String>::new(),
            "absent flag forwards nothing extra"
        );
    }

    #[test]
    fn budget_flags_lift_the_preset_wall() {
        // No flags: the measured UnattendedBatch wall stays (20-step
        // default-batch guard).
        assert_eq!(
            budget_for(&args_for(Path::new("/tmp/w"), None))
                .config()
                .max_wallclock,
            Duration::from_secs(900)
        );
        // Any explicit recipe flag owns the whole budget: the wall lifts
        // to the LongTask allowance so it can never truncate a flagged
        // recipe before its own caps bind (measured: a 60-step/300k cell
        // wall-halted at 24% of its token budget under the 900s preset).
        for (steps, actions, tokens) in [
            (Some(60u32), None, None),
            (None, Some(120u32), None),
            (None, None, Some(300_000u64)),
            (Some(60), Some(120), Some(300_000)),
        ] {
            let mut a = args_for(Path::new("/tmp/w"), steps);
            a.budget_actions = actions;
            a.budget_tokens = tokens;
            assert_eq!(
                budget_for(&a).config().max_wallclock,
                Duration::from_secs(3600),
                "flags: steps={steps:?} actions={actions:?} tokens={tokens:?}"
            );
        }
    }

    #[test]
    fn keep_thinking_parses_and_selects_the_profile() {
        let a = parse_args(&run_min(&["--keep-thinking"])).unwrap();
        assert!(a.keep_thinking);
        let plain = parse_args(&run_min(&[])).unwrap();
        assert!(!plain.keep_thinking, "default sends the thinking-off body");
        let on = endpoint_profile(true);
        assert!(on.thinking_off.is_none(), "keep-thinking sends no knob");
        let off = endpoint_profile(false);
        assert_eq!(off.emission_threshold_chars, on.emission_threshold_chars);
        assert!(
            off.thinking_off.is_some(),
            "default keeps the DeepSeek thinking-off body"
        );
    }
}
