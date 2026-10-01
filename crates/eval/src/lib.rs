//! Thin external-gate runner. See research/bench-landscape.md checklist A–E.
//! Primary: Terminal-Bench 2.1 frozen slice via Harbor protocol. Second:
//! Multi-SWE-bench Rust slice + flash. SWE-bench Verified calibration-only.
//! Outcome taxonomy rule: infra failures are never scored as capability.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Normalized instance across benchmark families.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub instruction: String,
    pub image: String,
    pub oracle: String,
    pub tests: Vec<String>,
    pub timeout_secs: u64,
}

/// Default per-instance timeout (Commit0's 1800s floor, bench-landscape §5).
pub const DEFAULT_TIMEOUT_SECS: u64 = 1800;

fn toml_str(content: &str, keys: &[&str]) -> String {
    for line in content.lines() {
        if let Some((k, v)) = line.split_once('=') {
            if keys.contains(&k.trim()) {
                return v.trim().trim_matches(['"', '\'']).to_string();
            }
        }
    }
    String::new()
}

fn toml_int(content: &str, keys: &[&str]) -> Option<u64> {
    for line in content.lines() {
        if let Some((k, v)) = line.split_once('=') {
            if keys.contains(&k.trim()) {
                let digits: String = v.chars().filter(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = digits.parse() {
                    return Some(n);
                }
            }
        }
    }
    None
}

/// TB adapter: walk tasks/<name>/ (task.toml + instruction.md + tests/).
pub fn load_tb_slice(dir: &Path, ids: &[String]) -> std::io::Result<Vec<Instance>> {
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if ids.is_empty() || ids.iter().any(|id| id == &name) {
                names.push(name);
            }
        }
    }
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let task = dir.join(&name);
            let toml = std::fs::read_to_string(task.join("task.toml")).unwrap_or_default();
            let mut image = toml_str(&toml, &["image"]);
            if image.is_empty() {
                image = toml_str(&toml, &["docker_image"]);
            }
            Ok(Instance {
                id: name.clone(),
                instruction: std::fs::read_to_string(task.join("instruction.md"))
                    .unwrap_or_default(),
                image,
                oracle: std::fs::read_to_string(task.join("solution").join("solve.sh"))
                    .unwrap_or_default(),
                tests: std::fs::read_dir(task.join("tests"))
                    .map(|rd| {
                        let mut files: Vec<String> = rd
                            .filter_map(|e| e.ok())
                            .map(|e| e.file_name().to_string_lossy().into_owned())
                            .collect();
                        files.sort();
                        files
                    })
                    .unwrap_or_default(),
                timeout_secs: toml_int(&toml, &["timeout", "timeout_secs", "time_limit"])
                    .unwrap_or(DEFAULT_TIMEOUT_SECS),
            })
        })
        .collect()
}

/// SWE-family adapter: JSONL rows → FAIL_TO_PASS / PASS_TO_PASS lists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweInstance {
    pub id: String,
    pub repo: String,
    pub fail_to_pass: Vec<String>,
    pub pass_to_pass: Vec<String>,
    pub test_patch: String,
}

fn str_field(v: &serde_json::Value, keys: &[&str]) -> String {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            return s.to_string();
        }
    }
    String::new()
}

fn str_list(v: &serde_json::Value, keys: &[&str]) -> Vec<String> {
    for k in keys {
        if let Some(x) = v.get(*k) {
            if let Some(arr) = x.as_array() {
                return arr
                    .iter()
                    .filter_map(|e| e.as_str().map(|s| s.to_string()))
                    .collect();
            }
            if let Some(s) = x.as_str() {
                let t = s.trim();
                if t.starts_with('[') {
                    let fixed = t.replace('\'', "\"");
                    if let Ok(arr) = serde_json::from_str::<Vec<String>>(&fixed) {
                        return arr;
                    }
                }
                return t
                    .trim_matches(['[', ']'])
                    .split(',')
                    .map(|p| p.trim().trim_matches(['"', '\'']).to_string())
                    .filter(|p| !p.is_empty())
                    .collect();
            }
        }
    }
    Vec::new()
}

