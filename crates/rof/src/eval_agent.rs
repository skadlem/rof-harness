//! `rof eval --agent`: per-instance agent leg, ported verbatim from the
//! frozen bash pilot driver (`~/.local/share/rof-tb/work/pilot-r1-driver.sh`,
//! `mk_goal` plain variant + `verdict` function). Fresh seeded workdir, the
//! loop run in-process through the exact `rof run` wiring (`crate::execute`),
//! then graded via the harbor CLI. Infra failures (workdir refusal, missing
//! harbor, grading timeout, run without RunEnd) are never capability.

use std::path::Path;
use std::time::{Duration, Instant};

use agent_event::AgentEvent;
use provider_openai::OpenAiCompat;

use crate::cli::{endpoint_profile, Args};
use crate::dump::finalize_dump;
use crate::eval_cmd::{apply_mode_for, EvalArgs};
use crate::exit::EXIT_NO_CREDENTIALS;
use crate::workdir::validate_workdir;
use crate::{execute, missing_credentials_message};

/// Goal rules tail, ported verbatim from the driver's `mk_goal` (plain
/// variant).
const GOAL_TAIL: &str = "\n\nRules: all paths are relative to the workdir (a /app prefix maps to the workdir root). The exec tool runs single commands with NO shell (no pipes, &&, or redirection) one at a time; its stdin is closed, so heredocs and stdin reads return nothing; write scripts to files and run them when you need composition. Do not add test-harness files. When your work is complete, reply with a text message and no tool calls.";

/// Goal text from the instruction: `/app/` goes first so the remaining bare
/// `/app` maps to the workdir. Ported verbatim (mk_goal plain variant).
pub(crate) fn agent_goal(instruction: &str) -> String {
    format!(
        "{}{}",
        instruction
            .replace("/app/", "")
            .replace("/app", "the workdir"),
        GOAL_TAIL
    )
}

/// solve.sh content written into the grade tree, verbatim from the driver.
const SOLVE_SH: &str = "#!/bin/bash\nset -euo pipefail\nSCRIPT_DIR=\"$(cd \"$(dirname \"$0\")\" && pwd)\"\ncp -r \"$SCRIPT_DIR/files/.\" /app/\n";

/// Agent workdir -> `solution/files` packaging exclusions, driver parity:
/// the bash driver checked the FIRST relative path component for
/// `.git`/`__pycache__`/`.pytest_cache` (a nested repo a task builds must
/// still reach the container), `.pyc` by suffix at any depth, plus the WAL
/// sidecar — an exclusion the driver predates (it had no default WAL).
const EXCLUDED_DIRS: [&str; 3] = [".git", "__pycache__", ".pytest_cache"];
const WAL_SIDECAR: &str = ".rof-events.jsonl";

/// Copy every file from `src` (recursive, contents preserved) into `dst`
/// keeping relative paths, excluding seeds the packaging must not carry.
/// Fills `solution/files` for the grade tree.
pub(crate) fn pack_workdir(src: &Path, dst: &Path) -> std::io::Result<()> {
    pack_dir(src, dst, true)
}

