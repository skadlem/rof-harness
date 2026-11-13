//! Minimal headless driver: `rof run ...` wires the frozen siblings once.
//! Prompt shape (goal + file map + named files via the context assembler)
//! lives in `agent_loop::run`; this crate only feeds it `RunConfig` + input.

mod agents;
mod chat;
mod cli;
mod dump;
mod eval_agent;
mod eval_cmd;
mod exit;
#[cfg(test)]
mod fixtures;
mod procedures;
mod workdir;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use agent_event::{AgentEvent, Emitter, RunOutcome, TurnEndReason};
use agent_loop::{FailureKind, Input, LoopState, NoBets, Outcome, Run};
use provider_core::{LlmClient, LlmError, Request, Response};
use provider_openai::OpenAiCompat;
use snapshot::TreeService;
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use tool_core::GrantGate;

use cli::{
    apply_env_defaults, budget_for, endpoint_profile, parse_args, parse_chat, resolve_endpoint,
    run_config, Args,
};
use dump::{attach_dump, finalize_dump};
use eval_cmd::{parse_eval, run_eval};
use exit::{exit_code, summarize, EXIT_NO_CREDENTIALS};
use workdir::{exclude_sidecar, resolve_log_path, validate_workdir};

/// Deliverable patch: `Ok` is the full uncut run patch for stdout (never the
/// 8KiB/200-line evidence bound); `Err` means no start HEAD was recorded
/// (unborn HEAD / empty workdir) and the run must not exit 0.
pub(crate) struct RunResult {
    pub outcome: Outcome,
    pub patch: Result<String, String>,
    pub events: Vec<AgentEvent>,
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

/// Tool policy for one run: `--allow-cmd` binary prefixes plus the
/// repeatable `--pass-env` allowlist. Provider-key-shaped names stay
/// withheld at spawn (the runner drops them), so listing a var forwards it
/// to `exec`/`test` children without ever leaking the provider key.
fn policy_for(args: &Args) -> tools_std::Policy {
    tools_std::Policy {
        root: args.workdir.clone(),
        // --allow-cmd entries are binary prefixes (no shell: `&&`/`|` are
        // literal args, so exact-string matching would allow nothing useful).
        allowed_commands: Vec::new(),
        allowed_prefixes: args.allow_cmd.clone(),
        syntax_cmd: None,
        denied_globs: tools_std::default_denied_globs(),
        pass_env: args.pass_env.clone(),
        allow_any_cmd: args.exec_policy == tools_std::ExecPolicy::Any,
        exec_wrap: args.exec_wrap.clone(),
    }
}

fn output_archive_dir(workdir: &Path) -> std::io::Result<std::path::PathBuf> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir();
    let work = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    let root = if root.starts_with(&work) {
        std::path::PathBuf::from("/var/tmp")
    } else {
        root
    };
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = root.join(format!(
        "rof-output-archive-{}-{nonce}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    if dir.starts_with(&work) {
        return Err(std::io::Error::other(
            "temporary output archive would be inside workdir",
        ));
    }
    Ok(dir)
}

fn tools_for(
    args: &Args,
    policy: Arc<tools_std::Policy>,
) -> Result<Vec<Arc<dyn tool_core::Tool>>, String> {
    let output_dir = if args.archive_output {
        Some(output_archive_dir(&args.workdir).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let history_log = if args.history_read {
        let path = resolve_log_path(args).ok_or("history_read requires the current run WAL")?;
        Some(if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(path)
        })
    } else {
        None
    };
    tools_std::experimental_tools(
        policy,
        tools_std::ExperimentalToolsConfig {
            output_dir,
            shell: args.shell,
            processes: args.processes,
            repo_lookup: args.repo_lookup,
            repo_evidence: args.repo_evidence,
            history_log,
            multi_edit: args.multi_edit,
        },
    )
}

/// Grant exactly the given tools to `agent`, plus `extra` names (`mcp__*`
/// before the MCP handshake; `spawn`/`wait` registered after the build).
fn registry_from(tools: Vec<Arc<dyn tool_core::Tool>>, extra: &[&str]) -> tool_core::Registry {
    let mut grants = tools
        .iter()
        .map(|tool| tool.definition().name)
        .collect::<Vec<_>>();
    grants.extend(extra.iter().map(|g| g.to_string()));
    let mut registry = tool_core::Registry::new(Arc::new(GrantGate::new(HashMap::from([(
        "agent".to_string(),
        grants,
    )]))));
    for tool in tools {
        registry.register(tool);
    }
    registry
}

fn registry_for(
    args: &Args,
    policy: Arc<tools_std::Policy>,
) -> Result<tool_core::Registry, String> {
    let mut extra = Vec::new();
    if !args.mcp.is_empty() {
        extra.push("mcp__*");
    }
    if args.agents.is_some() {
        extra.extend(["spawn", "wait"]);
    }
    Ok(registry_from(tools_for(args, policy)?, &extra))
}

/// `registry_for` plus one MCP stdio server per `--mcp` (tools `mcp__<name>`).
pub(crate) async fn registry_with_mcp(
    args: &Args,
    policy: Arc<tools_std::Policy>,
) -> Result<tool_core::Registry, String> {
    let mut reg = registry_for(args, policy)?;
    for cmd in &args.mcp {
        let argv = cmd.split_whitespace().map(str::to_owned).collect();
        tools_std::register_mcp_stdio(&mut reg, argv, args.exec_policy)
            .await
            .map_err(|e| format!("--mcp {cmd}: {e}"))?;
    }
    Ok(reg)
}

/// `--stream on`: route every call through `complete_streaming` and write text
/// deltas to `out` as they arrive. Display only; the returned Response is truth.
pub(crate) struct StreamPrint<'a, P> {
    pub inner: &'a P,
    pub out: Arc<std::sync::Mutex<dyn std::io::Write + Send>>,
}

#[async_trait::async_trait]
impl<P: LlmClient> LlmClient for StreamPrint<'_, P> {
    async fn complete(&self, model: &str, req: &Request) -> Result<Response, LlmError> {
        let mut wrote = false;
        let mut sink = |d: provider_core::Delta| {
            if let (provider_core::Delta::Text(t), Ok(mut o)) = (d, self.out.lock()) {
                wrote = true;
                let _ = o.write_all(t.as_bytes());
                let _ = o.flush();
            }
        };
        let res = self.inner.complete_streaming(model, req, &mut sink).await;
        if wrote {
            if let Ok(mut o) = self.out.lock() {
                let _ = o.write_all(b"\n");
            }
        }
        res
    }
}

