//! Minimal headless driver: `rof run ...` wires the frozen siblings once.
//! Prompt shape (goal + file map + named files via the context assembler)
//! lives in `agent_loop::run`; this crate only feeds it `RunConfig` + input.

mod cli;
mod dump;
mod eval_cmd;
mod exit;
#[cfg(test)]
mod fixtures;
mod workdir;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use agent_event::{AgentEvent, Emitter};
use agent_loop::{FailureKind, Input, LoopState, NoBets, Outcome, Run};
use provider_core::LlmClient;
use provider_openai::{EndpointProfile, OpenAiCompat};
use snapshot::TreeService;
use tokio_util::sync::CancellationToken;
use tool_core::GrantGate;

use cli::{budget_for, parse_args, resolve_endpoint, run_config, Args};
use dump::{attach_dump, finalize_dump};
use eval_cmd::{parse_eval, run_eval};
use exit::{exit_code, summarize};
use workdir::{exclude_sidecar, resolve_log_path, validate_workdir};

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
        pass_env: Vec::new(),
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
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let _workdir = match validate_workdir(&args.workdir) {
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
    use fixtures::{args_for, edit_resp, text_resp, tmp, tool_resp, write_resp, ScriptClient};

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
            matches!(&r.outcome, Outcome::Failed { message: m, .. } if m.contains("no baseline")),
            "failure outcome carries the stderr hint: {:?}",
            r.outcome
        );
        assert_eq!(exit_code(&r.outcome), 3);
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