/// `top` marks the workdir root, where the excluded-dir rule applies
/// (driver parity: `rel.parts[0]` only — nested dirs ride along).
fn pack_dir(src: &Path, dst: &Path, top: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let ty = entry.file_type()?;
        let to = dst.join(&name);
        if ty.is_dir() {
            if top && EXCLUDED_DIRS.contains(&name.as_str()) {
                continue;
            }
            pack_dir(&entry.path(), &to, false)?;
        } else if name == WAL_SIDECAR || name.ends_with(".pyc") {
            continue;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Recursive plain copy: `dst` gets `src`'s whole tree (no exclusion rules).
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Fresh workdir per run (driver parity: `rm -rf` then `mkdir`), seeded with
/// the CONTENTS of `<task>/environment` when it exists, else left empty
/// (can_create tasks).
fn fresh_workdir(task_dir: &Path, work: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_dir_all(work);
    std::fs::create_dir_all(work)?;
    match std::fs::read_dir(task_dir.join("environment")) {
        Ok(_) => copy_tree(&task_dir.join("environment"), work),
        Err(_) => Ok(()),
    }
}

/// Run-config `Args` for one instance: goal/workdir/events set here,
/// provider knobs passed through, budgets filled from the flags with the
/// driver-recipe defaults. Everything not mentioned stays at `rof run`
/// defaults (context_files empty, WAL inside the workdir, no bets/proof/
/// compaction, Full incentives — the `rof run` parse default).
pub(crate) fn agent_args(a: &EvalArgs, work: &Path, goal: &str, events_path: &Path) -> Args {
    // Budget defaults are chosen: copied from the frozen pilot recipe
    // (NEXT.md, 300000 tokens / 60 steps / 120 actions); validated when the
    // slice runs. `cli::budget_for` stays the one resolver.
    Args {
        goal: goal.into(),
        workdir: work.into(),
        // parse_eval refuses `--agent` without `--model`.
        model: a.model.clone().unwrap_or_default(),
        endpoint: a.endpoint.clone(),
        api_key_env: a
            .api_key_env
            .clone()
            .unwrap_or_else(|| "OPENAI_API_KEY".into()),
        headers: a.headers.clone(),
        allow_cmd: a.allow_cmd.clone(),
        budget_steps: Some(a.budget_steps.unwrap_or(60)),
        budget_actions: Some(a.budget_actions.unwrap_or(120)),
        budget_tokens: Some(a.budget_tokens.unwrap_or(300_000)),
        max_tokens: a.max_tokens,
        context_files: Vec::new(),
        dump_events: Some(events_path.to_string_lossy().into_owned()),
        log_path: None,
        no_log: false,
        // The eval leg builds its provider from `EvalArgs.keep_thinking`
        // directly (run_agent_leg); the run-Args copy stays consistent.
        keep_thinking: a.keep_thinking,
        proof_cmd: None,
        incentives: agent_loop::IncentivesLevel::Full,
        bets: false,
        compaction: None,
        thinking_keep: None,
        collapse_hysteresis: None,
        allow_dirty_workdir: false,
        pass_env: Vec::new(),
        exec_wrap: None,
    }
}

/// Job yaml: JSON written to a `.yaml` file, exactly the driver's shape
/// (`json.dumps` into the harbor `-c` file; JSON is valid YAML). Factored
/// for tests.
pub(crate) fn job_json(id: &str, grade_dir: &Path, jobs_dir: &Path) -> serde_json::Value {
    serde_json::json!({
        "job_name": format!("eval-{id}"),
        "jobs_dir": jobs_dir.display().to_string(),
        "tasks": [{"path": grade_dir.display().to_string()}],
        "agents": [{"name": "oracle"}],
    })
}

/// Reward from `<jobs_dir>/result.json`: first value of
/// `stats.evals.*.reward_stats.reward`. 1.0/0.0 are the driver currencies;
/// any other value has no verdict in the eval taxonomy, so it reports as
/// missing/unparseable (`ErrorNoReport`). Missing or garbage file likewise.
pub(crate) fn reward_verdict(result: &Path) -> eval::Verdict {
    let no_report = eval::Verdict::ErrorNoReport;
    let reward = std::fs::read_to_string(result)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.pointer("/stats/evals")
                .and_then(|e| e.as_object())
                .and_then(|m| m.values().next())
                .and_then(|ev| ev.pointer("/reward_stats/reward"))
                .and_then(parse_reward)
        });
    match reward {
        Some(r) if (r - 1.0).abs() < 1e-9 => eval::Verdict::Resolved,
        Some(0.0) => eval::Verdict::Unresolved,
        _ => no_report,
    }
}

/// The reward is a scalar in the harbor the pilots measured and a
/// `{value: [trials]}` histogram in current harbor; one trial per job
/// means exactly one key. Anything else fails closed.
fn parse_reward(r: &serde_json::Value) -> Option<f64> {
    if let Some(n) = r.as_f64() {
        return Some(n);
    }
    let m = r.as_object()?;
    if m.len() != 1 {
        return None; // multi-trial histogram: not the one-trial-per-job protocol
    }
    m.keys().next()?.parse().ok()
}

/// Last of the final 4 lines containing "passed" or "failed" (driver
/// parity). Shared by the harbor verifier files and the in-container
/// verifier output.
fn last_passed_failed(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let window: &[&str] = if lines.len() > 4 {
        &lines[lines.len() - 4..]
    } else {
        &lines
    };
    window
        .iter()
        .rev()
        .find(|l| l.contains("passed") || l.contains("failed"))
        .map(|l| l.trim().to_owned())
}

/// Short verifier string for the row: over `<jobs_dir>/*/verifier/
/// test-stdout.txt`, the last of the final 4 lines containing "passed" or
/// "failed" (driver parity).
fn verifier_string(jobs_dir: &Path) -> Option<String> {
    for entry in std::fs::read_dir(jobs_dir).ok().into_iter().flatten() {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().ok().is_some_and(|t| t.is_dir()) {
            continue;
        }
        let p = entry.path().join("verifier").join("test-stdout.txt");
        if !p.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&p).ok()?;
        return last_passed_failed(&text);
    }
    None
}

