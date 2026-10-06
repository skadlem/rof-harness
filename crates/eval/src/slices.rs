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

pub(crate) fn toml_int(content: &str, keys: &[&str]) -> Option<u64> {
    for line in content.lines() {
        if let Some((k, v)) = line.split_once('=') {
            if keys.contains(&k.trim()) {
                let token = v.trim().trim_matches(['"', '\'']).trim();
                if let Ok(n) = token.parse() {
                    return Some(n);
                }
                if let Ok(f) = token.parse::<f64>() {
                    if f.is_finite() && f >= 0.0 {
                        return Some(f as u64);
                    }
                }
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
pub(crate) fn task_artifacts(toml: &str) -> Result<Vec<String>, String> {
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
