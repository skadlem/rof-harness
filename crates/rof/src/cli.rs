//! `rof run` flags: table-driven hand-rolled parsing plus the pure mappers
//! from parsed flags to run knobs (budget, `RunConfig`, endpoint).

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use agent_budget::{config_for, with_steps, BudgetGuard, Capability};
use agent_loop::RunConfig;
use provider_openai::EndpointProfile;

use crate::workdir::resolve_log_path;

pub(crate) const USAGE: &str = "usage: rof run --goal TEXT --workdir DIR (--workdir must be a disposable dir: never /, $HOME, or the harness checkout) --model ID [--endpoint URL] [--api-key-env NAME] [--header NAME:VALUE] [--allow-cmd CMD...] [--budget-steps N] [--budget-actions N] [--budget-tokens N] [--budget-shape gross|miss-plus-out] [--exec-policy allowlist|any] [--max-tokens N] [--reasoning-effort LEVEL] [--steer on|off] [--streak-steer on|off] [--deliverable PATH]... [--deliverable-steer on|off] [--context-file PATH]... [--proof-cmd CMD] [--completion-check CMD] [--repo-lookup on|off] [--repo-evidence on|off] [--incentives LEVEL] [--bets] [--compaction FRAC --context-capacity N --output-reserve N] [--checkpoint-sources on|off] [--history-read on|off] [--dump-events PATH] [--log-path PATH] [--no-log] [--keep-thinking] [--thinking-keep N] [--collapse-hysteresis N] [--allow-dirty-workdir] [--allow-inert-spend-cap] [--pass-env NAME]...
--dump-events PATH: JSONL, one event per line (LF); replaces any previous dump at PATH
--log-path PATH: fail-closed WAL (default <workdir>/.rof-events.jsonl)
--no-log: disable the WAL (run without a durable log)
--keep-thinking: skip the thinking-off request knob for providers that reject it or when reasoning is wanted (default keeps the DeepSeek thinking-off knob, sent only on compaction-summary calls and the truncation ladder first rung; ordinary requests carry no thinking fragment)
--streak-steer on|off: enable the experimental six-read directive (default off)
--deliverable PATH: repeatable output file the task names, relative to the workdir (no absolute path, no `..`); if still missing at step 15 the crossing is logged (shadow, no behavior change)
--deliverable-steer on|off: additionally deliver one 'create it now' directive for missing --deliverable paths (experimental, unvalidated; default off)
--steer on|off: initiative steering nudges (unverified-declare hold, half/late action directives, wrap-up nudge). The workflow contract and the rollback notice ship either way. Default off: measured dead in dphn-steer2 (no named-test change on either cell, 2.64x tokens per solved task)
--reasoning-effort LEVEL (none|low|high|xhigh|max): send `reasoning_effort` on every request (default: none sent = endpoint default). DeepSeek docs (2026-11-09) list low/high/max and default high; xhigh maps to high. The 2026-10-19 one-prompt api.deepseek.com probe did not detect an effect; that result is historical, not universal. The vLLM gateway probe measured 0/722/1060/1149/2976 reasoning tokens for none/low/high/xhigh/max
--context-file PATH: pinned context; with neither --budget-tokens nor --budget-steps the run defaults to a 200000-token budget
--exec-policy allowlist|any: command-allowlist policy for exec/test. `any` skips the list entirely and is for callers whose process boundary is elsewhere (in-container eval wraps exec as `docker exec <container>`, so the allowlist is not a boundary there, only capability loss); the choice is recorded in the report provenance so relaxed rows never pool with strict rows
--compaction FRAC: opt-in checkpoint above FRAC of usable context capacity (0 < FRAC <= 1); requires explicit --context-capacity and --output-reserve >= max(initial output,32768), below capacity. Cumulative run token allowance is separate. Default off, unvalidated.
--checkpoint-sources on|off: opt-in fact/hypothesis/plan checkpoint labels with original source IDs; requires --compaction
--history-read on|off: opt-in read-only retrieval from current primary WAL, with IDs in collapsed observations; incompatible with --no-log or --review on (default off)
--thinking-keep N: echoed-reasoning rows kept per assistant message (flag wins over THINKING_KEEP; unset leaves the loop default)
--collapse-hysteresis N: collapse-boundary hysteresis rows (flag wins over COLLAPSE_HYSTERESIS; unset leaves the loop default)
--allow-dirty-workdir: run inside a dirty git workdir (default: refuse; allowed dirt appears in the reported patch)
--allow-inert-spend-cap: run when the $ cap cannot fire because the price book does not price --model (default: refuse; the inert cap is warned once)
--pass-env NAME: forward env var to exec/test children (repeatable; provider-key-shaped names stay withheld at spawn)
--repo-lookup on|off: opt-in literal repository lookup (default off)
--repo-evidence on|off: enriched bounded definition/test/config candidates; requires --repo-lookup on (default off)
--completion-check CMD: exact public test command checked on Done, at most one repair and two checks within the original caps; argv, no shell; cannot combine with --review on (default unset)
--review on|off: opt-in bounded fresh-context read-only defect review and one repair continuation; default off
--procedure-file PATH: add a validated JSON procedure record (repeatable); only case-insensitive literal cue matches are injected, with bounded text and explicit source repositories
exit codes: 0 done, 2 usage/parse, 3 run failure (halt/cancel/provider), 4 missing credentials, 5 log failure, 6 snapshot failure, 7 input/config failure, 8 unmet completion predicate";

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
    /// Command-allowlist policy for exec/test (`Allowlist` default). `Any`
    /// skips the list: the caller owns a real process boundary (in-container
    /// eval wraps exec as `docker exec <container>`).
    pub exec_policy: tools_std::ExecPolicy,
    pub budget_steps: Option<u32>,
    pub budget_actions: Option<u32>,
    pub budget_tokens: Option<u64>,
    /// Token-cap accounting shape (`None` = the loop default `Gross`);
    /// `MissPlusOut` is a different estimand — never mixed with Gross rows.
    pub budget_shape: Option<agent_loop::BudgetShape>,
    pub max_tokens: Option<usize>,
    pub context_files: Vec<String>,
    pub dump_events: Option<String>,
    /// WAL override (`None` = default inside `--workdir`); `--no-log` wins.
    pub log_path: Option<String>,
    /// Opt out of the fail-closed WAL.
    pub no_log: bool,
    /// Run with a `$` cap that cannot fire. The default preset arms the $4 cap
    /// and an unpriced model reports no cost, so `budget_for` refuses such a
    /// run as a configuration error. Set this only for an endpoint the price
    /// book deliberately does not model (the vLLM gateway's
    /// `deepseek-ai/DeepSeek-V4.1-Flash`): choice is recorded, and the inert
    /// cap is warned about once. Default false.
    pub allow_inert_spend_cap: bool,
    /// Send no thinking-off knob in the request body (`EndpointProfile`
    /// with `thinking_off: None`): for providers that reject the
    /// DeepSeek-specific fragment or when reasoning is wanted. Default
    /// false keeps the current body byte-identical.
    pub keep_thinking: bool,
    /// `reasoning_effort` level sent on every request (`None` = endpoint
    /// default). Validated at the edge against [`EFFORT_LEVELS`].
    pub reasoning_effort: Option<String>,
    /// Initiative steering nudges (default `false`: measured dead in
    /// `dphn-steer2`, docs/decisions.md 2026-11-06 — no named-test change on
    /// either cell at 2.64x tokens per solved task). `--steer on` re-enables
    /// the unverified-declare hold, the half/late action directives and the
    /// wrap-up nudge; the workflow contract and the rollback notice ship
    /// either way.
    pub steer: bool,
    /// Experimental read-streak intervention, off by default.
    pub streak_steer: bool,
    /// Named output files checked at step 15 (shadow log).
    pub deliverables: Vec<PathBuf>,
    /// Opt-in directive for missing deliverables, off by default.
    pub deliverable_steer: bool,
    pub proof_cmd: Option<String>,
    pub completion_check: Option<String>,
    pub incentives: agent_loop::IncentivesLevel,
    pub bets: bool,
    /// Compaction trigger fraction; `None` = checkpoint off (default).
    pub compaction: Option<f64>,
    pub context_capacity: Option<u64>,
    pub output_reserve: Option<u64>,
    pub checkpoint_sources: bool,
    pub history_read: bool,
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
    pub preserve_failed_checks: bool,
    pub minimal_workflow: bool,
    pub observation_keep: Option<usize>,
    pub stall_threshold: Option<usize>,
    pub archive_output: bool,
    pub shell: bool,
    pub processes: bool,
    pub repo_lookup: bool,
    pub repo_evidence: bool,
    pub multi_edit: bool,
    pub review: bool,
    pub procedure_files: Vec<PathBuf>,
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
    exec_policy: Option<tools_std::ExecPolicy>,
    budget_steps: Option<u32>,
    budget_actions: Option<u32>,
    budget_tokens: Option<u64>,
    budget_shape: Option<agent_loop::BudgetShape>,
    max_tokens: Option<usize>,
    context_files: Vec<String>,
    dump_events: Option<String>,
    log_path: Option<String>,
    no_log: bool,
    keep_thinking: bool,
    reasoning_effort: Option<String>,
    steer: Option<bool>,
    streak_steer: Option<bool>,
    deliverables: Vec<PathBuf>,
    deliverable_steer: Option<bool>,
    proof_cmd: Option<String>,
    completion_check: Option<String>,
    incentives: Option<agent_loop::IncentivesLevel>,
    bets: bool,
    compaction: Option<f64>,
    context_capacity: Option<u64>,
    output_reserve: Option<u64>,
    checkpoint_sources: Option<bool>,
    history_read: Option<bool>,
    thinking_keep: Option<usize>,
    collapse_hysteresis: Option<usize>,
    allow_dirty_workdir: bool,
    allow_inert_spend_cap: bool,
    pass_env: Vec<String>,
    preserve_failed_checks: bool,
    minimal_workflow: Option<bool>,
    observation_keep: Option<usize>,
    stall_threshold: Option<usize>,
    archive_output: Option<bool>,
    shell: Option<bool>,
    processes: Option<bool>,
    repo_lookup: Option<bool>,
    repo_evidence: Option<bool>,
    multi_edit: Option<bool>,
    review: bool,
    procedure_files: Vec<PathBuf>,
}