fn parse_review_finding(text: &str) -> Option<(bool, String, String)> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    if object.len() != 3 {
        return None;
    }
    let defect = object.get("defect")?.as_bool()?;
    let evidence = object.get("evidence")?.as_str()?.trim();
    let reproduction = object.get("reproduction")?.as_str()?.trim();
    if evidence.is_empty() || reproduction.is_empty() {
        return None;
    }
    Some((defect, evidence.into(), reproduction.into()))
}

const REVIEW_PATCH_CHARS: usize = 12_000;

fn bounded_review_patch(patch: &str) -> String {
    let mut chars = patch.chars();
    let preview = chars.by_ref().take(REVIEW_PATCH_CHARS).collect::<String>();
    let omitted = chars.count();
    if omitted == 0 {
        preview
    } else {
        format!("{preview}\n\n[review context omitted {omitted} characters; use view/search to inspect the workspace]")
    }
}

struct CappedProvider<'a, P> {
    inner: &'a P,
    calls: std::sync::atomic::AtomicUsize,
    max: usize,
}

#[async_trait::async_trait]
impl<P: LlmClient> LlmClient for CappedProvider<'_, P> {
    async fn complete(&self, model: &str, req: &Request) -> Result<Response, LlmError> {
        use std::sync::atomic::Ordering;
        if self.calls.fetch_add(1, Ordering::Relaxed) >= self.max {
            return Err(LlmError::Transport(
                "review stage provider-step cap reached".into(),
            ));
        }
        self.inner.complete(model, req).await
    }
}

fn review_registry(policy: Arc<tools_std::Policy>) -> tool_core::Registry {
    let names = ["view", "search"].into_iter().map(str::to_owned).collect();
    let mut registry = tool_core::Registry::new(Arc::new(GrantGate::new(HashMap::from([(
        "agent".to_string(),
        names,
    )]))));
    registry.register(Arc::new(tools_std::view_tool(policy.clone())));
    registry.register(Arc::new(tools_std::search_tool(policy)));
    registry
}

#[allow(clippy::too_many_arguments)] // phase needs the caller-owned budget, emitter, cancellation and frozen run inputs.
async fn run_fresh_phase<P: LlmClient>(
    provider: &P,
    args: &Args,
    registry: &tool_core::Registry,
    state: &mut LoopState,
    emitter: &mut Emitter,
    bets_hook: &dyn agent_loop::BetsHook,
    cancel: &CancellationToken,
    phase: &str,
    input: String,
) -> Outcome {
    let log_path = if args.no_log {
        None
    } else {
        Some(args.workdir.join(format!(".rof-{phase}.jsonl")))
    };
    if let Some(path) = &log_path {
        if let Err(e) = preserve_log(path, &args.workdir) {
            return Outcome::Failed {
                kind: FailureKind::Log,
                message: format!("cannot preserve {phase} WAL: {e}"),
            };
        }
        exclude_sidecar(&args.workdir, path);
    }
    let mut stage_state = LoopState::new();
    std::mem::swap(&mut state.budget, &mut stage_state.budget);
    stage_state.usage_totals = state.usage_totals.clone();
    stage_state.emit_next = state.emit_next;
    stage_state.turn = state.turn;
    let mut cfg = run_config(args);
    cfg.goal = phase.into();
    cfg.log_path = log_path;
    if phase == "review" {
        cfg.minimal_workflow = true;
        cfg.incentives = agent_loop::IncentivesLevel::Contract;
        cfg.steer = false;
        cfg.proof_cmd = None;
    }
    let outcome = agent_loop::run(
        &mut stage_state,
        Run {
            provider,
            registry,
            agent: "agent",
            workdir: &args.workdir,
            emitter,
            bets: bets_hook,
            cfg,
        },
        vec![Input::User(input)],
        cancel,
    )
    .await;
    std::mem::swap(&mut state.budget, &mut stage_state.budget);
    state.usage_totals = stage_state.usage_totals;
    state.emit_next = stage_state.emit_next;
    state.turn = stage_state.turn;
    outcome
}

fn latest_assistant_text(emitter: &Emitter) -> Option<String> {
    emitter
        .history()
        .iter()
        .rev()
        .find_map(|event| match event {
            AgentEvent::MessageEnd { message, .. } => Some(message.content.clone()),
            _ => None,
        })
}

fn review_diagnostic(emitter: &mut Emitter, code: &str, message: &str) {
    emitter.emit(AgentEvent::Error {
        error: agent_event::AgentError {
            code: code.into(),
            message: message.into(),
        },
    });
}

