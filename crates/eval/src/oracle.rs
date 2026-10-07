use crate::slices::Instance;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Oracle checkers: TB state verifier (tests/ + reward file) and SWE patch
/// verifier (apply mode + eval.sh, per-repo parser).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Resolved,
    Unresolved,
    ErrorNoReport,
    EmptyPatch,
    InfraFailure,
    Ambiguous,
}

/// Log markers for taxonomy that the minimal `(lists, log)` seam can see.
fn taxonomy_mark(log: &str) -> Option<Verdict> {
    let up = log.to_uppercase();
    if up.contains("INFRA") || up.contains("CONTAINER FAILED") {
        Some(Verdict::InfraFailure)
    } else if log.trim().is_empty() {
        Some(Verdict::ErrorNoReport)
    } else if up.contains("EMPTY_PATCH") || up.contains("EMPTY PATCH") {
        Some(Verdict::EmptyPatch)
    } else if up.contains("AMBIGUOUS") {
        Some(Verdict::Ambiguous)
    } else {
        None
    }
}

/// Per-repo parser seam: `PASS <test>` / `FAIL <test>` lines. `FAIL` wins.
pub fn swe_test_status(log: &str, test: &str) -> Option<bool> {
    let (mut pass, mut fail) = (false, false);
    for line in log.lines() {
        let line = line.trim();
        if line == format!("FAIL {test}") {
            fail = true;
        } else if line == format!("PASS {test}") {
            pass = true;
        }
    }
    if fail {
        Some(false)
    } else if pass {
        Some(true)
    } else {
        None
    }
}

pub fn check_swe_results(fail_to_pass: &[String], pass_to_pass: &[String], log: &str) -> Verdict {
    if let Some(v) = taxonomy_mark(log) {
        return v;
    }
    if fail_to_pass.is_empty() {
        return Verdict::Ambiguous;
    }
    let f2p_ok = fail_to_pass
        .iter()
        .all(|t| swe_test_status(log, t) == Some(true));
    let p2p_ok = pass_to_pass
        .iter()
        .all(|t| swe_test_status(log, t) != Some(false));
    if f2p_ok && p2p_ok {
        Verdict::Resolved
    } else {
        Verdict::Unresolved
    }
}

/// How strictly a patch must match to count as applied. Strict (the
/// default) is `git apply` only; Lenient keeps the old `patch --fuzz=5`
/// fallback. Strict grades are not comparable with earlier fuzz-lenient
/// benchmark numbers: the same patch can grade differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ApplyMode {
    #[default]
    Strict,
    Lenient,
}

/// What the applier did: whether the patch landed, which mode ran, and
/// the applier's stderr (empty when the applier was silent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyOutcome {
    pub applied: bool,
    pub mode: ApplyMode,
    pub detail: String,
}

// ponytail: one spawn helper for both appliers; stdin is closed before
// wait so a fast-exiting child can never deadlock the write.
fn run_applier(
    program: &str,
    args: &[&str],
    workdir: &Path,
    patch: &str,
) -> std::io::Result<std::process::Output> {
    use std::io::Write as _;
    let mut child = std::process::Command::new(program)
        .args(args)
        .current_dir(workdir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        // A fast failure (bad patch) can close the pipe first; the exit
        // status below already reports that, so never bubble EPIPE here.
        let _ = stdin.write_all(patch.as_bytes());
    }
    child.wait_with_output()
}