pub(crate) fn parse_toggle(flag: &str, v: Option<String>) -> Result<bool, String> {
    match v.as_deref() {
        Some("on") => Ok(true),
        Some("off") => Ok(false),
        other => Err(format!("{flag} expects on|off, got {other:?}")),
    }
}

fn set_preserve_failed_checks(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.preserve_failed_checks = parse_toggle("--preserve-failed-checks", v)?;
    Ok(())
}

fn set_workflow(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.minimal_workflow = Some(match v.as_deref() {
        Some("legacy") => false,
        Some("minimal") => true,
        other => return Err(format!("--workflow expects legacy|minimal, got {other:?}")),
    });
    Ok(())
}

pub(crate) fn set_positive_usize(flag: &str, v: Option<String>) -> Result<usize, String> {
    let s = v.unwrap_or_default();
    let n: usize = s
        .parse()
        .map_err(|_| format!("{flag} needs a positive integer, got {s:?}"))?;
    if n == 0 {
        return Err(format!("{flag} must be > 0"));
    }
    Ok(n)
}

fn set_observation_keep(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.observation_keep = Some(set_positive_usize("--observation-keep", v)?);
    Ok(())
}

fn set_stall_threshold(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    let s = v.unwrap_or_default();
    b.stall_threshold = Some(
        s.parse()
            .map_err(|_| format!("--stall-threshold needs a non-negative integer, got {s:?}"))?,
    );
    Ok(())
}