fn preserve_log(path: &Path, workdir: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    let canonical_workdir = workdir.canonicalize()?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let canonical_parent = parent.canonicalize()?;
    let archive_dir = if canonical_parent.starts_with(&canonical_workdir) {
        canonical_workdir
            .parent()
            .ok_or_else(|| std::io::Error::other("workdir has no parent"))?
    } else {
        &canonical_parent
    };
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("log has no file name"))?
        .to_string_lossy();
    for n in 0u64.. {
        let archive = archive_dir.join(format!("{name}.{}.{}.bak", std::process::id(), n));
        match std::fs::hard_link(path, &archive) {
            Ok(()) => {
                std::fs::remove_file(path)?;
                eprintln!("preserved previous WAL at {}", archive.display());
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

async fn execute<P: LlmClient>(provider: &P, args: &Args) -> RunResult {
    execute_with_deadline(provider, args, None).await
}

/// Same run with an optional wall-clock deadline, used by the eval agent leg to
/// enforce the task's `[agent] timeout_sec` the way harbor does. The deadline
/// cancels through the same token Ctrl-C uses, so the loop still ends with
/// `Outcome::Cancelled`, emits `RunEnd` and leaves a gradeable workdir — a
/// dropped future would look like an infra failure instead.
async fn execute_with_deadline<P: LlmClient>(
    provider: &P,
    args: &Args,
    deadline: Option<Instant>,
) -> RunResult {
    let procedure_catalog = match procedures::load(&args.procedure_files) {
        Ok(selection) => selection,
        Err(message) => {
            return RunResult {
                outcome: Outcome::Failed {
                    kind: FailureKind::Input,
                    message,
                },
                patch: Ok(String::new()),
                events: Vec::new(),
            };
        }
    };
    let procedures = match procedure_catalog.select(&args.goal) {
        Ok(selection) => selection,
        Err(message) => {
            return RunResult {
                outcome: Outcome::Failed {
                    kind: FailureKind::Input,
                    message,
                },
                patch: Ok(String::new()),
                events: Vec::new(),
            }
        }
    };
    let policy = Arc::new(policy_for(args));
    let reg = match registry_with_mcp(args, policy.clone()).await {
        Ok(reg) => reg,
        Err(message) => {
            return RunResult {
                outcome: Outcome::Failed {
                    kind: FailureKind::Provider,
                    message: format!("cannot configure tools: {message}"),
                },
                patch: Ok(String::new()),
                events: Vec::new(),
            };
        }
    };
    let mut reg = reg;
    let mut agent_tree = args.agents.as_ref().map(agents::Tree::new);
    if let Some(t) = &agent_tree {
        t.register_root(&mut reg);
    }
    let mut state = LoopState::new();
    if let Some(p) = args.resume.as_deref() {
        if let Err(e) = state.restore_from(Path::new(p)) {
            return RunResult {
                outcome: Outcome::Failed {
                    kind: FailureKind::Log,
                    message: format!("--resume {p}: {e}"),
                },
                patch: Ok(String::new()),
                events: Vec::new(),
            };
        }
    }
    // Pre-seeded budget: `run` never rebuilds it (precedence is documented
    // on `agent_loop::run`: cfg-over-state). A `$` guard that cannot fire is a
    // configuration error, so it stops the run before any provider request
    // instead of warning and proceeding unguarded.
    state.budget = match budget_for(args) {
        Ok(budget) => budget,
        Err(message) => {
            return RunResult {
                outcome: Outcome::Failed {
                    kind: FailureKind::Input,
                    message,
                },
                patch: Ok(String::new()),
                events: Vec::new(),
            };
        }
    };
    let gate = Gate {
        incremental: args.proof_cmd.is_some(),
    };
    let bets_hook: &dyn agent_loop::BetsHook = if args.bets { &gate } else { &NoBets };
    let mut emitter = Emitter::new();
    let cancel = CancellationToken::new();
    if let Some(d) = deadline {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(tokio::time::Instant::from_std(d)).await;
            cancel.cancel();
        });
    }
    // First Ctrl-C cancels the run; the loop reports Outcome::Cancelled.
    tokio::spawn({
        let cancel = cancel.clone();
        async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancel.cancel();
            }
        }
    });
    // A fresh run restarts log sequence numbers; preserve the prior WAL
    // outside the workdir before creating a new one.
    let log_path = resolve_log_path(args);
    if let (Some(p), None) = (log_path.as_deref(), args.resume.as_deref()) {
        if let Err(e) = preserve_log(p, &args.workdir) {
            return RunResult {
                outcome: Outcome::Failed {
                    kind: FailureKind::Log,
                    message: format!("cannot preserve WAL: {e}"),
                },
                patch: Ok(String::new()),
                events: Vec::new(),
            };
        }
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
    let mut primary_input = args.goal.clone();
    if !procedures.instructions.is_empty() {
        primary_input.push_str("\n\nExternally sourced procedure guidance (applicability matched against the original task; no learning claim):\n");
        primary_input.push_str(&procedures.instructions);
    }
    let mut outcome = agents::with_children(
        agent_tree.as_mut(),
        provider,
        args,
        &policy,
        agent_loop::run(
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
            vec![Input::User(primary_input)],
            &cancel,
        ),
    )
    .await;
    let primary_outcome = outcome.clone();
    let mut effective_run_end = false;
    if args.review && matches!(outcome, Outcome::Done) && state.budget.exceeded().is_none() {
        let patch_before_review = tree.patch_since_start_full().unwrap_or_default();
        let review_prompt = format!(
            "Fresh-context review. Original task:\n{}\n\nCurrent full patch:\n{}\n\nUse only the available view/search tools to inspect the current workspace and patch. Look for a concrete defect and a reproducible way to show it. Do not edit files or run a hidden verifier. Return exactly one JSON object: {{\"defect\":boolean,\"evidence\":string,\"reproduction\":string}}. Both strings must be non-empty; if no concrete defect is supported, explain what you inspected and set reproduction to \"none found\".",
            args.goal, bounded_review_patch(&patch_before_review),
        );
        let review_tools = review_registry(policy.clone());
        let capped_provider = CappedProvider {
            inner: provider,
            calls: std::sync::atomic::AtomicUsize::new(0),
            max: 2,
        };
        let review_outcome = run_fresh_phase(
            &capped_provider,
            args,
            &review_tools,
            &mut state,
            &mut emitter,
            &NoBets,
            &cancel,
            "review",
            review_prompt,
        )
        .await;
        let finding = if matches!(review_outcome, Outcome::Done) {
            latest_assistant_text(&emitter).and_then(|text| parse_review_finding(&text))
        } else {
            None
        };
        match finding {
            Some((true, evidence, reproduction)) if state.budget.exceeded().is_none() => {
                review_diagnostic(
                    &mut emitter,
                    "review-defect",
                    &format!("evidence: {evidence}; reproduction: {reproduction}"),
                );
                let repair_input = format!(
                    "Continue the original task: {}\n\nA fresh read-only review found a concrete possible defect. Evidence: {}\nReproduction: {}\n\nInspect, reproduce with available tools when feasible, and repair the defect. Then run the relevant checks and report completion.",
                    args.goal, evidence, reproduction,
                );
                outcome = run_fresh_phase(
                    provider,
                    args,
                    &reg,
                    &mut state,
                    &mut emitter,
                    bets_hook,
                    &cancel,
                    "repair",
                    repair_input,
                )
                .await;
            }
            Some((true, evidence, reproduction)) => {
                review_diagnostic(&mut emitter, "review-repair-skipped", &format!("budget exhausted after finding: evidence: {evidence}; reproduction: {reproduction}"));
                outcome = primary_outcome.clone();
                effective_run_end = true;
            }
            Some((false, evidence, reproduction)) => {
                review_diagnostic(
                    &mut emitter,
                    "review-no-defect",
                    &format!("evidence: {evidence}; reproduction: {reproduction}"),
                );
                outcome = primary_outcome.clone();
                effective_run_end = true;
            }
            None if matches!(review_outcome, Outcome::Cancelled) => {
                review_diagnostic(
                    &mut emitter,
                    "review-cancelled",
                    "review was cancelled; cancellation applies to the full run",
                );
                outcome = Outcome::Cancelled;
            }
            None => {
                review_diagnostic(&mut emitter, "review-unknown", &format!("review ended as {review_outcome:?} or returned no valid structured evidence; retaining primary result"));
                outcome = primary_outcome.clone();
                effective_run_end = true;
            }
        }
    }
    if effective_run_end {
        let run_id = state.emit_next;
        state.emit_next = state.emit_next.saturating_add(1);
        emitter.emit(AgentEvent::RunStart {
            run_id,
            goal: "effective reviewed result".into(),
        });
        let has_latest_usage = emitter
            .history()
            .iter()
            .rev()
            .find_map(|e| match e {
                AgentEvent::TurnEnd { usage_totals, .. } => Some(usage_totals),
                _ => None,
            })
            .is_some_and(|usage| usage == &state.usage_totals);
        if !has_latest_usage {
            emitter.emit(AgentEvent::TurnStart { turn: state.turn });
            emitter.emit(AgentEvent::TurnEnd {
                turn: state.turn,
                reason: TurnEndReason::Completed,
                usage_totals: state.usage_totals.clone(),
            });
        }
        let final_event_outcome = match &outcome {
            Outcome::Done => RunOutcome::Passed,
            Outcome::Halted(s) => RunOutcome::Failed(s.clone()),
            Outcome::Failed { message, .. } => RunOutcome::Failed(message.clone()),
            Outcome::Cancelled => RunOutcome::Aborted,
        };
        emitter.emit(AgentEvent::RunEnd {
            outcome: final_event_outcome,
            messages: Vec::new(),
        });
    }
    // The per-Dispatch baseline commits absorb what landed, so the index-based
    // `tree.diff()` reports an empty patch; report the whole run instead.
    // Full uncut deliverable: no 8KiB/200-line evidence bound, new-file
    // contents included. `Err` is unborn HEAD / empty workdir (no baseline).
    let patch = tree.patch_since_start_full().map_err(|e| e.to_string());
    // Success exits 0 only when the patch computed: a `Done` run with no
    // baseline becomes a failure outcome, so the existing exit-code path
    // reports non-zero and `main` prints the stderr warning.
    let outcome = match (outcome, &patch) {
        (Outcome::Done, Err(e)) => Outcome::Failed {
            kind: FailureKind::Snapshot,
            message: format!("no baseline: {e}"),
        },
        (outcome, _) => outcome,
    };
    eprintln!("ablation {:?}", state.experiment.ablation);
    eprintln!("events {}", summarize(emitter.history()));
    RunResult {
        outcome,
        patch,
        events: emitter.history().to_vec(),
    }
}