/// Apply prediction via `git apply` (Strict) or `git apply` with a
/// `patch --batch --fuzz=5 -p1` fallback (Lenient). `Ok` with
/// `applied == false` = did not apply; only spawn/wait failures are `Err`.
pub fn apply_patch_with_mode(
    workdir: &Path,
    patch: &str,
    mode: ApplyMode,
) -> std::io::Result<ApplyOutcome> {
    if patch.trim().is_empty() {
        return Ok(ApplyOutcome {
            applied: false,
            mode,
            detail: "empty patch".into(),
        });
    }
    let git = run_applier("git", &["apply", "-"], workdir, patch)?;
    if git.status.success() {
        return Ok(ApplyOutcome {
            applied: true,
            mode,
            detail: String::new(),
        });
    }
    let git_stderr = String::from_utf8_lossy(&git.stderr).into_owned();
    if mode == ApplyMode::Strict {
        return Ok(ApplyOutcome {
            applied: false,
            mode,
            detail: git_stderr,
        });
    }
    let fallback = run_applier("patch", &["--batch", "--fuzz=5", "-p1"], workdir, patch)?;
    if fallback.status.success() {
        return Ok(ApplyOutcome {
            applied: true,
            mode,
            detail: git_stderr,
        });
    }
    let mut detail = git_stderr;
    let fallback_stderr = String::from_utf8_lossy(&fallback.stderr).into_owned();
    if !fallback_stderr.is_empty() {
        if !detail.is_empty() {
            detail.push('\n');
        }
        detail.push_str(&fallback_stderr);
    }
    Ok(ApplyOutcome {
        applied: false,
        mode,
        detail,
    })
}

/// Strict-only shorthand (`git apply`, no fuzz fallback); the default mode.
pub fn apply_patch(workdir: &Path, patch: &str) -> std::io::Result<bool> {
    apply_patch_with_mode(workdir, patch, ApplyMode::Strict).map(|o| o.applied)
}

/// Run the repo test command; output goes through [`swe_test_status`].
pub fn run_eval_sh(workdir: &Path) -> std::io::Result<String> {
    let out = std::process::Command::new("sh")
        .arg("eval.sh")
        .current_dir(workdir)
        .output()?;
    let mut log = String::from_utf8_lossy(&out.stdout).into_owned();
    log.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(log)
}

fn reward_json(v: &serde_json::Value) -> Option<Verdict> {
    for k in ["reward", "passed", "pass"] {
        match v.get(k) {
            Some(serde_json::Value::Bool(true)) => return Some(Verdict::Resolved),
            Some(serde_json::Value::Bool(false)) => return Some(Verdict::Unresolved),
            Some(serde_json::Value::Number(n)) => {
                return Some(if n.as_f64().unwrap_or(0.0) > 0.0 {
                    Verdict::Resolved
                } else {
                    Verdict::Unresolved
                })
            }
            Some(serde_json::Value::String(s)) if s.eq_ignore_ascii_case("pass") || s == "1" => {
                return Some(Verdict::Resolved)
            }
            Some(serde_json::Value::String(s)) if s.eq_ignore_ascii_case("fail") || s == "0" => {
                return Some(Verdict::Unresolved)
            }
            _ => {}
        }
    }
    None
}

pub fn check_tb_reward(reward_file: &Path) -> std::io::Result<Verdict> {
    let raw = std::fs::read_to_string(reward_file)?;
    if let Some(v) = taxonomy_mark(&raw) {
        return Ok(v);
    }
    let t = raw.trim();
    if t.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
            if let Some(verdict) = reward_json(&v) {
                return Ok(verdict);
            }
        }
        return Ok(Verdict::Ambiguous);
    }
    Ok(match t.to_ascii_lowercase().as_str() {
        "1" | "pass" | "passed" | "true" | "resolved" => Verdict::Resolved,
        "0" | "fail" | "failed" | "false" | "unresolved" => Verdict::Unresolved,
        _ => Verdict::Ambiguous,
    })
}

/// Gold-patch pre-flight: instances whose oracle fails are excluded loudly
/// before any agent spend.
pub fn preflight(instances: &[Instance]) -> Vec<String> {
    instances
        .iter()
        .filter(|i| {
            i.oracle.trim().is_empty()
                || i.tests.is_empty()
                || i.image.trim().is_empty()
                || i.timeout_secs == 0
        })
        .map(|i| {
            eprintln!("pre-flight: excluding {} (broken oracle/tests/image)", i.id);
            i.id.clone()
        })
        .collect()
}