fn set_archive_output(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.archive_output = Some(parse_toggle("--archive-output", v)?);
    Ok(())
}

fn set_shell(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.shell = Some(parse_toggle("--shell", v)?);
    Ok(())
}

fn set_processes(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.processes = Some(parse_toggle("--processes", v)?);
    Ok(())
}

fn set_repo_lookup(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.repo_lookup = Some(parse_toggle("--repo-lookup", v)?);
    Ok(())
}

fn set_repo_evidence(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.repo_evidence = Some(parse_toggle("--repo-evidence", v)?);
    Ok(())
}

fn set_completion_check(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    let command = v.ok_or("--completion-check needs a command")?;
    if command.trim().is_empty() {
        return Err("--completion-check needs a nonempty command".into());
    }
    b.completion_check = Some(command);
    Ok(())
}

fn set_multi_edit(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.multi_edit = Some(parse_toggle("--multi-edit", v)?);
    Ok(())
}

fn set_review(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.review = parse_toggle("--review", v)?;
    Ok(())
}

fn set_procedure_file(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.procedure_files
        .push(PathBuf::from(v.ok_or("--procedure-file needs a path")?));
    Ok(())
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

/// Levels the gateway serves (its `thinkingLevelMap`, verified live
/// 2026-10-19). Validated here so a typo fails loudly instead of sending
/// junk the endpoint ignores.
pub(crate) const EFFORT_LEVELS: [&str; 5] = ["none", "low", "high", "xhigh", "max"];

fn set_steer(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.steer = Some(match v.as_deref() {
        Some("on") => true,
        Some("off") => false,
        other => return Err(format!("--steer expects on|off, got {other:?}")),
    });
    Ok(())
}

fn set_streak_steer(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.streak_steer = Some(match v.as_deref() {
        Some("on") => true,
        Some("off") => false,
        other => return Err(format!("--streak-steer expects on|off, got {other:?}")),
    });
    Ok(())
}

/// A deliverable is a path inside the workdir: relative, no `..`.
pub(crate) fn parse_deliverable(v: Option<String>) -> Result<PathBuf, String> {
    let p = PathBuf::from(v.ok_or("--deliverable needs a path")?);
    if p.as_os_str().is_empty()
        || p.is_absolute()
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!(
            "--deliverable expects a relative path without `..`, got {p:?}"
        ));
    }
    Ok(p)
}

pub(crate) fn parse_on_off(flag: &str, v: Option<String>) -> Result<bool, String> {
    match v.as_deref() {
        Some("on") => Ok(true),
        Some("off") => Ok(false),
        other => Err(format!("{flag} expects on|off, got {other:?}")),
    }
}

fn set_deliverable(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.deliverables.push(parse_deliverable(v)?);
    Ok(())
}

fn set_deliverable_steer(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.deliverable_steer = Some(parse_on_off("--deliverable-steer", v)?);
    Ok(())
}