/// Startup credential-gate message: names the env var the provider read so
/// the fix (export it, or point `--api-key-env` elsewhere) is on the line.
pub(crate) fn missing_credentials_message(api_key_env: &str, err: &str) -> String {
    format!("rof: {err}; set ${api_key_env} or pass --api-key-env NAME")
}

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("eval") {
        let eargs = match parse_eval(&argv) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        };
        std::process::exit(run_eval(&eargs).await);
    }
    let chat = argv.get(1).map(String::as_str) == Some("chat");
    let mut args = match if chat {
        parse_chat(&argv)
    } else {
        parse_args(&argv)
    } {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = apply_env_defaults(&mut args) {
        eprintln!("{e}");
        std::process::exit(2);
    }
    let _workdir = match validate_workdir(&args.workdir, args.allow_dirty_workdir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
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
    let mut provider = OpenAiCompat::new(
        &endpoint,
        endpoint_profile(args.keep_thinking, args.reasoning_effort.as_deref()),
    )
    .with_key_env(&args.api_key_env);
    for h in &args.headers {
        if let Some((name, value)) = h.split_once(':') {
            provider = provider.with_header(name, value);
        } else {
            eprintln!("--header needs NAME:VALUE, got {h:?}");
            std::process::exit(2);
        }
    }
    // Fail fast before any spend: S3's key-presence check, auth-setup exit.
    if let Err(e) = provider.check_credentials() {
        eprintln!(
            "{}",
            missing_credentials_message(&args.api_key_env, &e.to_string())
        );
        std::process::exit(EXIT_NO_CREDENTIALS);
    }
    if chat {
        let out = Arc::new(std::sync::Mutex::new(std::io::stdout()));
        let code = if args.stream {
            let p = StreamPrint {
                inner: &provider,
                out: out.clone(),
            };
            chat::run_chat(&p, &args, std::io::stdin().lock(), out).await
        } else {
            chat::run_chat(&provider, &args, std::io::stdin().lock(), out).await
        };
        std::process::exit(code);
    }
    let r = if args.stream {
        let p = StreamPrint {
            inner: &provider,
            out: Arc::new(std::sync::Mutex::new(std::io::stderr())),
        };
        execute(&p, &args).await
    } else {
        execute(&provider, &args).await
    };
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
    // The outcome carries the only human-readable reason a run stopped; without
    // this a refused start (exit 7) and a dead provider (3) look identical.
    match &r.outcome {
        Outcome::Failed { kind, message } => eprintln!("run failed ({kind:?}): {message}"),
        Outcome::Halted(reason) => eprintln!("run halted: {reason}"),
        Outcome::Cancelled => eprintln!("run cancelled"),
        Outcome::Done => {}
    }
    std::process::exit(exit_code(&r.outcome));
}

#[cfg(test)]
mod tests {
    use super::*;
    use fixtures::{args_for, edit_resp, text_resp, tmp, tool_resp, write_resp, ScriptClient};

    #[test]
    fn review_finding_requires_structured_evidence_and_reproduction() {
        assert_eq!(
            parse_review_finding(
                r#"{"defect":true,"evidence":"unchecked path","reproduction":"run x"}"#
            ),
            Some((true, "unchecked path".into(), "run x".into()))
        );
        assert_eq!(
            parse_review_finding(
                r#"{"defect":false,"evidence":"reviewed diff","reproduction":"none found"}"#
            ),
            Some((false, "reviewed diff".into(), "none found".into()))
        );
        assert!(
            parse_review_finding(r#"{"defect":true,"evidence":"","reproduction":"run x"}"#)
                .is_none()
        );
        assert!(parse_review_finding("no defects").is_none());
        assert!(parse_review_finding(
            r#"{"defect":false,"evidence":"ok","reproduction":"none","extra":1}"#
        )
        .is_none());
        let bounded = bounded_review_patch(&"x".repeat(REVIEW_PATCH_CHARS + 5));
        assert!(bounded.starts_with(&"x".repeat(REVIEW_PATCH_CHARS)));
        assert!(bounded.contains("omitted 5 characters"));
    }

    #[test]
    fn startup_credential_gate_names_the_var() {
        // Unique env var, set-then-removed in this test only, so parallel
        // tests cannot observe it.
        std::env::remove_var("ROF_TEST_CRED_GATE_KEY");
        let p = OpenAiCompat::new("http://127.0.0.1:1", cli::endpoint_profile(false, None))
            .with_key_env("ROF_TEST_CRED_GATE_KEY");
        let err = p.check_credentials().unwrap_err().to_string();
        let msg = missing_credentials_message("ROF_TEST_CRED_GATE_KEY", &err);
        assert!(
            msg.contains("ROF_TEST_CRED_GATE_KEY") && msg.contains("--api-key-env"),
            "the stderr line says how to fix it: {msg}"
        );
        assert_eq!(
            EXIT_NO_CREDENTIALS, 4,
            "distinct from 2 (usage) and 3 (run)"
        );
        std::env::set_var("ROF_TEST_CRED_GATE_KEY", "k");
        assert!(p.check_credentials().is_ok());
        std::env::remove_var("ROF_TEST_CRED_GATE_KEY");
    }

    #[test]
    fn pass_env_flag_reaches_policy() {
        let mut a = args_for(Path::new("/tmp/w"), None);
        assert!(
            policy_for(&a).pass_env.is_empty(),
            "absent flag forwards nothing extra"
        );
        a.pass_env = vec!["FOO".into(), "BAR".into()];
        assert_eq!(
            policy_for(&a).pass_env,
            vec!["FOO".to_string(), "BAR".to_string()]
        );
        assert_eq!(
            policy_for(&a).root,
            Path::new("/tmp/w"),
            "policy root still the workdir"
        );
    }

    const MCP_SERVER: &str = r#"while read -r l; do
  id=$(printf %s "$l" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$l" in
    *tools/list*) echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"echo\",\"description\":\"d\",\"inputSchema\":{\"type\":\"object\"}}]}}";;
    *) echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{}}";;
  esac
done"#;

    #[tokio::test]
    async fn mcp_flag_registers_granted_tools_and_needs_exec_policy_any() {
        let dir = crate::fixtures::tmp();
        let script = dir.join("mcp.sh");
        std::fs::write(&script, MCP_SERVER).unwrap();
        let mut args = crate::fixtures::args_for(&dir, None);
        args.mcp = vec![format!("sh {}", script.display())];
        args.exec_policy = tools_std::ExecPolicy::Allowlist;
        let denied = registry_with_mcp(&args, Arc::new(policy_for(&args))).await;
        assert!(denied.is_err_and(|e| e.contains("exec policy any")));
        args.exec_policy = tools_std::ExecPolicy::Any;
        let reg = registry_with_mcp(&args, Arc::new(policy_for(&args)))
            .await
            .unwrap();
        assert!(reg.definitions().iter().any(|d| d.name == "mcp__echo"));
        let call = tool_core::ToolCall {
            call_id: "1".into(),
            name: "mcp__echo".into(),
            args: serde_json::json!({}),
        };
        assert!(matches!(
            reg.prepare("agent", call),
            tool_core::CallStatus::Dispatch(_)
        ));
    }

    #[test]
    fn registry_exposes_and_grants_only_enabled_experimental_tools() {
        let workdir =
            std::env::temp_dir().join(format!("rof-registry-work-{}", std::process::id()));
        std::fs::create_dir_all(&workdir).unwrap();
        let mut args = args_for(&workdir, None);
        let default = registry_for(&args, Arc::new(policy_for(&args))).unwrap();
        let names = default
            .definitions()
            .into_iter()
            .map(|d| d.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec!["edit", "exec", "search", "test", "view", "write"]
        );
        assert!(matches!(
            default.prepare(
                "agent",
                tool_core::ToolCall {
                    call_id: "disabled".into(),
                    name: "repo_lookup".into(),
                    args: serde_json::json!({"symbol":"Thing"}),
                }
            ),
            tool_core::CallStatus::Result(_)
        ));

        args.exec_policy = tools_std::ExecPolicy::Any;
        args.archive_output = true;
        args.shell = true;
        args.processes = true;
        args.repo_lookup = true;
        args.multi_edit = true;
        let enabled = registry_for(&args, Arc::new(policy_for(&args))).unwrap();
        let names = enabled
            .definitions()
            .into_iter()
            .map(|d| d.name)
            .collect::<Vec<_>>();
        for name in [
            "output_read",
            "shell",
            "process",
            "repo_lookup",
            "multi_edit",
        ] {
            assert!(names.iter().any(|n| n == name), "missing {name}: {names:?}");
            assert!(
                matches!(
                    enabled.prepare(
                        "agent",
                        tool_core::ToolCall {
                            call_id: name.into(),
                            name: name.into(),
                            args: match name {
                                "repo_lookup" => serde_json::json!({"symbol":"Thing"}),
                                "multi_edit" => serde_json::json!({"edits":[]}),
                                "output_read" => serde_json::json!({"handle":"unknown"}),
                                "process" => serde_json::json!({"action":"start","cmd":"true"}),
                                _ => serde_json::json!({"cmd":"true"}),
                            },
                        }
                    ),
                    tool_core::CallStatus::Dispatch(_)
                ),
                "enabled tool {name} must be granted"
            );
        }
        let archive = output_archive_dir(&args.workdir).unwrap();
        assert!(!archive.starts_with(&args.workdir));
        std::fs::remove_dir_all(workdir).unwrap();
    }

    #[tokio::test]
    async fn collapsed_historical_observation_is_recovered_from_the_current_wal() {
        struct HistoryClient(std::sync::atomic::AtomicUsize);
        #[async_trait::async_trait]
        impl LlmClient for HistoryClient {
            async fn complete(&self, _: &str, req: &Request) -> Result<Response, LlmError> {
                use provider_core::ToolCallRef;
                use std::sync::atomic::Ordering;
                let mut response = text_resp();
                response.message.tool_calls = match self.0.fetch_add(1, Ordering::Relaxed) {
                    0 => vec![ToolCallRef {
                        id: "one-time".into(),
                        name: "exec".into(),
                        args: serde_json::json!({"cmd":"python3 -c \"from pathlib import Path; p=Path('transient.txt'); print(p.read_text()); p.write_text('changed')\""}),
                    }],
                    1 => (0..6)
                        .map(|i| ToolCallRef {
                            id: format!("note-{i}"),
                            name: "view".into(),
                            args: serde_json::json!({"path":format!("note-{i}.txt")}),
                        })
                        .collect(),
                    2 => {
                        let old = req
                            .messages
                            .iter()
                            .find(|m| m.tool_call_id.as_deref() == Some("one-time"))
                            .unwrap();
                        assert!(old.content.starts_with("[collapsed:"));
                        assert!(!old.content.contains("HISTORICAL-SENTINEL"));
                        let id = old
                            .content
                            .split("item_id=")
                            .nth(1)
                            .unwrap()
                            .split(';')
                            .next()
                            .unwrap();
                        vec![ToolCallRef {
                            id: "recover-old".into(),
                            name: "history_read".into(),
                            args: serde_json::json!({"item_id":id,"offset":400,"limit":200}),
                        }]
                    }
                    3 => {
                        let recovered = req
                            .messages
                            .iter()
                            .find(|m| m.tool_call_id.as_deref() == Some("recover-old"))
                            .unwrap();
                        assert!(
                            recovered.content.contains("HISTORICAL-SENTINEL"),
                            "{}",
                            recovered.content
                        );
                        assert!(recovered.content.to_lowercase().contains("historical"));
                        vec![]
                    }
                    _ => panic!("unexpected provider request"),
                };
                if !response.message.tool_calls.is_empty() {
                    response.stop = provider_core::StopReason::ToolUse;
                }
                Ok(response)
            }
        }
        let work = tmp();
        std::fs::write(
            work.join("transient.txt"),
            format!("{}HISTORICAL-SENTINEL", "x".repeat(500)),
        )
        .unwrap();
        for i in 0..6 {
            std::fs::write(work.join(format!("note-{i}.txt")), "note").unwrap();
        }
        let mut args = args_for(&work, None);
        args.history_read = true;
        args.exec_policy = tools_std::ExecPolicy::Any;
        args.incentives = agent_loop::IncentivesLevel::Base;
        args.steer = false;
        let provider = HistoryClient(std::sync::atomic::AtomicUsize::new(0));
        let result = execute(&provider, &args).await;
        assert!(
            matches!(result.outcome, Outcome::Done),
            "{:?}",
            result.outcome
        );
        assert_eq!(provider.0.load(std::sync::atomic::Ordering::Relaxed), 4);
        assert_eq!(
            std::fs::read_to_string(work.join("transient.txt")).unwrap(),
            "changed"
        );
        let wal = std::fs::read_to_string(work.join(".rof-events.jsonl")).unwrap();
        assert!(wal
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .any(|item| item["kind"] == "ToolResult"
                && item["call_id"] == "one-time"
                && item["content"]
                    .as_str()
                    .unwrap()
                    .contains("HISTORICAL-SENTINEL")));
        std::fs::remove_dir_all(work).unwrap();
    }

    #[tokio::test]
    async fn public_completion_predicate_uses_real_policy_and_records_results() {
        for (command, expected_code, checks) in [("true", 0, 1), ("false", 8, 2)] {
            let work = tmp();
            std::fs::write(work.join("note.txt"), "unchanged").unwrap();
            let mut args = args_for(&work, None);
            args.completion_check = Some(command.into());
            args.allow_cmd = vec![command.into()];
            let provider = ScriptClient::new(vec![text_resp(), text_resp()]);
            let result = execute(&provider, &args).await;
            assert_eq!(
                exit_code(&result.outcome),
                expected_code,
                "{:?}",
                result.outcome
            );
            assert_eq!(
                result
                    .events
                    .iter()
                    .filter(|e| matches!(e, AgentEvent::ToolStart { name, .. } if name == "test"))
                    .count(),
                checks
            );
            assert_eq!(
                result
                    .events
                    .iter()
                    .filter(|e| matches!(e, AgentEvent::ToolEnd { .. }))
                    .count(),
                checks
            );
            assert_eq!(
                std::fs::read_to_string(work.join("note.txt")).unwrap(),
                "unchanged"
            );
            assert!(result.patch.unwrap().is_empty());
            let wal = std::fs::read_to_string(work.join(".rof-events.jsonl")).unwrap();
            assert!(wal.contains("harness-origin completion-check"));
            std::fs::remove_dir_all(work).unwrap();
        }
    }

    #[tokio::test]
    async fn evidence_flag_reaches_real_registry_without_changing_defaults() {
        let work = tmp();
        std::fs::write(work.join("lib.rs"), "pub fn Rocket() {}\n").unwrap();
        let mut args = args_for(&work, None);
        args.repo_lookup = true;
        args.repo_evidence = true;
        let registry = registry_for(&args, Arc::new(policy_for(&args))).unwrap();
        let call = tool_core::ToolCall {
            call_id: "lookup".into(),
            name: "repo_lookup".into(),
            args: serde_json::json!({"symbol":"Rocket"}),
        };
        let tool_core::CallStatus::Dispatch(inv) = registry.prepare("agent", call) else {
            panic!("lookup not dispatched")
        };
        let result = registry
            .resolve("repo_lookup")
            .unwrap()
            .execute(inv, CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success, "{}", result.content);
        assert!(result.content.contains("definition"), "{}", result.content);
        assert!(result.content.contains("lib.rs:1"), "{}", result.content);
        std::fs::remove_dir_all(work).unwrap();
    }

    #[tokio::test]
    async fn collapsed_archive_observation_remains_retrievable() {
        struct ArchiveClient(std::sync::atomic::AtomicUsize);

        #[async_trait::async_trait]
        impl LlmClient for ArchiveClient {
            async fn complete(&self, _: &str, req: &Request) -> Result<Response, LlmError> {
                use provider_core::ToolCallRef;
                use std::sync::atomic::Ordering;

                let mut response = text_resp();
                response.message.tool_calls = match self.0.fetch_add(1, Ordering::Relaxed) {
                    0 => vec![ToolCallRef {
                        id: "archived".into(),
                        name: "exec".into(),
                        args: serde_json::json!({"cmd": "python3 -c \"print('x'*9000); print('RECOVERED-END')\""}),
                    }],
                    1 => (0..6)
                        .map(|i| ToolCallRef {
                            id: format!("view-{i}"),
                            name: "view".into(),
                            args: serde_json::json!({"path": format!("note-{i}.txt")}),
                        })
                        .collect(),
                    2 => {
                        let old = req
                            .messages
                            .iter()
                            .find(|m| m.tool_call_id.as_deref() == Some("archived"))
                            .expect("archived observation remains in history");
                        assert!(old.content.starts_with("[collapsed:"));
                        let handle = old
                            .content
                            .split_whitespace()
                            .find_map(|word| word.strip_prefix("handle="))
                            .expect("collapsed observation must retain its retrieval handle");
                        vec![ToolCallRef {
                            id: "recovered".into(),
                            name: "output_read".into(),
                            args: serde_json::json!({"handle": handle, "offset": 9000, "limit": 32}),
                        }]
                    }
                    3 => {
                        let recovered = req
                            .messages
                            .iter()
                            .find(|m| m.tool_call_id.as_deref() == Some("recovered"))
                            .expect("output_read result reaches the provider");
                        assert!(recovered.content.contains("RECOVERED-END"));
                        Vec::new()
                    }
                    _ => panic!("unexpected extra provider request"),
                };
                if !response.message.tool_calls.is_empty() {
                    response.stop = provider_core::StopReason::ToolUse;
                }
                Ok(response)
            }
        }

        let workdir = tmp().join("work");
        std::fs::create_dir_all(&workdir).unwrap();
        std::fs::write(workdir.join("note.txt"), "hello\n").unwrap();
        for i in 0..6 {
            std::fs::write(workdir.join(format!("note-{i}.txt")), "hello\n").unwrap();
        }
        let mut args = args_for(&workdir, None);
        args.archive_output = true;
        args.exec_policy = tools_std::ExecPolicy::Any;
        args.incentives = agent_loop::IncentivesLevel::Base;
        let client = ArchiveClient(std::sync::atomic::AtomicUsize::new(0));
        let result = execute_with_deadline(&client, &args, None).await;
        assert!(
            matches!(result.outcome, Outcome::Done),
            "{:?}",
            result.outcome
        );
        assert_eq!(client.0.load(std::sync::atomic::Ordering::Relaxed), 4);
        std::fs::remove_dir_all(workdir).unwrap();
    }

    #[tokio::test]
    async fn deadline_cancels_the_run_and_still_emits_run_end() {
        // Harbor parity for the eval agent leg: an expired deadline must end the
        // run through the same cancel token Ctrl-C uses, so the loop reports
        // Outcome::Cancelled and still emits RunEnd (a dropped future would look
        // like an infra failure instead).
        let dir = tmp().join("work");
        std::fs::create_dir_all(&dir).unwrap();
        // Non-empty tree: an empty workdir has an unborn HEAD and no baseline
        // to diff from, which fails before the deadline can be observed.
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        let args = args_for(&dir, None);
        // Already expired: the scripted provider is never consulted.
        let past = Instant::now() - std::time::Duration::from_millis(1);
        let r =
            execute_with_deadline(&ScriptClient::new(vec![text_resp()]), &args, Some(past)).await;
        assert!(
            matches!(r.outcome, Outcome::Cancelled),
            "expired deadline cancels, got {:?}",
            r.outcome
        );
        assert!(
            r.events
                .iter()
                .any(|e| matches!(e, AgentEvent::RunEnd { .. })),
            "cancelled runs still emit RunEnd so the row is gradeable"
        );
        // A future deadline is inert: same scripted run, fresh workdir.
        let dir2 = tmp().join("work");
        std::fs::create_dir_all(&dir2).unwrap();
        std::fs::write(dir2.join("note.txt"), "hello\n").unwrap();
        let args2 = args_for(&dir2, None);
        let ok = execute_with_deadline(
            &ScriptClient::new(vec![text_resp()]),
            &args2,
            Some(Instant::now() + std::time::Duration::from_secs(30)),
        )
        .await;
        assert!(
            matches!(ok.outcome, Outcome::Done),
            "future deadline is inert, got {:?}",
            ok.outcome
        );
    }

    #[tokio::test]
    async fn repeated_run_preserves_previous_wal_outside_workdir() {
        let parent = tmp();
        let dir = parent.join("work");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        let args = args_for(&dir, None);
        let first = execute(&ScriptClient::new(vec![text_resp()]), &args).await;
        assert!(matches!(first.outcome, Outcome::Done));
        let old = std::fs::read_to_string(dir.join(".rof-events.jsonl")).unwrap();
        let second = execute(&ScriptClient::new(vec![text_resp()]), &args).await;
        assert!(matches!(second.outcome, Outcome::Done));
        assert!(
            std::fs::read_dir(&parent).unwrap().flatten().any(|e| {
                e.path().is_file()
                    && std::fs::read_to_string(e.path()).ok().as_deref() == Some(&old)
            }),
            "previous WAL must survive outside the workdir"
        );
    }

    #[test]
    fn custom_log_archives_are_unique_and_preservation_fails_closed() {
        let parent = tmp();
        let work = parent.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let path = work.join("custom.jsonl");
        for data in ["first", "second"] {
            std::fs::write(&path, data).unwrap();
            preserve_log(&path, &work).unwrap();
        }
        let archived: Vec<String> = std::fs::read_dir(&parent)
            .unwrap()
            .flatten()
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .collect();
        assert!(archived.contains(&"first".to_string()));
        assert!(archived.contains(&"second".to_string()));
        std::fs::create_dir(&path).unwrap();
        assert!(preserve_log(&path, &work).is_err());
        assert!(path.is_dir());
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
    async fn review_finding_gets_one_fresh_repair_with_cumulative_usage_and_wals() {
        let work = tmp();
        std::fs::write(work.join("note.txt"), "hello").unwrap();
        let mut args = args_for(&work, Some(8));
        args.review = true;
        args.incentives = agent_loop::IncentivesLevel::Contract;
        let mut finding = text_resp();
        finding.message.content = r#"{"defect":true,"evidence":"the note still has the placeholder","reproduction":"view note.txt and observe 'bye'"}"#.into();
        let provider = ScriptClient::new(vec![
            edit_resp("note.txt", "hello", "bye"),
            text_resp(),
            finding,
            edit_resp("note.txt", "bye", "fixed"),
            text_resp(),
        ]);
        let result = execute(&provider, &args).await;
        assert!(matches!(result.outcome, Outcome::Done));
        assert_eq!(
            std::fs::read_to_string(work.join("note.txt")).unwrap(),
            "fixed"
        );
        let endings = result
            .events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::RunEnd { outcome, .. } => Some(outcome),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(endings.last(), Some(&&RunOutcome::Passed));
        assert!(result
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::Error { error } if error.code == "review-defect")));
        let final_usage = result
            .events
            .iter()
            .rev()
            .find_map(|e| match e {
                AgentEvent::TurnEnd { usage_totals, .. } => Some(usage_totals),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            (final_usage.input_tokens, final_usage.output_tokens),
            (5, 5)
        );
        assert!(work.join(".rof-review.jsonl").is_file());
        assert!(work.join(".rof-repair.jsonl").is_file());
        let patch = result.patch.as_ref().unwrap();
        assert!(
            !patch.contains(".rof-review.jsonl"),
            "review WAL leaked into deliverable patch"
        );
        assert!(
            !patch.contains(".rof-repair.jsonl"),
            "repair WAL leaked into deliverable patch"
        );
        for path in [".rof-review.jsonl", ".rof-repair.jsonl"] {
            let tracked = std::process::Command::new("git")
                .args(["ls-files", "--", path])
                .current_dir(&work)
                .output()
                .unwrap();
            assert!(tracked.status.success());
            assert!(
                tracked.stdout.is_empty(),
                "{path} is tracked as a deliverable sidecar"
            );
        }
        std::fs::remove_dir_all(work).unwrap();
    }

    #[tokio::test]
    async fn malformed_review_retains_primary_and_emits_aggregate_usage_and_run_end() {
        let work = tmp();
        std::fs::write(work.join("note.txt"), "hello").unwrap();
        let mut args = args_for(&work, Some(8));
        args.review = true;
        args.incentives = agent_loop::IncentivesLevel::Contract;
        let provider = ScriptClient::new(vec![
            edit_resp("note.txt", "hello", "bye"),
            text_resp(),
            text_resp(),
        ]);
        let result = execute(&provider, &args).await;
        assert!(matches!(result.outcome, Outcome::Done));
        assert_eq!(
            std::fs::read_to_string(work.join("note.txt")).unwrap(),
            "bye"
        );
        assert!(result
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::Error { error } if error.code == "review-unknown")));
        assert!(matches!(
            result
                .events
                .iter()
                .rev()
                .find(|e| matches!(e, AgentEvent::RunEnd { .. })),
            Some(AgentEvent::RunEnd {
                outcome: RunOutcome::Passed,
                ..
            })
        ));
        let latest = result
            .events
            .iter()
            .rev()
            .find_map(|e| match e {
                AgentEvent::TurnEnd { usage_totals, .. } => Some(usage_totals),
                _ => None,
            })
            .unwrap();
        assert_eq!((latest.input_tokens, latest.output_tokens), (3, 3));
        assert!(!work.join(".rof-repair.jsonl").exists());
        std::fs::remove_dir_all(work).unwrap();
    }

    #[tokio::test]
    async fn exhausted_shared_budget_skips_review() {
        let work = tmp();
        std::fs::write(work.join("note.txt"), "hello").unwrap();
        let mut args = args_for(&work, Some(2));
        args.review = true;
        args.incentives = agent_loop::IncentivesLevel::Contract;
        let provider = ScriptClient::new(vec![edit_resp("note.txt", "hello", "bye"), text_resp()]);
        let result = execute(&provider, &args).await;
        assert!(matches!(result.outcome, Outcome::Done));
        assert!(!result
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::RunStart { goal, .. } if goal == "review")));
        assert!(!work.join(".rof-review.jsonl").exists());
        std::fs::remove_dir_all(work).unwrap();
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
    async fn empty_workdir_still_gets_a_baseline_so_the_patch_is_real() {
        // A task whose image ships an EMPTY /app (kv-store-grpc) starts with an
        // empty workdir. `ensure()` now records an empty baseline commit
        // (`--allow-empty`) instead of leaving HEAD unborn, so the run does not
        // die with "no baseline" and the deliverable patch contains what the
        // agent created. Measured before the fix: a row that resolved 7/7 still
        // carried halt_reason "snapshot rollback: ... not recorded a start HEAD".
        let dir = tmp();
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
        let client = ScriptClient::new(vec![
            write_resp("new.txt", "created\n"),
            text_resp(),
            text_resp(),
        ]);
        let r = execute(&client, &args_for(&dir, None)).await;
        assert!(
            matches!(r.outcome, Outcome::Done),
            "empty start is not a failure: {:?}",
            r.outcome
        );
        let patch = r.patch.expect("empty baseline still yields a patch");
        assert!(
            patch.contains("new.txt"),
            "the patch carries what the agent created: {patch}"
        );
        assert_eq!(exit_code(&r.outcome), 0);
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
