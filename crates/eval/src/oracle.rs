use crate::slices::Instance;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Oracle checkers: TB state verifier (tests/ + reward file).
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