fn swe_row(v: &serde_json::Value) -> SweInstance {
    let mut id = str_field(v, &["instance_id", "instance", "id"]);
    if id.is_empty() {
        let (org, repo, num) = (
            str_field(v, &["org"]),
            str_field(v, &["repo"]),
            v.get("number").map(|n| n.to_string()).unwrap_or_default(),
        );
        if !repo.is_empty() {
            id = format!("{org}__{repo}-{num}");
        }
    }
    let mut repo = str_field(v, &["repo"]);
    if repo.is_empty() {
        repo = str_field(v, &["org"]);
    }
    SweInstance {
        id,
        repo,
        fail_to_pass: str_list(v, &["FAIL_TO_PASS", "fail_to_pass"]),
        pass_to_pass: str_list(v, &["PASS_TO_PASS", "pass_to_pass"]),
        test_patch: str_field(v, &["test_patch", "test_patch_text", "fix_patch", "patch"]),
    }
}

pub fn load_swe_slice(jsonl: &Path, ids: &[String]) -> std::io::Result<Vec<SweInstance>> {
    let text = std::fs::read_to_string(jsonl)?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|v| swe_row(&v))
        .filter(|s| ids.is_empty() || ids.iter().any(|id| id == &s.id))
        .collect())
}

/// Container engine seam. Docker first; Modal/Daytona are later backends.
pub trait Engine: Send + Sync {
    fn run_instance(
        &self,
        _instance: &Instance,
        _workdir: &Path,
    ) -> std::io::Result<ContainerOutcome>;
}

#[derive(Debug, Clone)]
pub struct ContainerOutcome {
    pub reward: Option<bool>,
    pub logs: String,
}

/// Docker-first engine over std Command (no new crates). Infra errors
/// surface as `Err` so they are never scored as capability.
#[derive(Debug, Clone, Default)]
pub struct DockerEngine;

impl DockerEngine {
    pub fn new() -> Self {
        Self
    }
}

impl Engine for DockerEngine {
    fn run_instance(
        &self,
        instance: &Instance,
        workdir: &Path,
    ) -> std::io::Result<ContainerOutcome> {
        let out = std::process::Command::new("docker")
            .args([
                "run",
                "--rm",
                "-v",
                &format!("{}:/work", workdir.display()),
                &instance.image,
            ])
            .output()?;
        let mut logs = String::from_utf8_lossy(&out.stdout).into_owned();
        logs.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok(ContainerOutcome { reward: None, logs })
    }
}

/// Oracle checkers: TB state verifier (tests/ + reward file) and SWE patch
/// verifier (git-apply chain with patch fallback, eval.sh, per-repo parser).
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