fn set_reasoning_effort(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    let level = v.ok_or("--reasoning-effort needs a level")?;
    if !EFFORT_LEVELS.contains(&level.as_str()) {
        return Err(format!(
            "--reasoning-effort expects {} , got {level:?}",
            EFFORT_LEVELS.join("|")
        ));
    }
    b.reasoning_effort = Some(level);
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

fn set_exec_policy(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    let s = v.unwrap_or_default();
    b.exec_policy = Some(s.parse()?);
    Ok(())
}

fn set_budget_shape(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    let s = v.unwrap_or_default();
    b.budget_shape = Some(
        s.parse()
            .map_err(|_| format!("--budget-shape needs gross or miss-plus-out, got {s:?}"))?,
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

fn set_context_capacity(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    let n = v
        .unwrap_or_default()
        .parse::<u64>()
        .map_err(|_| "--context-capacity needs a positive integer")?;
    if n == 0 {
        return Err("--context-capacity must be > 0".into());
    }
    b.context_capacity = Some(n);
    Ok(())
}
fn set_output_reserve(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.output_reserve = Some(
        v.unwrap_or_default()
            .parse::<u64>()
            .map_err(|_| "--output-reserve needs a nonnegative integer")?,
    );
    Ok(())
}
fn set_checkpoint_sources(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.checkpoint_sources = Some(parse_toggle("--checkpoint-sources", v)?);
    Ok(())
}
fn set_history_read(b: &mut RunBuilder, v: Option<String>) -> Result<(), String> {
    b.history_read = Some(parse_toggle("--history-read", v)?);
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

fn set_allow_inert_spend_cap(b: &mut RunBuilder, _: Option<String>) -> Result<(), String> {
    b.allow_inert_spend_cap = true;
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
        name: "--context-capacity",
        takes_value: true,
        set: set_context_capacity,
    },
    Flag {
        name: "--output-reserve",
        takes_value: true,
        set: set_output_reserve,
    },
    Flag {
        name: "--checkpoint-sources",
        takes_value: true,
        set: set_checkpoint_sources,
    },
    Flag {
        name: "--history-read",
        takes_value: true,
        set: set_history_read,
    },
    Flag {
        name: "--preserve-failed-checks",
        takes_value: true,
        set: set_preserve_failed_checks,
    },
    Flag {
        name: "--workflow",
        takes_value: true,
        set: set_workflow,
    },
    Flag {
        name: "--observation-keep",
        takes_value: true,
        set: set_observation_keep,
    },
    Flag {
        name: "--stall-threshold",
        takes_value: true,
        set: set_stall_threshold,
    },
    Flag {
        name: "--archive-output",
        takes_value: true,
        set: set_archive_output,
    },
    Flag {
        name: "--shell",
        takes_value: true,
        set: set_shell,
    },
    Flag {
        name: "--processes",
        takes_value: true,
        set: set_processes,
    },
    Flag {
        name: "--repo-lookup",
        takes_value: true,
        set: set_repo_lookup,
    },
    Flag {
        name: "--repo-evidence",
        takes_value: true,
        set: set_repo_evidence,
    },
    Flag {
        name: "--completion-check",
        takes_value: true,
        set: set_completion_check,
    },
    Flag {
        name: "--multi-edit",
        takes_value: true,
        set: set_multi_edit,
    },
    Flag {
        name: "--review",
        takes_value: true,
        set: set_review,
    },
    Flag {
        name: "--procedure-file",
        takes_value: true,
        set: set_procedure_file,
    },
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
        name: "--reasoning-effort",
        takes_value: true,
        set: set_reasoning_effort,
    },
    Flag {
        name: "--steer",
        takes_value: true,
        set: set_steer,
    },
    Flag {
        name: "--streak-steer",
        takes_value: true,
        set: set_streak_steer,
    },
    Flag {
        name: "--deliverable",
        takes_value: true,
        set: set_deliverable,
    },
    Flag {
        name: "--deliverable-steer",
        takes_value: true,
        set: set_deliverable_steer,
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
        name: "--exec-policy",
        takes_value: true,
        set: set_exec_policy,
    },
    Flag {
        name: "--budget-shape",
        takes_value: true,
        set: set_budget_shape,
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
        name: "--allow-inert-spend-cap",
        takes_value: false,
        set: set_allow_inert_spend_cap,
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
    let exec_policy = b.exec_policy.unwrap_or(tools_std::ExecPolicy::Allowlist);
    if (b.shell.unwrap_or(false) || b.processes.unwrap_or(false))
        && exec_policy != tools_std::ExecPolicy::Any
    {
        return Err("--shell and --processes require --exec-policy any".into());
    }
    if b.repo_evidence.unwrap_or(false) && !b.repo_lookup.unwrap_or(false) {
        return Err("--repo-evidence requires --repo-lookup on".into());
    }
    if b.completion_check.is_some() && b.review {
        return Err("--completion-check cannot be combined with --review on".into());
    }
    if b.history_read.unwrap_or(false) && b.no_log {
        return Err("--history-read on requires the current run WAL; remove --no-log".into());
    }
    if b.history_read.unwrap_or(false) && b.review {
        return Err("--history-read cannot be combined with --review on; history is bound to the primary run".into());
    }
    if b.checkpoint_sources.unwrap_or(false) && b.compaction.is_none() {
        return Err("--checkpoint-sources requires --compaction".into());
    }
    if b.compaction.is_some() {
        let capacity = b
            .context_capacity
            .ok_or("--compaction requires --context-capacity")?;
        let reserve = b
            .output_reserve
            .ok_or("--compaction requires --output-reserve")?;
        let maximum_output = b.max_tokens.unwrap_or(8000).max(32768) as u64;
        if reserve >= capacity || reserve < maximum_output {
            return Err(format!(
                "--output-reserve must be >= {maximum_output} and below --context-capacity"
            ));
        }
    }
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
        exec_policy,
        budget_steps: b.budget_steps,
        budget_actions: b.budget_actions,
        budget_tokens: b.budget_tokens,
        budget_shape: b.budget_shape,
        dump_events: b.dump_events,
        log_path: b.log_path,
        no_log: b.no_log,
        allow_inert_spend_cap: b.allow_inert_spend_cap,
        keep_thinking: b.keep_thinking,
        reasoning_effort: b.reasoning_effort,
        steer: b.steer.unwrap_or(false),
        streak_steer: b.streak_steer.unwrap_or(false),
        deliverables: b.deliverables,
        deliverable_steer: b.deliverable_steer.unwrap_or(false),
        proof_cmd: b.proof_cmd,
        completion_check: b.completion_check,
        incentives: b.incentives.unwrap_or(agent_loop::IncentivesLevel::Full),
        bets: b.bets,
        compaction: b.compaction,
        context_capacity: b.context_capacity,
        output_reserve: b.output_reserve,
        checkpoint_sources: b.checkpoint_sources.unwrap_or(false),
        history_read: b.history_read.unwrap_or(false),
        thinking_keep: b.thinking_keep,
        collapse_hysteresis: b.collapse_hysteresis,
        allow_dirty_workdir: b.allow_dirty_workdir,
        pass_env: b.pass_env,
        preserve_failed_checks: b.preserve_failed_checks,
        minimal_workflow: b.minimal_workflow.unwrap_or(false),
        observation_keep: b.observation_keep,
        stall_threshold: b.stall_threshold,
        archive_output: b.archive_output.unwrap_or(false),
        shell: b.shell.unwrap_or(false),
        processes: b.processes.unwrap_or(false),
        repo_lookup: b.repo_lookup.unwrap_or(false),
        repo_evidence: b.repo_evidence.unwrap_or(false),
        multi_edit: b.multi_edit.unwrap_or(false),
        review: b.review,
        procedure_files: b.procedure_files,
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
pub(crate) fn budget_for(args: &Args) -> Result<BudgetGuard, String> {
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
    let guard = BudgetGuard::new(b, Instant::now());
    if args.allow_inert_spend_cap {
        if let Some(msg) = inert_cap_accepted_warning(args, &guard) {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| eprintln!("{msg}"));
        }
    }
    match spend_guard_error(args, &guard) {
        Some(message) => Err(message),
        None => Ok(guard),
    }
}

/// The same fact as [`spend_guard_error`], reported rather than refused: the
/// operator opted in with `--allow-inert-spend-cap`, so the unenforceable cap
/// is named once instead of silently shrinking the guard surface to
/// tokens/steps.
fn inert_cap_accepted_warning(args: &Args, guard: &BudgetGuard) -> Option<String> {
    let capped = guard.config().max_spend_cents?;
    (!provider_openai::prices_model(&args.model)).then(|| {
        format!(
            "warning: no price-book entry for model {}: the ${} spend cap is inert \
             (dollars stay null) and only the token/step caps guard this run",
            args.model,
            capped / 100
        )
    })
}

/// The `$` cap only binds when the price book prices the model: an id with no
/// rate anchor (never invented) reports no cost, so `max_spend_cents` can
/// never trip. A guard that cannot fire is a configuration error, not a
/// warning: refusing here is the only place that cannot be bypassed, because
/// `budget_for` is the one resolver every run and eval instance goes through.
fn spend_guard_error(args: &Args, guard: &BudgetGuard) -> Option<String> {
    let capped = guard.config().max_spend_cents?;
    if args.allow_inert_spend_cap {
        return None;
    }
    (!provider_openai::prices_model(&args.model)).then(|| {
        format!(
            "model {} has no price-book entry, so the ${} spend cap cannot be \
             enforced (dollars would stay null) — refusing to start. Use a priced \
             model id, or pass --allow-inert-spend-cap to accept an unenforced \
             dollar cap for an endpoint the price book does not model",
            args.model,
            capped / 100
        )
    })
}

/// Request profile for one run: `keep_thinking` skips the DeepSeek-specific
/// thinking-off body (for providers that reject the fragment, or when
/// reasoning is wanted); the default profile stays byte-identical. Shared
/// by `rof run` (main) and the eval agent leg so the knob has one impl.
pub(crate) fn endpoint_profile(
    keep_thinking: bool,
    reasoning_effort: Option<&str>,
) -> EndpointProfile {
    let mut p = if keep_thinking {
        EndpointProfile {
            emission_threshold_chars: 12_000,
            thinking_off: None,
            reasoning_effort: None,
        }
    } else {
        EndpointProfile::default()
    };
    p.reasoning_effort = reasoning_effort.map(str::to_owned);
    p
}

/// One run's loop config from the parsed flags. The compaction checkpoint is
/// opt-in: absent `--compaction` leaves the default disabled config, so the
/// run stays byte-identical. The WAL is the opposite: default on inside
/// `--workdir` (fail-closed on real runs, not only tests). The fold knobs
/// (`thinking_keep`, `collapse_hysteresis`) are flag-or-env resolved at the
/// edge by [`apply_env_defaults`]; unset falls back to the loop default here,
/// so the loop itself never reads the environment.
pub(crate) fn run_config(args: &Args) -> RunConfig {
    let dflt = RunConfig::default();
    let mut cfg = RunConfig {
        goal: args.goal.clone(),
        model: args.model.clone(),
        context_files: args.context_files.clone(),
        max_tokens: args.max_tokens.unwrap_or(8_000),
        incentives: args.incentives,
        proof_cmd: args.proof_cmd.clone(),
        completion_check: args.completion_check.clone(),
        history_read: args.history_read,
        steer: args.steer,
        streak_steer: args.streak_steer,
        deliverables: args.deliverables.clone(),
        deliverable_steer: args.deliverable_steer,
        preserve_failed_checks: args.preserve_failed_checks,
        minimal_workflow: args.minimal_workflow,
        observation_keep: args.observation_keep.unwrap_or(dflt.observation_keep),
        stall_threshold: args.stall_threshold.unwrap_or(dflt.stall_threshold),
        ..RunConfig::default()
    };
    cfg.thinking_keep = args.thinking_keep.unwrap_or(dflt.thinking_keep);
    cfg.collapse_hysteresis = args.collapse_hysteresis.unwrap_or(dflt.collapse_hysteresis);
    cfg.budget_shape = args.budget_shape.unwrap_or(dflt.budget_shape);
    cfg.log_path = resolve_log_path(args);
    cfg.compaction.context_capacity_tokens = args.context_capacity;
    cfg.compaction.output_reserve_tokens = args.output_reserve.unwrap_or(0);
    cfg.compaction.checkpoint_sources = args.checkpoint_sources;
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
            "--budget-shape",
            "miss-plus-out",
            "--context-file",
            "app/a.py",
            "--dump-events",
            "/tmp/ev.json",
            "--log-path",
            "/tmp/w.log",
            "--compaction",
            "0.6",
            "--context-capacity",
            "131072",
            "--output-reserve",
            "32768",
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
                exec_policy: tools_std::ExecPolicy::Allowlist,
                budget_steps: Some(7),
                budget_actions: Some(9),
                budget_tokens: None,
                budget_shape: Some(agent_loop::BudgetShape::MissPlusOut),
                context_files: vec!["app/a.py".into()],
                max_tokens: None,
                dump_events: Some("/tmp/ev.json".into()),
                log_path: Some("/tmp/w.log".into()),
                no_log: false,
                allow_inert_spend_cap: false,
                keep_thinking: false,
                reasoning_effort: None,
                steer: false,
                streak_steer: false,
                deliverables: Vec::new(),
                deliverable_steer: false,
                bets: false,
                incentives: agent_loop::IncentivesLevel::Full,
                proof_cmd: None,
                completion_check: None,
                compaction: Some(0.6),
                context_capacity: Some(131072),
                output_reserve: Some(32768),
                checkpoint_sources: false,
                history_read: false,
                thinking_keep: None,
                collapse_hysteresis: None,
                allow_dirty_workdir: false,
                pass_env: Vec::new(),
                preserve_failed_checks: false,
                minimal_workflow: false,
                observation_keep: None,
                stall_threshold: None,
                archive_output: false,
                shell: false,
                processes: false,
                repo_lookup: false,
                repo_evidence: false,
                multi_edit: false,
                review: false,
                procedure_files: Vec::new(),
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
        assert_eq!(b.budget_shape, None, "absent shape keeps the loop default");
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
                "--budget-shape",
                "miss+out",
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
    fn exec_policy_flag_defaults_to_allowlist_and_parses_any() {
        let base = [
            "rof",
            "run",
            "--goal",
            "g",
            "--workdir",
            "/w",
            "--model",
            "m",
        ];
        assert_eq!(
            parse_args(&argv(&base)).unwrap().exec_policy,
            tools_std::ExecPolicy::Allowlist,
            "absent flag keeps the strict list"
        );
        let mut words = base.to_vec();
        words.extend(["--exec-policy", "any"]);
        assert_eq!(
            parse_args(&argv(&words)).unwrap().exec_policy,
            tools_std::ExecPolicy::Any
        );
        let mut words = base.to_vec();
        words.extend(["--exec-policy", "allowlist"]);
        assert_eq!(
            parse_args(&argv(&words)).unwrap().exec_policy,
            tools_std::ExecPolicy::Allowlist
        );
        let mut words = base.to_vec();
        words.extend(["--exec-policy", "trust"]);
        assert!(parse_args(&argv(&words)).is_err(), "typos must be loud");
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
        assert_eq!(
            budget_for(&args_for(dir, None))
                .expect("priced model")
                .config()
                .max_tokens,
            50_000
        );
        let mut pinned = args_for(dir, None);
        pinned.context_files = vec!["a.py".into()];
        assert_eq!(
            budget_for(&pinned)
                .expect("priced model")
                .config()
                .max_tokens,
            200_000
        );
        // Explicit flags win over the pilot default.
        pinned.budget_tokens = Some(7);
        assert_eq!(
            budget_for(&pinned)
                .expect("priced model")
                .config()
                .max_tokens,
            7
        );
        // A step budget also suppresses the token default.
        pinned.budget_tokens = None;
        pinned.budget_steps = Some(5);
        assert_eq!(
            budget_for(&pinned)
                .expect("priced model")
                .config()
                .max_tokens,
            50_000
        );
        assert_eq!(
            budget_for(&pinned)
                .expect("priced model")
                .config()
                .max_steps
                .get(),
            5
        );
        // The step override is the single `agent_budget::with_steps`
        // implementation: warn clamps to min(warn, max-1), refunds re-derive.
        assert_eq!(
            budget_for(&pinned)
                .expect("priced model")
                .config()
                .warn_steps
                .get(),
            4
        );
        assert_eq!(
            budget_for(&pinned)
                .expect("priced model")
                .config()
                .max_refunds,
            1
        );
        assert_eq!(
            budget_for(&pinned)
                .expect("priced model")
                .config()
                .warn_steps,
            agent_budget::with_steps(
                agent_budget::config_for(agent_budget::Capability::UnattendedBatch),
                NonZeroU32::new(5).unwrap()
            )
            .warn_steps
        );
        // --budget-actions overrides the trial action cap.
        pinned.budget_actions = Some(9);
        assert_eq!(
            budget_for(&pinned)
                .expect("priced model")
                .config()
                .actions_per_trial,
            9
        );
        assert_eq!(
            budget_for(&args_for(dir, None))
                .expect("priced model")
                .config()
                .actions_per_trial,
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
        let mut a = args_for(dir, None);
        a.budget_shape = Some(agent_loop::BudgetShape::MissPlusOut);
        assert_eq!(
            run_config(&a).budget_shape,
            agent_loop::BudgetShape::MissPlusOut
        );
        assert_eq!(
            run_config(&args_for(dir, None)).budget_shape,
            dflt.budget_shape
        );
    }

    #[test]
    fn allow_inert_spend_cap_parses_opt_in() {
        let a = parse_args(&run_min(&["--allow-inert-spend-cap"])).unwrap();
        assert!(a.allow_inert_spend_cap);
        assert!(!parse_args(&run_min(&[])).unwrap().allow_inert_spend_cap);
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
                .expect("priced model")
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
                budget_for(&a).expect("priced model").config().max_wallclock,
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
        let on = endpoint_profile(true, None);
        assert!(on.thinking_off.is_none(), "keep-thinking sends no knob");
        let off = endpoint_profile(false, None);
        assert_eq!(off.emission_threshold_chars, on.emission_threshold_chars);
        assert!(
            off.thinking_off.is_some(),
            "default keeps the DeepSeek thinking-off body"
        );
    }

    #[test]
    fn reasoning_effort_parses_validates_and_lands_in_the_profile() {
        assert_eq!(endpoint_profile(false, None).reasoning_effort, None);
        for level in ["none", "low", "high", "xhigh", "max"] {
            let a = parse_args(&run_min(&["--reasoning-effort", level])).unwrap();
            assert_eq!(a.reasoning_effort.as_deref(), Some(level));
            let p = endpoint_profile(a.keep_thinking, a.reasoning_effort.as_deref());
            assert_eq!(p.reasoning_effort.as_deref(), Some(level));
            assert!(p.thinking_off.is_some(), "effort never drops the knob");
        }
        let err = parse_args(&run_min(&["--reasoning-effort", "turbo"])).unwrap_err();
        assert!(err.contains("none|low|high|xhigh|max"), "{err}");
        assert!(parse_args(&run_min(&["--reasoning-effort"])).is_err());
    }

    #[test]
    fn steer_flag_defaults_off_and_records_both_arms() {
        // dphn-steer2 (2026-11-06): the nudges changed no named test on either
        // cell and cost 2.64x tokens per solved task, so the shipped default is
        // off; `on` stays available as the substrate for a future gate.
        let plain = parse_args(&run_min(&[])).unwrap();
        assert!(!plain.steer, "default ships without the nudges");
        assert!(!run_config(&plain).steer, "loop config carries it");
        let on = parse_args(&run_min(&["--steer", "on"])).unwrap();
        assert!(on.steer);
        assert!(run_config(&on).steer);
        let off = parse_args(&run_min(&["--steer", "off"])).unwrap();
        assert!(!off.steer);
        assert!(!run_config(&off).steer);
        let err = parse_args(&run_min(&["--steer", "sometimes"])).unwrap_err();
        assert!(err.contains("on|off"), "{err}");
    }

    #[test]
    fn streak_steer_flag_is_accepted() {
        assert!(!parse_args(&run_min(&[])).unwrap().streak_steer);
        let on = parse_args(&run_min(&["--streak-steer", "on"])).unwrap();
        assert!(on.streak_steer && run_config(&on).streak_steer);
        assert!(
            !parse_args(&run_min(&["--streak-steer", "off"]))
                .unwrap()
                .streak_steer
        );
        assert!(parse_args(&run_min(&["--streak-steer", "invalid"])).is_err());
    }

    #[test]
    fn deliverable_flags_parse_and_reject_escaping_paths() {
        let plain = parse_args(&run_min(&[])).unwrap();
        assert!(plain.deliverables.is_empty() && !plain.deliverable_steer);
        let on = parse_args(&run_min(&[
            "--deliverable",
            "primers.fasta",
            "--deliverable",
            "out/vm.js",
            "--deliverable-steer",
            "on",
        ]))
        .unwrap();
        assert_eq!(on.deliverables.len(), 2);
        let cfg = run_config(&on);
        assert!(cfg.deliverable_steer && cfg.deliverables == on.deliverables);
        for bad in ["/etc/passwd", "../x", "a/../../x", ""] {
            assert!(
                parse_args(&run_min(&["--deliverable", bad])).is_err(),
                "{bad}"
            );
        }
        assert!(parse_args(&run_min(&["--deliverable-steer", "maybe"])).is_err());
    }

    #[test]
    fn history_and_compaction_controls_validate_capacity_and_logging() {
        assert!(parse_args(&run_min(&["--history-read", "on"])).is_ok());
        assert!(parse_args(&run_min(&["--history-read", "on", "--no-log"])).is_err());
        assert!(parse_args(&run_min(&[
            "--compaction",
            "0.7",
            "--context-capacity",
            "131072",
            "--output-reserve",
            "32768",
            "--checkpoint-sources",
            "on"
        ]))
        .is_ok());
        assert!(parse_args(&run_min(&["--compaction", "0.7"])).is_err());
        assert!(parse_args(&run_min(&[
            "--compaction",
            "0.7",
            "--context-capacity",
            "131072",
            "--output-reserve",
            "8000"
        ]))
        .is_err());
        assert!(parse_args(&run_min(&[
            "--compaction",
            "0.7",
            "--context-capacity",
            "131072",
            "--output-reserve",
            "32768",
            "--max-tokens",
            "65536"
        ]))
        .is_err());
        assert!(parse_args(&run_min(&["--checkpoint-sources", "on"])).is_err());
    }

    #[test]
    fn evidence_and_completion_flags_parse_and_validate() {
        let a = parse_args(&run_min(&[
            "--repo-lookup",
            "on",
            "--repo-evidence",
            "on",
            "--completion-check",
            "cargo test",
        ]))
        .unwrap();
        assert!(a.repo_lookup && a.repo_evidence);
        assert_eq!(
            run_config(&a).completion_check.as_deref(),
            Some("cargo test")
        );
        assert!(run_config(&parse_args(&run_min(&[])).unwrap())
            .completion_check
            .is_none());
        assert!(parse_args(&run_min(&["--completion-check", "true", "--review", "on"])).is_err());
        assert!(parse_args(&run_min(&["--repo-evidence", "on"])).is_err());
        assert!(parse_args(&run_min(&["--completion-check", "  "])).is_err());
    }

    #[test]
    fn capability_flags_default_off_propagate_and_validate() {
        let plain = parse_args(&run_min(&[])).unwrap();
        assert!(!plain.review);
        assert!(plain.procedure_files.is_empty());
        assert!(!plain.preserve_failed_checks);
        assert!(!plain.minimal_workflow);
        assert_eq!(plain.observation_keep, None);
        assert_eq!(plain.stall_threshold, None);
        assert!(!plain.archive_output && !plain.shell && !plain.processes);
        assert!(!plain.repo_lookup && !plain.multi_edit);
        let cfg = run_config(&plain);
        let dflt = RunConfig::default();
        assert_eq!(cfg.preserve_failed_checks, dflt.preserve_failed_checks);
        assert_eq!(cfg.minimal_workflow, dflt.minimal_workflow);
        assert_eq!(cfg.observation_keep, dflt.observation_keep);
        assert_eq!(cfg.stall_threshold, dflt.stall_threshold);

        let on = parse_args(&run_min(&[
            "--preserve-failed-checks",
            "on",
            "--workflow",
            "minimal",
            "--observation-keep",
            "8",
            "--stall-threshold",
            "3",
            "--archive-output",
            "on",
            "--repo-lookup",
            "on",
            "--multi-edit",
            "on",
        ]))
        .unwrap();
        assert!(on.preserve_failed_checks && on.minimal_workflow);
        assert_eq!(
            (on.observation_keep, on.stall_threshold),
            (Some(8), Some(3))
        );
        assert!(on.archive_output && on.repo_lookup && on.multi_edit);
        let cfg = run_config(&on);
        assert!(cfg.preserve_failed_checks && cfg.minimal_workflow);
        assert_eq!((cfg.observation_keep, cfg.stall_threshold), (8, 3));
        assert!(parse_args(&run_min(&["--shell", "on"])).is_err());
        assert!(parse_args(&run_min(&["--processes", "on"])).is_err());
        assert!(parse_args(&run_min(&["--observation-keep", "0"])).is_err());
        let disabled_stall = parse_args(&run_min(&["--stall-threshold", "0"])).unwrap();
        assert_eq!(disabled_stall.stall_threshold, Some(0));
        assert_eq!(run_config(&disabled_stall).stall_threshold, 0);
        let wave2 = parse_args(&run_min(&[
            "--review",
            "on",
            "--procedure-file",
            "p1.json",
            "--procedure-file",
            "p2.json",
        ]))
        .unwrap();
        assert!(wave2.review);
        assert_eq!(
            wave2.procedure_files,
            vec![PathBuf::from("p1.json"), PathBuf::from("p2.json")]
        );
    }

    #[test]
    fn an_unenforceable_spend_cap_refuses_to_start() {
        // The default preset arms the `$` cap, so a model the price book
        // cannot price would leave only the token/step caps guarding spend.
        // That is a configuration error, and it must stop the run before any
        // provider request rather than warn and continue.
        let mut priced = args_for(Path::new("/tmp/w"), Some(60));
        priced.model = "deepseek-flash".into();
        let g = budget_for(&priced).expect("priced model starts");
        assert!(
            g.config().max_spend_cents.is_some(),
            "preset arms the $ cap"
        );
        assert_eq!(spend_guard_error(&priced, &g), None);

        let mut unpriced = args_for(Path::new("/tmp/w"), Some(60));
        unpriced.model = "deepseek-ai/DeepSeek-V4.1-Flash".into();
        let err = budget_for(&unpriced).err().expect("unpriced model refuses");
        assert!(
            err.contains("deepseek-ai/DeepSeek-V4.1-Flash") && err.contains("cannot be enforced"),
            "{err}"
        );

        // An org-prefixed spelling is the realistic mistake: the same model
        // priced under its bare id must still start.
        let mut go = args_for(Path::new("/tmp/w"), Some(60));
        go.model = "deepseek-v4.1-flash".into();
        assert!(budget_for(&go).is_ok(), "prefixed id is priced");

        // Explicit opt-out: the vLLM gateway id is deliberately unpriced, so
        // a driver that accepts unmetered dollars must say so. The guard is
        // then a warning, not a refusal, and the choice is on the row.
        let mut opted = args_for(Path::new("/tmp/w"), Some(60));
        opted.model = "deepseek-ai/DeepSeek-V4.1-Flash".into();
        opted.allow_inert_spend_cap = true;
        let g = budget_for(&opted).expect("opt-out starts");
        assert!(g.config().max_spend_cents.is_some(), "cap is still armed");
        assert!(
            inert_cap_accepted_warning(&opted, &g).is_some_and(|w| w.contains("inert")),
            "the accepted risk is named"
        );

        // No cap configured: an unpriced model is allowed, because there is
        // no guard to leave inert.
        let mut b = agent_budget::config_for(agent_budget::Capability::Interactive);
        b.max_spend_cents = None;
        let guard = BudgetGuard::new(b, Instant::now());
        assert_eq!(spend_guard_error(&unpriced, &guard), None);
    }
}