/// Crash guard (driver parity): never bank an agent-less cell — a dump with
/// no RunEnd row means the agent never finished, so the instance is infra,
/// never a capability failure.
fn has_run_end(events: &Path) -> bool {
    std::fs::read_to_string(events)
        .map(|t| {
            t.lines().filter(|l| !l.trim().is_empty()).any(|l| {
                serde_json::from_str::<AgentEvent>(l)
                    .map(|e| matches!(e, AgentEvent::RunEnd { .. }))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Env container name for one instance: the single handle for the agent
/// exec wrap, the pre-clean, and the verifier (docker exec/rm accept name
/// or id).
fn env_container_name(id: &str) -> String {
    format!("eval-{id}-env")
}

/// Exec-wrap argv for one live env container: prepended at spawn by the
/// tool policy so every exec/test call lands inside the container (still
/// no shell, still argv-exact on the unwrapped command).
fn docker_exec_wrap(container: &str) -> Vec<String> {
    vec!["docker".into(), "exec".into(), container.into()]
}

/// Exact `docker run` argv for one in-container instance: detached, named,
/// workdir bind-mounted at /app, task tests read-only at /tests, /app as
/// the container workdir, cpus/memory capped, and the image entrypoint
/// overridden to `tail -f /dev/null` so a server-entrypoint image does not
/// hijack the run. Pure so tests pin the flag list without docker.
fn env_container_run_args(image: &str, work: &Path, tasks_dir: &Path, id: &str) -> Vec<String> {
    // /logs is the harbor verifier-artifact root: task test.sh scripts
    // write /logs/verifier/{ctrf.json,reward.txt}. Mounted writable at
    // <home>/verifier-logs (work's parent is the instance home) so the
    // artifacts persist on the host; the `verifier` subdir is pre-created
    // by start_env_container (pytest --ctrf does not mkdir).
    let logs = work.parent().unwrap_or(work).join("verifier-logs");
    vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        env_container_name(id),
        "-v".into(),
        format!("{}:/app", work.display()),
        "-v".into(),
        format!("{}:/tests:ro", tasks_dir.join(id).join("tests").display()),
        "-v".into(),
        format!("{}:/logs", logs.display()),
        "-w".into(),
        "/app".into(),
        "--cpus=2".into(),
        "--memory=4096m".into(),
        "--entrypoint".into(),
        "tail".into(),
        image.into(),
        "-f".into(),
        "/dev/null".into(),
    ]
}

/// docker rm -f, best-effort: the stale pre-clean before a run and the
/// cleanup afterwards (every path, including failure).
async fn rm_env_container(id: &str) {
    let _ = tokio::process::Command::new("docker")
        .args(["rm", "-f", &env_container_name(id)])
        .output()
        .await;
}

/// Wipe the workdir inside a throwaway container: in-container runs write as
/// root through the bind mount, and those files outlive a host-side rm.
async fn wipe_workdir_via_docker(image: &str, work: &Path) {
    let target = format!("{}:/w", work.display());
    let _ = tokio::process::Command::new("docker")
        .args([
            "run",
            "--rm",
            "-v",
            &target,
            "--entrypoint",
            "sh",
            image,
            "-c",
            "rm -rf /w/* /w/.[!.]* /w/..?* 2>/dev/null; true",
        ])
        .output()
        .await;
}

/// Start the task environment container BEFORE the agent run: pre-clean a
/// stale container from a crashed attempt, then run detached with the
/// workdir bind-mounted at /app and tests read-only at /tests. Returns the
/// container name; spawn/pull failure is infra (never capability).
async fn start_env_container(
    inst: &eval::Instance,
    work: &Path,
    tasks_dir: &Path,
) -> Result<String, String> {
    rm_env_container(&inst.id).await;
    // The verifier artifact dir: harbor's compose pre-creates /logs/verifier;
    // pytest --ctrf will not mkdir it itself.
    let _ = std::fs::create_dir_all(
        work.parent()
            .unwrap_or(work)
            .join("verifier-logs")
            .join("verifier"),
    );
    // ponytail: cpus/memory chosen to match the task specs observed
    // (cpus=2, memory_mb=4096); upgrade path: parse [environment] from
    // task.toml per task. The entrypoint override keeps task images with
    // server entrypoints from hijacking the run.
    let out = tokio::process::Command::new("docker")
        .args(env_container_run_args(
            &inst.image,
            work,
            tasks_dir,
            &inst.id,
        ))
        .output()
        .await
        .map_err(|e| format!("cannot spawn docker: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "docker run failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(env_container_name(&inst.id))
}

/// Verdict from the verifier exit: 0 is Resolved, nonzero Unresolved; a
/// spawn failure or wall-clock timeout is infra (never capability). Pure so
/// tests pin the mapping.
/// Verdict from the live-container verifier exit. Docker's daemon-error
/// exit (125: "Error response from daemon" — e.g. the container died
/// mid-run), the daemon's error text on stderr, and the verifier-machinery
/// codes (126 not-executable, 127 not-found — the test script never ran)
/// are InfraFailure: the verifier did not execute, so it can never score
/// as capability. Test suites report failure through their own exit
/// codes (pytest 1-5), which stay Unresolved.
fn grade_exit_verdict(status: &std::process::ExitStatus, stderr: &str) -> eval::Verdict {
    if status.success() {
        return eval::Verdict::Resolved;
    }
    if status.code() == Some(125)
        || status.code() == Some(126)
        || status.code() == Some(127)
        || stderr.contains("Error response from daemon")
    {
        return eval::Verdict::InfraFailure;
    }
    eval::Verdict::Unresolved
}

/// Grade one agent run inside the live env container: exec /tests/test.sh
/// in the SAME container the agent worked in, so agent-installed packages
/// and running services persist to grading. Verdict provenance here is the
/// verifier exit code — a NEW estimand, never mixed with the harbor
/// result.json reward. Spawn failure or timeout is infra (never
/// capability).
async fn grade_live_container(
    inst: &eval::Instance,
    container: &str,
    home: &Path,
) -> eval::Verdict {
    // ponytail: 300s slack above the instance timeout is chosen (same slack
    // as the harbor path); upgrade path: a per-task verifier timeout.
    let out = tokio::time::timeout(
        Duration::from_secs(inst.timeout_secs + 300),
        tokio::process::Command::new("docker")
            // bash-invoked: the ro tests mount carries no exec bit, and a
            // missing/unrunnable test.sh is 126/127 — machinery, not the
            // suite failing.
            .args(["exec", container, "bash", "/tests/test.sh"])
            .output(),
    )
    .await;
    let ok: Option<std::process::Output> = match out {
        Err(_) => None,  // wall-clock timeout is infra
        Ok(r) => r.ok(), // spawn failure (no docker) is infra
    };
    let Some(o) = ok else {
        eprintln!(
            "infra {}: docker exec timed out or failed to spawn",
            inst.id
        );
        return eval::Verdict::InfraFailure;
    };
    let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
    let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
    text.push_str(&stderr);
    // Persist the verifier output (harbor's path keeps test-stdout.txt; this
    // is the in-container mode's equivalent) and name the exit code.
    let _ = std::fs::write(home.join("verifier-stdout.txt"), &text);
    eprintln!("{} verifier exit: {:?}", inst.id, o.status.code());
    if let Some(vs) = last_passed_failed(&text) {
        eprintln!("{} verifier: {vs}", inst.id);
    }
    let verdict = grade_exit_verdict(&o.status, &stderr);
    if matches!(verdict, eval::Verdict::InfraFailure) {
        eprintln!(
            "infra {}: docker daemon error at grading (container died?)",
            inst.id
        );
    }
    verdict
}

/// Grade one agent run via harbor: build the grade tree, run
/// `harbor run -c <job> -y -q`, read the reward. Spawn failure, timeout, or
/// a non-zero exit without result.json is infra (never capability).
async fn grade(inst: &eval::Instance, args: &EvalArgs, work: &Path) -> eval::Verdict {
    let home = args.out_dir.join(&inst.id);
    let grade_dir = home.join("grade-task");
    let jobs = args.out_dir.join("jobs").join(format!("eval-{}", inst.id));
    let infra = |e: String| {
        eprintln!("infra {}: {e}", inst.id);
        eval::Verdict::InfraFailure
    };
    // Grade task tree: whole task copy, solution replaced by files/ + solve.sh.
    let _ = std::fs::remove_dir_all(&grade_dir);
    if let Err(e) = copy_tree(&args.tasks_dir.join(&inst.id), &grade_dir) {
        return infra(format!("cannot build {}: {e}", grade_dir.display()));
    }
    let _ = std::fs::remove_dir_all(grade_dir.join("solution"));
    let files = grade_dir.join("solution").join("files");
    if let Err(e) = pack_workdir(work, &files) {
        return infra(format!("cannot pack workdir: {e}"));
    }
    let solve = grade_dir.join("solution").join("solve.sh");
    if let Err(e) = std::fs::write(&solve, SOLVE_SH) {
        return infra(format!("cannot write solve.sh: {e}"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&solve, std::fs::Permissions::from_mode(0o755));
    }
    let job = home.join("grade-job.yaml");
    if let Err(e) = std::fs::write(
        &job,
        serde_json::to_string(&job_json(&inst.id, &grade_dir, &args.out_dir.join("jobs")))
            .expect("job json always serialises"),
    ) {
        return infra(format!("cannot write {}: {e}", job.display()));
    }
    // ponytail: 300s slack above the instance timeout is chosen; upgrade
    // path: let harbor own a per-job timeout instead of our wall clock.
    let out = tokio::time::timeout(
        Duration::from_secs(inst.timeout_secs + 300),
        tokio::process::Command::new("harbor")
            .args(["run", "-c", job.to_string_lossy().as_ref(), "-y", "-q"])
            .current_dir(&args.out_dir)
            .output(),
    )
    .await;
    let result = jobs.join("result.json");
    let ok: Option<std::process::Output> = match out {
        Err(_) => None,  // grading wall-clock timeout is infra
        Ok(r) => r.ok(), // spawn failure (no harbor binary) is infra
    };
    let ran = match ok {
        None => false,
        Some(o) => o.status.success() || result.exists(),
    };
    if !ran {
        eprintln!(
            "infra {}: harbor run timed out or produced no result",
            inst.id
        );
        return eval::Verdict::InfraFailure;
    }
    if let Some(vs) = verifier_string(&jobs) {
        eprintln!("{} verifier: {vs}", inst.id);
    }
    reward_verdict(&result)
}

/// The agent leg: credential gate once, then per instance — fresh seeded
/// workdir, in-process `rof run` wiring, harbor grading, report row. Every
/// per-instance failure is infra and the loop continues; only an
/// un-creatable out-dir or a failed report row aborts the slice (exit 2).
pub(crate) async fn run_agent_leg(
    args: &EvalArgs,
    instances: &[eval::Instance],
    caps: &eval::ToolCaps,
) -> i32 {
    // Credential gate exactly once, before any spend (never per instance).
    let endpoint = match args.endpoint.as_deref() {
        Some(e) => e,
        None => {
            eprintln!("no endpoint: pass --endpoint URL or set OPENAI_BASE_URL");
            return 2;
        }
    };
    let api_key_env = args.api_key_env.as_deref().unwrap_or("OPENAI_API_KEY");
    let mut provider =
        OpenAiCompat::new(endpoint, endpoint_profile(args.keep_thinking)).with_key_env(api_key_env);
    for h in &args.headers {
        if let Some((name, value)) = h.split_once(':') {
            provider = provider.with_header(name, value);
        }
    }
    if let Err(e) = provider.check_credentials() {
        eprintln!(
            "{}",
            missing_credentials_message(api_key_env, &e.to_string())
        );
        return EXIT_NO_CREDENTIALS;
    }
    let mut reports = Vec::new();
    for inst in instances {
        let task_dir = args.tasks_dir.join(&inst.id);
        for bad in eval::check_winnability(&task_dir, caps) {
            eprintln!("unwinnable {}: {bad}", inst.id);
        }
        let home = args.out_dir.join(&inst.id);
        if std::fs::create_dir_all(&home).is_err() {
            eprintln!("cannot create {}", home.display());
            return 2;
        }
        let work = home.join("work");
        // In-container mode: root-owned leftovers from a previous run's
        // container writes (bind-mounted /app) survive a host-side rm;
        // wipe them inside a throwaway container from the task image BEFORE
        // seeding, so reruns start clean. ponytail: the image is local (it
        // is about to be run anyway); images without sh stay dirty — upgrade
        // path: an Alpine helper image.
        if args.in_container && !inst.image.is_empty() {
            wipe_workdir_via_docker(&inst.image, &work).await;
        }
        if let Err(e) = fresh_workdir(&task_dir, &work) {
            eprintln!("infra {}: workdir: {e}", inst.id);
            reports.push(infra_report(
                inst,
                args,
                &home,
                eval::Verdict::InfraFailure,
                0,
            ));
            continue;
        }
        let start = Instant::now();
        let goal = agent_goal(&inst.instruction);
        let events = home.join("events.jsonl");
        let mut run_args = agent_args(args, &work, &goal, &events);
        // A workdir refusal is per-instance infra failure, never capability.
        if validate_workdir(&work, false).is_err() {
            eprintln!("infra {}: workdir refused: {}", inst.id, work.display());
            let wall = start.elapsed().as_secs();
            reports.push(infra_report(
                inst,
                args,
                &home,
                eval::Verdict::InfraFailure,
                wall,
            ));
            continue;
        }
        // In-container mode: the agent works INSIDE the task environment
        // container (started before the run, workdir bind-mounted at /app)
        // and the verifier execs the same live container, so installed
        // packages and running services persist to grading; the harbor
        // grade-task path is replaced entirely in this mode. File tools
        // stay host-side on the bind-mounted workdir, so the deliverable
        // patch still captures file work.
        let container = if args.in_container {
            if inst.image.is_empty() {
                eprintln!("infra {}: no docker_image in task.toml", inst.id);
                let wall = start.elapsed().as_secs();
                reports.push(infra_report(
                    inst,
                    args,
                    &home,
                    eval::Verdict::InfraFailure,
                    wall,
                ));
                continue;
            }
            match start_env_container(inst, &work, &args.tasks_dir).await {
                Ok(name) => {
                    run_args.exec_wrap = Some(docker_exec_wrap(&name));
                    Some(name)
                }
                Err(e) => {
                    eprintln!("infra {}: {e}", inst.id);
                    let wall = start.elapsed().as_secs();
                    reports.push(infra_report(
                        inst,
                        args,
                        &home,
                        eval::Verdict::InfraFailure,
                        wall,
                    ));
                    continue;
                }
            }
        } else {
            None
        };
        // ponytail: execute() spawns a fresh ctrl-c listener per call (one
        // per instance here) — bounded by the slice size; upgrade path: a
        // shared CancellationToken threaded through execute().
        let r = execute(&provider, &run_args).await;
        if let Some(path) = run_args.dump_events.as_deref() {
            if let Err(e) = finalize_dump(path, &r.events) {
                eprintln!("infra {}: cannot write {path}: {e}", inst.id);
                let wall = start.elapsed().as_secs();
                if container.is_some() {
                    rm_env_container(&inst.id).await;
                }
                reports.push(infra_report(
                    inst,
                    args,
                    &home,
                    eval::Verdict::InfraFailure,
                    wall,
                ));
                continue;
            }
        }
        let wall = start.elapsed().as_secs();
        // Failed/Halted/Cancelled runs still get graded (driver parity: the
        // workdir is graded whenever the run ended); only a missing RunEnd
        // is infra.
        let verdict = if !has_run_end(&events) {
            eval::Verdict::InfraFailure
        } else if let Some(name) = &container {
            grade_live_container(inst, name, &home).await
        } else {
            grade(inst, args, &work).await
        };
        // The env container dies on every path, including infra grading.
        if container.is_some() {
            rm_env_container(&inst.id).await;
        }
        let patch = r.patch.unwrap_or_default();
        match eval::instance_report_with_mode(
            &inst.id,
            verdict,
            &events,
            wall,
            &patch,
            apply_mode_for(args),
            None,
        ) {
            Ok(row) => reports.push(row),
            Err(e) => {
                eprintln!("cannot report {}: {e}", inst.id);
                return 2;
            }
        }
    }
    crate::eval_cmd::finish_report(args, reports)
}

/// Zero-facts row for an instance that failed before any agent ran (wall
/// measured regardless; the dump may not exist yet, so a hand-built row
/// stands in when the report reader cannot).
fn infra_report(
    inst: &eval::Instance,
    args: &EvalArgs,
    home: &Path,
    verdict: eval::Verdict,
    wall: u64,
) -> eval::InstanceReport {
    let events = home.join("events.jsonl");
    match eval::instance_report_with_mode(
        &inst.id,
        verdict,
        &events,
        wall,
        "",
        apply_mode_for(args),
        None,
    ) {
        Ok(r) => r,
        Err(_) => eval::InstanceReport {
            id: inst.id.clone(),
            verdict,
            tokens_in: None,
            tokens_out: None,
            dollars: None,
            wall_secs: wall,
            steps: 0,
            halt_reason: None,
            patch_digest: String::new(),
            apply_mode: apply_mode_for(args),
            apply_detail: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::argv;

    fn tmp() -> std::path::PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("rof-eval-agent-{}-{n}", std::process::id()))
    }

    fn tmp_created() -> std::path::PathBuf {
        let p = tmp();
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn goal_maps_app_and_appends_tail_once() {
        let g = agent_goal("Build the server in /app/thing and tidy /app");
        assert_eq!(
            g,
            format!("Build the server in thing and tidy the workdir{GOAL_TAIL}")
        );
        assert_eq!(
            g.matches("Rules: all paths are relative to the workdir")
                .count(),
            1,
            "tail appended exactly once: {g}"
        );
        assert!(g.ends_with(GOAL_TAIL), "tail is the last thing: {g}");
        // stdin clause (26-cell sweep arm marker: banked cells ran the old
        // tail without it).
        assert!(
            g.contains("its stdin is closed, so heredocs and stdin reads return nothing"),
            "{g}"
        );
    }

    #[test]
    fn pack_workdir_excludes_testcache_wal_and_pyc() {
        let root = tmp_created();
        let work = root.join("work");
        for (rel, body) in [
            (".git/config", "g\n"),
            ("__pycache__/m.py", "c\n"),
            (".pytest_cache/v/cache", "c\n"),
            ("comp.pyc", "c\n"),
            (".rof-events.jsonl", "{}\n"),
            ("keep/me.txt", "wanted\n"),
            ("nested/inner/keep.py", "fine\n"),
            ("nested/x.pyc", "c\n"),
            ("nested/.git/HEAD", "h\n"),
            ("nested/__pycache__/m.py", "c\n"),
        ] {
            let p = work.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
        }
        let dst = root.join("files");
        pack_workdir(&work, &dst).unwrap();
        let mut got: Vec<String> = walk(&dst, &dst);
        got.sort();
        assert_eq!(
            got,
            vec![
                "keep/me.txt".to_string(),
                "nested/.git/HEAD".into(),
                "nested/__pycache__/m.py".into(),
                "nested/inner/keep.py".into(),
            ],
            "top-level dirs pruned, .pyc and the WAL sidecar excluded at any depth, \
             nested dirs ride along (driver parity: parts[0] only): {got:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    fn walk(root: &Path, dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let e = entry.unwrap();
            if e.file_type().unwrap().is_dir() {
                out.extend(walk(root, &e.path()));
            } else {
                out.push(
                    e.path()
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
        out
    }

    #[test]
    fn reward_reads_first_evals_entry() {
        let dir = tmp_created();
        let r = |body: &str| {
            let p = dir.join("result.json");
            std::fs::write(&p, body).unwrap();
            reward_verdict(&p)
        };
        assert_eq!(
            r("{\"stats\":{\"evals\":{\"tb\":{\"reward_stats\":{\"reward\":1.0}}}}}"),
            eval::Verdict::Resolved
        );
        assert_eq!(
            r("{\"stats\":{\"evals\":{\"tb\":{\"reward_stats\":{\"reward\":0.0}}}}}"),
            eval::Verdict::Unresolved
        );
        assert_eq!(
            r("{\"stats\":{\"evals\":{\"tb\":{\"reward_stats\":{\"reward\":{\"0.0\":[\"t\"]}}}}}}"),
            eval::Verdict::Unresolved,
            "current harbor: reward is a {{value: [trials]}} histogram"
        );
        assert_eq!(
            r("{\"stats\":{\"evals\":{\"tb\":{\"reward_stats\":{\"reward\":{\"1.0\":[\"t\"]}}}}}}"),
            eval::Verdict::Resolved
        );
        assert_eq!(
            r("{\"stats\":{\"evals\":{\"tb\":{\"reward_stats\":{\"reward\":{\"0.0\":[\"a\"],\"1.0\":[\"b\"]}}}}}}"),
            eval::Verdict::ErrorNoReport,
            "multi-trial histogram is not the one-trial-per-job protocol"
        );
        assert_eq!(
            r("{\"stats\":{\"evals\":{\"tb\":{\"reward_stats\":{\"reward\":{\"maybe\":[\"t\"]}}}}}}"),
            eval::Verdict::ErrorNoReport,
            "non-numeric histogram key"
        );
        assert_eq!(
            r("{\"stats\":{\"evals\":{\"tb\":{\"reward_stats\":{\"reward\":{\"0.5\":[\"t\"]}}}}}}"),
            eval::Verdict::ErrorNoReport,
            "neither 1.0 nor 0.0 has a verdict in the taxonomy"
        );
        assert_eq!(
            r("{\"stats\":{\"evals\":{}}}"),
            eval::Verdict::ErrorNoReport
        );
        assert_eq!(r("{not json"), eval::Verdict::ErrorNoReport);
        std::fs::write(dir.join("missing-never"), "").unwrap();
        assert_eq!(
            reward_verdict(&dir.join("absent.json")),
            eval::Verdict::ErrorNoReport
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn job_json_has_the_driver_shape() {
        let v = job_json(
            "some-task",
            Path::new("/out/some-task/grade-task"),
            Path::new("/out/jobs"),
        );
        assert_eq!(v["job_name"], "eval-some-task");
        assert_eq!(v["jobs_dir"], "/out/jobs");
        assert_eq!(v["tasks"][0]["path"], "/out/some-task/grade-task");
        assert_eq!(v["agents"][0]["name"], "oracle");
        assert!(v.as_object().unwrap().len() == 4, "no extra keys: {v}");
    }

    #[test]
    fn no_run_end_means_infra() {
        let dir = tmp_created();
        let p = dir.join("events.jsonl");
        std::fs::write(
            &p,
            concat!(
                "{\"type\":\"MessageEnd\",\"id\":1,\"message\":{\"role\":\"Assistant\",\"content\":\"\"},\"interrupted\":false,\"usage\":null}\n",
                "{\"type\":\"RunEnd\",\"outcome\":{\"Failed\":\"steps\"},\"messages\":[]}\n",
            ),
        )
        .unwrap();
        assert!(has_run_end(&p));
        std::fs::write(
            &p,
            "{\"type\":\"MessageEnd\",\"id\":1,\"message\":{\"role\":\"Assistant\",\"content\":\"\"},\"interrupted\":false,\"usage\":null}\n",
        )
        .unwrap();
        assert!(!has_run_end(&p), "crash guard: no RunEnd row");
        assert!(!has_run_end(&dir.join("absent.jsonl")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn solve_sh_is_verbatim_and_executable() {
        let root = tmp_created();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("f.txt"), "x\n").unwrap();
        let grade = root.join("grade");
        let files = grade.join("solution").join("files");
        pack_workdir(&work, &files).unwrap();
        let solve = grade.join("solution").join("solve.sh");
        std::fs::write(&solve, SOLVE_SH).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&solve, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(
                std::fs::metadata(&solve).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        assert_eq!(
            std::fs::read_to_string(&solve).unwrap(),
            "#!/bin/bash\nset -euo pipefail\nSCRIPT_DIR=\"$(cd \"$(dirname \"$0\")\" && pwd)\"\ncp -r \"$SCRIPT_DIR/files/.\" /app/\n"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn fresh_workdir_seeds_environment_or_leaves_empty() {
        let root = tmp_created();
        let task = root.join("task");
        std::fs::create_dir_all(task.join("environment").join("sub")).unwrap();
        std::fs::write(task.join("environment").join("a.txt"), "seed\n").unwrap();
        std::fs::write(
            task.join("environment").join("sub").join("b.txt"),
            "seed2\n",
        )
        .unwrap();
        let work = root.join("out").join("t").join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("stale.txt"), "stale\n").unwrap();
        fresh_workdir(&task, &work).unwrap();
        assert_eq!(
            std::fs::read_to_string(work.join("a.txt")).unwrap(),
            "seed\n"
        );
        assert_eq!(
            std::fs::read_to_string(work.join("sub").join("b.txt")).unwrap(),
            "seed2\n"
        );
        assert!(
            !work.join("stale.txt").exists(),
            "fresh run: old work wiped"
        );
        // can_create task: empty environment dir, workdir stays empty.
        std::fs::remove_dir_all(task.join("environment")).unwrap();
        std::fs::create_dir_all(task.join("environment")).unwrap();
        fresh_workdir(&task, &work).unwrap();
        assert_eq!(
            std::fs::read_dir(work).unwrap().count(),
            0,
            "empty environment => empty workdir"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn flags_parse_and_fill_agent_args() {
        let a = crate::eval_cmd::parse_eval(&argv(&[
            "rof",
            "eval",
            "--tasks-dir",
            "/tmp/t",
            "--agent",
            "--model",
            "deepseek-v4.1-flash",
            "--endpoint",
            "https://e/v1",
            "--header",
            "x-opencode-session:r1-a",
            "--allow-cmd",
            "python3",
            "--allow-cmd",
            "cargo test",
            "--budget-steps",
            "17",
            "--budget-actions",
            "19",
            "--budget-tokens",
            "12345",
            "--max-tokens",
            "432",
        ]))
        .unwrap();
        assert!(a.agent);
        assert_eq!(a.model.as_deref(), Some("deepseek-v4.1-flash"));
        assert_eq!(a.endpoint.as_deref(), Some("https://e/v1"));
        assert_eq!(a.headers, vec!["x-opencode-session:r1-a".to_string()]);
        assert_eq!(
            a.allow_cmd,
            vec!["python3".to_string(), "cargo test".to_string()]
        );
        assert_eq!(
            (
                a.budget_steps,
                a.budget_actions,
                a.budget_tokens,
                a.max_tokens
            ),
            (Some(17), Some(19), Some(12345), Some(432))
        );
        // Budget flags land in the run Args and win over the defaults.
        let run = agent_args(
            &a,
            Path::new("/out/id/work"),
            "goal",
            Path::new("/out/id/events.jsonl"),
        );
        assert_eq!(run.model, "deepseek-v4.1-flash");
        assert_eq!(run.endpoint, Some("https://e/v1".into()));
        assert_eq!(run.headers, a.headers);
        assert_eq!(run.allow_cmd, a.allow_cmd);
        assert_eq!(run.budget_steps, Some(17));
        assert_eq!(run.budget_actions, Some(19));
        assert_eq!(run.budget_tokens, Some(12345));
        assert_eq!(run.max_tokens, Some(432));
        assert_eq!(run.workdir, Path::new("/out/id/work"));
        assert_eq!(run.dump_events, Some("/out/id/events.jsonl".into()));
        assert!(run.context_files.is_empty(), "no context pinning in eval");
        assert!(!run.bets && run.proof_cmd.is_none() && run.compaction.is_none());
        assert_eq!(run.incentives, agent_loop::IncentivesLevel::Full);
        assert!(!run.allow_dirty_workdir && run.pass_env.is_empty());
        assert_eq!(run.log_path, None, "WAL defaults inside the workdir");
        // Default api-key-env fills OPENAI_API_KEY.
        assert_eq!(run.api_key_env, "OPENAI_API_KEY");
        // Default path stays host-side: no exec wrap without --in-container.
        assert_eq!(run.exec_wrap, None, "default path: no exec wrap");
    }

    #[test]
    fn omitted_budgets_fill_the_frozen_pilot_recipe() {
        let mut a = crate::eval_cmd::parse_eval(&argv(&[
            "rof",
            "eval",
            "--tasks-dir",
            "/tmp/t",
            "--agent",
            "--model",
            "m",
            "--endpoint",
            "https://e",
        ]))
        .unwrap();
        let run = agent_args(&a, Path::new("/w"), "g", Path::new("/e"));
        assert_eq!(
            (run.budget_tokens, run.budget_steps, run.budget_actions),
            (Some(300_000), Some(60), Some(120)),
            "frozen pilot recipe when the user omits every budget flag"
        );
        // Partial flags: only omitted slots fill.
        a.budget_tokens = Some(42);
        let run = agent_args(&a, Path::new("/w"), "g", Path::new("/e"));
        assert_eq!(run.budget_tokens, Some(42));
        assert_eq!(run.budget_steps, Some(60));
        assert_eq!(run.budget_actions, Some(120));
    }

    #[test]
    fn env_container_run_args_exact_flag_list() {
        let got = env_container_run_args(
            "img:1",
            Path::new("/out/id/work"),
            Path::new("/tasks"),
            "id",
        );
        assert_eq!(
            got,
            [
                "run",
                "-d",
                "--name",
                "eval-id-env",
                "-v",
                "/out/id/work:/app",
                "-v",
                "/tasks/id/tests:/tests:ro",
                "-v",
                "/out/id/verifier-logs:/logs",
                "-w",
                "/app",
                "--cpus=2",
                "--memory=4096m",
                "--entrypoint",
                "tail",
                "img:1",
                "-f",
                "/dev/null",
            ]
        );
    }

    #[test]
    fn docker_exec_wrap_prefixes_the_container() {
        assert_eq!(
            docker_exec_wrap("eval-id-env"),
            vec![
                "docker".to_string(),
                "exec".to_string(),
                "eval-id-env".to_string()
            ]
        );
    }

    #[test]
    fn live_container_verdict_maps_exit_and_infra() {
        #[cfg(unix)]
        fn est(code: i32) -> std::process::ExitStatus {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(code << 8)
        }
        #[cfg(unix)]
        {
            assert_eq!(grade_exit_verdict(&est(0), ""), eval::Verdict::Resolved);
            assert_eq!(
                grade_exit_verdict(&est(1), "1 failed"),
                eval::Verdict::Unresolved
            );
            assert_eq!(
                grade_exit_verdict(&est(125), "Error response from daemon"),
                eval::Verdict::InfraFailure,
                "docker daemon error (container dead) is infra, never capability"
            );
            assert_eq!(
                grade_exit_verdict(&est(2), "Error response from daemon: nope"),
                eval::Verdict::InfraFailure,
                "daemon error text on stderr is infra at any exit code"
            );
            assert_eq!(
                grade_exit_verdict(&est(2), "py exit 2"),
                eval::Verdict::Unresolved,
                "a plain test-suite exit stays a capability row"
            );
            assert_eq!(
                grade_exit_verdict(&est(126), "bash: /tests/test.sh: Permission denied"),
                eval::Verdict::InfraFailure,
                "verifier script never ran (not executable) — infra"
            );
            assert_eq!(
                grade_exit_verdict(&est(127), "bash: /tests/test.sh: No such file"),
                eval::Verdict::InfraFailure,
                "verifier script missing — infra"
            );
        }
        #[cfg(not(unix))]
        {
            use std::process::ExitStatus;
            let ok = std::process::Command::new("true").status().unwrap();
            let bad = std::process::Command::new("false").status().unwrap();
            assert_eq!(grade_exit_verdict(&ok, ""), eval::Verdict::Resolved);
            assert_eq!(grade_exit_verdict(&bad, ""), eval::Verdict::Unresolved);
        }
    }

    #[test]
    fn last_passed_failed_takes_last_of_final_four() {
        let text = "noise\nnoise2\n3 passed; 0 failed\nnoise3\nnoise4\nnoise5";
        assert_eq!(
            last_passed_failed(text).as_deref(),
            Some("3 passed; 0 failed"),
            "driver parity: last of the final 4 lines containing passed/failed"
        );
        assert_eq!(last_passed_failed("no markers at all"), None);
        assert_eq!(last_passed_failed("1 failed"), Some("1 failed".to_string()));
    }
}