/// Apply prediction via `git apply`, falling back to `patch --batch
/// --fuzz=5 -p1` (both battle-tested upstream). `Ok(false)` = did not apply.
pub fn apply_patch(workdir: &Path, patch: &str) -> std::io::Result<bool> {
    if patch.trim().is_empty() {
        return Ok(false);
    }
    let mut git = std::process::Command::new("git")
        .args(["apply", "-"])
        .current_dir(workdir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    use std::io::Write;
    git.stdin.take().unwrap().write_all(patch.as_bytes())?;
    if git.wait()?.success() {
        return Ok(true);
    }
    let mut fallback = std::process::Command::new("patch")
        .args(["--batch", "--fuzz=5", "-p1"])
        .current_dir(workdir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    fallback.stdin.take().unwrap().write_all(patch.as_bytes())?;
    Ok(fallback.wait()?.success())
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

/// Per-instance report: capability numbers plus the scaffold-study metrics
/// that are the real objective (tokens-per-solved, no-action turns).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceReport {
    pub id: String,
    pub verdict: Verdict,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub dollars: f64,
    pub wall_secs: u64,
    pub steps: u32,
    pub halt_reason: Option<String>,
    pub patch_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub instances: Vec<InstanceReport>,
}

impl RunReport {
    /// Guardrail metric (≤1.3× budget): `None` when nothing solved.
    pub fn tokens_per_solved(&self) -> Option<f64> {
        let solved = self
            .instances
            .iter()
            .filter(|r| r.verdict == Verdict::Resolved)
            .count();
        if solved == 0 {
            return None;
        }
        let total: u64 = self
            .instances
            .iter()
            .map(|r| r.tokens_in + r.tokens_out)
            .sum();
        Some(total as f64 / solved as f64)
    }
}

/// Mean pass rate with Wilson 95% CI. Never report a bare percentage:
/// 89 tasks means wide intervals.
pub fn wilson_ci(passed: u64, total: u64) -> (f64, f64, f64) {
    if total == 0 {
        return (0.0, 0.0, 0.0);
    }
    let (p, n, z) = (passed as f64 / total as f64, total as f64, 1.96);
    let denom = 1.0 + z * z / n;
    let center = (p + z * z / (2.0 * n)) / denom;
    let delta = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / denom;
    (
        (p).clamp(0.0, 1.0),
        (center - delta).clamp(0.0, 1.0),
        (center + delta).clamp(0.0, 1.0),
    )
}

/// Slice definition: frozen ids + tags living in-repo, reproducible byte-for-byte.
pub fn slice_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("slices")
        .join(format!("{name}.json"))
}

/// Freeze-time winnability: every artifact the verifier reads must be
/// producible by the agent's toolset. $0 check that fails the freeze loud
/// (would have caught layout's missing write tool before any spend).
#[derive(Debug, Clone)]
pub struct ToolCaps {
    pub can_create: bool,
    pub can_patch: bool,
    pub exec_prefixes: Vec<String>,
}

/// Parse `artifacts = [...]` from task.toml: single- or multi-line arrays,
/// `#` comments tolerated. `Err` on malformed input — an empty list must
/// never mean "the parser gave up quietly".
fn task_artifacts(toml: &str) -> Result<Vec<String>, String> {
    let mut buf = String::new();
    let mut open = false;
    let mut done = false;
    for raw in toml.lines() {
        if done {
            break;
        }
        let line = raw.trim();
        let line = match line.find(" #") {
            Some(i) => line[..i].trim_end(),
            None => line,
        };
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if open {
            buf.push(' ');
            buf.push_str(line);
            if line.contains(']') {
                done = true;
            }
        } else if let Some((k, v)) = line.split_once('=') {
            if k.trim() != "artifacts" {
                continue;
            }
            buf.push_str(v.trim());
            if buf.contains(']') {
                done = true;
            } else {
                open = true;
            }
        }
    }
    if open && !done {
        return Err("task.toml: artifacts array never closed with ']'".into());
    }
    if buf.is_empty() {
        return Ok(Vec::new());
    }
    let end = buf.find(']').map(|i| i + 1).unwrap_or(buf.len());
    let mut out = Vec::new();
    for e in buf[..end].trim().trim_matches(['[', ']']).split(',') {
        let e = e.trim();
        if e.is_empty() {
            continue;
        }
        let inner = e
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| e.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .ok_or_else(|| format!("task.toml: unparseable artifacts entry {e:?}"))?;
        if inner.contains('"') || inner.contains('\'') {
            return Err(format!("task.toml: unparseable artifacts entry {e:?}"));
        }
        out.push(inner.to_string());
    }
    Ok(out)
}

/// Returns violation strings; empty means the task is winnable with `caps`.
/// Agent seed = `task_dir/environment` (starting files); `/app/X` maps there.
/// Absent artifacts need create; present ones need patch; missing tests,
/// oracle, or an unreadable/malformed task.toml are always violations —
/// no task.toml can never mean winnable.
pub fn check_winnability(task_dir: &Path, caps: &ToolCaps) -> Vec<String> {
    let mut bad = Vec::new();
    match std::fs::read_to_string(task_dir.join("task.toml")) {
        Err(e) => bad.push(format!("task.toml missing or unreadable: {e}")),
        Ok(toml) => match task_artifacts(&toml) {
            Err(e) => bad.push(e),
            Ok(artifacts) => {
                for a in artifacts {
                    let rel = a.strip_prefix("/app/").unwrap_or(&a);
                    let seed = task_dir.join("environment").join(rel.trim_end_matches('/'));
                    let present = seed.exists();
                    if present && !caps.can_patch {
                        bad.push(format!("{a}: present in seed but no patch tool"));
                    }
                    if !present && !caps.can_create {
                        bad.push(format!("{a}: absent from seed and no write tool"));
                    }
                }
            }
        },
    }
    let tests_empty = std::fs::read_dir(task_dir.join("tests"))
        .map(|rd| rd.count() == 0)
        .unwrap_or(true);
    if tests_empty {
        bad.push("no verifier tests".to_string());
    }
    if !task_dir.join("solution").join("solve.sh").exists() {
        bad.push("no oracle solve.sh".to_string());
    }
    bad
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("eval-test-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn tb_task(root: &Path, name: &str, image: &str, timeout: u64) {
        let t = root.join(name);
        std::fs::create_dir_all(t.join("solution")).unwrap();
        std::fs::create_dir_all(t.join("tests")).unwrap();
        std::fs::write(
            t.join("task.toml"),
            format!("image = \"{image}\"\ntimeout = {timeout}\n"),
        )
        .unwrap();
        std::fs::write(t.join("instruction.md"), format!("# {name}\nDo it.\n")).unwrap();
        std::fs::write(t.join("solution").join("solve.sh"), "#!/bin/sh\necho ok\n").unwrap();
        std::fs::write(t.join("tests").join("test.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    }

    fn inst(id: &str, oracle: &str, tests: Vec<String>) -> Instance {
        Instance {
            id: id.into(),
            instruction: "x".into(),
            image: "img".into(),
            oracle: oracle.into(),
            tests,
            timeout_secs: 60,
        }
    }

    #[test]
    fn tb_adapter_walks_fixture_tasks_dir() {
        let root = tmp();
        tb_task(&root, "aaa", "img-a", 300);
        tb_task(&root, "bbb", "img-b", 600);
        let all = load_tb_slice(&root, &[]).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "aaa"); // sorted
        assert!(all[0].instruction.contains("aaa"));
        assert_eq!(all[0].image, "img-a");
        assert!(all[0].oracle.contains("echo ok"));
        assert_eq!(all[0].tests, vec!["test.sh".to_string()]);
        assert_eq!(all[0].timeout_secs, 300);
        let one = load_tb_slice(&root, &["bbb".to_string()]).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].image, "img-b");
    }

    #[test]
    fn swe_adapter_reads_both_families() {
        let root = tmp();
        let f = root.join("instances.jsonl");
        std::fs::write(
            &f,
            "{\"instance_id\":\"django__django-11099\",\"repo\":\"django\",\"FAIL_TO_PASS\":[\"t1\",\"t2\"],\"PASS_TO_PASS\":[\"t3\"],\"test_patch\":\"p\"}\n\
             {\"org\":\"acme\",\"repo\":\"lib\",\"number\":7,\"FAIL_TO_PASS\":\"['a']\",\"PASS_TO_PASS\":[],\"fix_patch\":\"q\"}\n",
        )
        .unwrap();
        let all = load_swe_slice(&f, &[]).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "django__django-11099");
        assert_eq!(
            all[0].fail_to_pass,
            vec!["t1".to_string(), "t2".to_string()]
        );
        assert_eq!(all[0].pass_to_pass, vec!["t3".to_string()]);
        assert_eq!(all[0].test_patch, "p");
        assert_eq!(all[1].id, "acme__lib-7"); // Multi-SWE org/repo/number keying
        assert_eq!(all[1].fail_to_pass, vec!["a".to_string()]);
        let one = load_swe_slice(&f, &["acme__lib-7".to_string()]).unwrap();
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn swe_verdict_needs_all_f2p_and_no_p2p_regress() {
        let f2p = vec!["t1".to_string(), "t2".to_string()];
        let p2p = vec!["t3".to_string()];
        assert_eq!(
            check_swe_results(&f2p, &p2p, "PASS t1\nPASS t2\nPASS t3\n"),
            Verdict::Resolved
        );
        assert_eq!(
            check_swe_results(&f2p, &p2p, "PASS t1\nFAIL t2\nPASS t3\n"),
            Verdict::Unresolved
        );
        assert_eq!(
            check_swe_results(&f2p, &p2p, "PASS t1\nPASS t2\nFAIL t3\n"),
            Verdict::Unresolved
        );
    }

    #[test]
    fn taxonomy_infra_is_never_capability() {
        assert_eq!(
            check_swe_results(&["t".to_string()], &[], "INFRA: container failed"),
            Verdict::InfraFailure
        );
        assert_eq!(
            check_swe_results(&["t".to_string()], &[], ""),
            Verdict::ErrorNoReport
        );
        assert_eq!(
            check_swe_results(&["t".to_string()], &[], "EMPTY_PATCH"),
            Verdict::EmptyPatch
        );
        assert_eq!(
            check_swe_results(&["t".to_string()], &[], "AMBIGUOUS output"),
            Verdict::Ambiguous
        );
        assert_eq!(check_swe_results(&[], &[], "PASS t\n"), Verdict::Ambiguous);
        for v in [
            Verdict::InfraFailure,
            Verdict::ErrorNoReport,
            Verdict::EmptyPatch,
            Verdict::Ambiguous,
        ] {
            assert_ne!(v, Verdict::Resolved); // infra/errors never score as capability
            assert_ne!(v, Verdict::Unresolved);
        }
    }

    struct FakeEngine(ContainerOutcome);
    impl Engine for FakeEngine {
        fn run_instance(&self, _i: &Instance, _w: &Path) -> std::io::Result<ContainerOutcome> {
            Ok(ContainerOutcome {
                reward: self.0.reward,
                logs: self.0.logs.clone(),
            })
        }
    }

    #[test]
    fn engine_trait_fake_needs_no_docker() {
        let e = FakeEngine(ContainerOutcome {
            reward: Some(true),
            logs: "ok".into(),
        });
        let out = e
            .run_instance(&inst("i", "o", vec!["t".into()]), Path::new("/tmp"))
            .unwrap();
        assert_eq!(out.reward, Some(true));
    }

    #[test]
    fn preflight_excludes_broken_before_spend() {
        let good = inst("good", "solve", vec!["test.sh".into()]);
        let bad_oracle = inst("bad-oracle", "", vec!["test.sh".into()]);
        let bad_tests = inst("bad-tests", "solve", vec![]);
        let excluded = preflight(&[good, bad_oracle, bad_tests]);
        assert_eq!(
            excluded,
            vec!["bad-oracle".to_string(), "bad-tests".to_string()]
        );
    }

    #[test]
    fn reward_file_txt_and_json() {
        let root = tmp();
        let txt = root.join("reward.txt");
        std::fs::write(&txt, "1\n").unwrap();
        assert_eq!(check_tb_reward(&txt).unwrap(), Verdict::Resolved);
        std::fs::write(&txt, "0\n").unwrap();
        assert_eq!(check_tb_reward(&txt).unwrap(), Verdict::Unresolved);
        let js = root.join("reward.json");
        std::fs::write(&js, "{\"reward\": 1}").unwrap();
        assert_eq!(check_tb_reward(&js).unwrap(), Verdict::Resolved);
        std::fs::write(&js, "{\"reward\": \"INFRA\"}").unwrap();
        assert_eq!(check_tb_reward(&js).unwrap(), Verdict::InfraFailure);
    }

    #[test]
    fn wilson_math_on_known_values() {
        assert_eq!(wilson_ci(0, 0), (0.0, 0.0, 0.0));
        let (p, lo, hi) = wilson_ci(60, 100);
        assert!((p - 0.6).abs() < 1e-12);
        assert!((lo - 0.502).abs() < 0.005 && (hi - 0.691).abs() < 0.005);
        assert!(lo < p && p < hi); // never a bare percentage
        let (_, lo89, hi89) = wilson_ci(45, 89);
        assert!(hi89 - lo89 > 0.15); // 89 tasks => wide intervals
    }

    #[test]
    fn tokens_per_solved_guards_division() {
        let rep = |id: &str, v: Verdict, t: u64| InstanceReport {
            id: id.into(),
            verdict: v,
            tokens_in: t,
            tokens_out: 0,
            dollars: 0.0,
            wall_secs: 0,
            steps: 0,
            halt_reason: None,
            patch_digest: String::new(),
        };
        let empty = RunReport { instances: vec![] };
        assert_eq!(empty.tokens_per_solved(), None);
        let r = RunReport {
            instances: vec![
                rep("a", Verdict::Resolved, 100),
                rep("b", Verdict::Unresolved, 100),
            ],
        };
        assert_eq!(r.tokens_per_solved(), Some(200.0));
    }

    #[test]
    fn slice_ids_live_in_repo() {
        let p = slice_path("tb-slice-a");
        assert_eq!(p.file_name().unwrap(), "tb-slice-a.json");
        let raw = std::fs::read_to_string(&p).expect("frozen slice file must exist");
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(v.get("ids").and_then(|x| x.as_array()).is_some());
    }

    fn win_task(root: &Path, artifacts: &str, seed_files: &[&str]) -> PathBuf {
        let t = root.join("task");
        std::fs::create_dir_all(t.join("tests")).unwrap();
        std::fs::create_dir_all(t.join("solution")).unwrap();
        std::fs::write(t.join("tests").join("test.sh"), "exit 0\n").unwrap();
        std::fs::write(t.join("solution").join("solve.sh"), "echo ok\n").unwrap();
        std::fs::write(t.join("task.toml"), format!("artifacts = [{artifacts}]\n")).unwrap();
        for f in seed_files {
            let p = t.join("environment").join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "seed\n").unwrap();
        }
        t
    }

    fn full_caps() -> ToolCaps {
        ToolCaps {
            can_create: true,
            can_patch: true,
            exec_prefixes: vec!["python3".into()],
        }
    }

    #[test]
    fn winnability_pass_and_missing_write() {
        let root = tmp();
        // Present artifact needs patch; absent needs create.
        let t = win_task(
            &root,
            "\"/app/data\", \"/app/output/config.json\"",
            &["data/x"],
        );
        assert!(check_winnability(&t, &full_caps()).is_empty());
        let no_write = ToolCaps {
            can_create: false,
            ..full_caps()
        };
        let bad = check_winnability(&t, &no_write);
        assert_eq!(bad.len(), 1);
        assert!(bad[0].contains("no write tool"), "{bad:?}");
    }

    #[test]
    fn winnability_missing_tests_or_oracle() {
        let root = tmp();
        let t = win_task(&root, "\"/app/data\"", &["data/x"]);
        std::fs::remove_dir_all(t.join("tests")).unwrap();
        let bad = check_winnability(&t, &full_caps());
        assert!(
            bad.iter().any(|b| b.contains("no verifier tests")),
            "{bad:?}"
        );
        assert!(!bad.iter().any(|b| b.contains("no oracle")), "{bad:?}");

        // missing-oracle branch: tests restored, solve.sh gone.
        std::fs::create_dir_all(t.join("tests")).unwrap();
        std::fs::write(t.join("tests").join("test.sh"), "exit 0\n").unwrap();
        std::fs::remove_file(t.join("solution").join("solve.sh")).unwrap();
        let bad = check_winnability(&t, &full_caps());
        assert!(
            bad.iter().any(|b| b.contains("no oracle solve.sh")),
            "{bad:?}"
        );
        assert!(
            !bad.iter().any(|b| b.contains("no verifier tests")),
            "{bad:?}"
        );
    }

    #[test]
    fn artifacts_multiline_array_parses() {
        let root = tmp();
        let t = win_task(&root, "\"/app/data\"", &["data/x"]);
        std::fs::write(
            t.join("task.toml"),
            "image = \"img\"\nartifacts = [\n  \"/app/data\",\n  \"/app/output\",\n]\n",
        )
        .unwrap();
        assert!(check_winnability(&t, &full_caps()).is_empty());
        let no_write = ToolCaps {
            can_create: false,
            ..full_caps()
        };
        let bad = check_winnability(&t, &no_write);
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(bad[0].contains("no write tool"), "{bad:?}");
    }

    #[test]
    fn artifacts_comment_lines_tolerated() {
        let toml = "# task metadata\nimage = \"img\"\n\nartifacts = [\n  # what the verifier reads\n  \"/app/data\", # seed present\n]\ntimeout = 60\n";
        assert_eq!(task_artifacts(toml).unwrap(), vec!["/app/data".to_string()]);
        assert!(task_artifacts("# nothing here\n").unwrap().is_empty());
    }

    #[test]
    fn artifacts_malformed_is_loud_error() {
        // unquoted entry
        assert!(task_artifacts("artifacts = [\n  /app/data,\n]").is_err());
        // array never closed
        assert!(task_artifacts("artifacts = [\"/app/data\"\n").is_err());
        // through check_winnability: never an empty ("winnable") pass
        let root = tmp();
        let t = win_task(&root, "\"/app/data\"", &["data/x"]);
        std::fs::write(t.join("task.toml"), "artifacts = [/app/data]\n").unwrap();
        let bad = check_winnability(&t, &full_caps());
        assert!(bad.iter().any(|b| b.contains("unparseable")), "{bad:?}");
    }

    #[test]
    fn winnability_missing_task_toml_is_violation() {
        let root = tmp();
        let t = win_task(&root, "\"/app/data\"", &["data/x"]);
        std::fs::remove_file(t.join("task.toml")).unwrap();
        let bad = check_winnability(&t, &full_caps());
        assert!(bad.iter().any(|b| b.contains("task.toml")), "{bad:?}");
    }

    #[test]
    fn frozen_slices_pinned_by_digest_and_ids() {
        // Re-freezing either slice must break `cargo test` before any run.
        for (name, sha, ids) in [
            (
                "tb-slice-a",
                "a460a97aff950f2dc1041fd2207b6f5751c73bd207df7b361bf190ae3dde3525",
                2usize,
            ),
            (
                "multiswe-rust",
                "badd9685eb31cd5675465e29294c321e168a1678321613ed028b30fbadd07b36",
                50usize,
            ),
        ] {
            let p = slice_path(name); // CARGO_MANIFEST_DIR/slices/<name>.json
            let out = std::process::Command::new("sha256sum")
                .arg(&p)
                .output()
                .expect("sha256sum must run");
            let got = String::from_utf8(out.stdout).unwrap();
            assert!(got.starts_with(sha), "{name}: slice re-frozen? got {got}");
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            assert_eq!(v["ids"].as_array().unwrap().len(), ids, "{name} ids");
        }
    }

    #[ignore]
    #[test]
    fn winnability_frozen_tasks_live() {
        // Env-gated like live_smoke: ROF_TB_TASKS or default share path; skip if absent.
        let base = std::env::var("ROF_TB_TASKS").unwrap_or_else(|_| {
            format!(
                "{}/.local/share/rof-tb/tasks",
                std::env::var("HOME").unwrap_or_default()
            )
        });
        let base = PathBuf::from(base);
        if !base.is_dir() {
            return;
        }
        let mut checked = 0;
        for entry in std::fs::read_dir(&base).unwrap().filter_map(|e| e.ok()) {
            if !entry.file_type().map(|f| f.is_dir()).unwrap_or(false) {
                continue;
            }
            let bad = check_winnability(&entry.path(), &full_caps());
            assert!(bad.is_empty(), "{:?}: {bad:?}", entry.file_name());
            checked += 1;
        }
        assert!(checked > 0, "no task dirs under {}", base.display());
    }
}
