use serde::{Deserialize, Serialize};

/// One eval task: run the full loop on `goal`, expect this verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalTask {
    pub name: String,
    pub goal: String,
    #[serde(default = "pass")]
    pub expect_pass: bool,
    /// Allowlisted commands run as acceptance evidence (needs matching
    /// entries in permissions.allowed_commands).
    #[serde(default)]
    pub checks: Vec<String>,
    /// Fail-to-pass oracle: each must fail on the pristine copy and pass after.
    #[serde(default)]
    pub fail_to_pass: Vec<String>,
    /// Pass-to-pass oracle: each must pass on the pristine copy and still pass after.
    #[serde(default)]
    pub pass_to_pass: Vec<String>,
    /// Default true: a task that changes nothing cannot pass on prose alone.
    #[serde(default = "expects_writes")]
    pub expect_writes: bool,
    /// Per-task token ceiling override (None = config default, 0 = unlimited).
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

fn expects_writes() -> bool {
    true
}

fn pass() -> bool {
    true
}

/// Pure oracle fold: F2P entries need baseline-fail + final-pass, P2P entries
/// need pass on both sides, matched by exact command string. A declared check
/// with no final entry fails (unproven is not passed). Empty lists are
/// vacuously ok, so tasks that never heard of the split score exactly as before.
pub fn oracle_ok(
    baseline: &[crate::engine::CheckResult],
    finals: &[crate::engine::CheckResult],
    fail_to_pass: &[String],
    pass_to_pass: &[String],
) -> bool {
    let outcome = |name: &str, list: &[crate::engine::CheckResult]| {
        list.iter().find(|c| c.name == name).map(|c| c.passed)
    };
    for name in fail_to_pass {
        if outcome(name, baseline) != Some(false) || outcome(name, finals) != Some(true) {
            return false;
        }
    }
    for name in pass_to_pass {
        if outcome(name, baseline) != Some(true) || outcome(name, finals) != Some(true) {
            return false;
        }
    }
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSuite {
    pub name: String,
    #[serde(default)]
    pub tasks: Vec<EvalTask>,
}

impl EvalSuite {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }
}

#[cfg(test)]
mod tests {
    use super::{oracle_ok, EvalTask};
    use crate::engine::CheckResult;

    fn cr(name: &str, passed: bool) -> CheckResult {
        CheckResult {
            name: name.to_string(),
            passed,
            output: String::new(),
        }
    }

    #[test]
    fn f2p_needs_a_baseline_fail_and_a_final_pass() {
        let base = vec![cr("cargo test foo", false)];
        let fin = vec![cr("cargo test foo", true)];
        assert!(oracle_ok(&base, &fin, &["cargo test foo".to_string()], &[]));
        assert!(!oracle_ok(&fin, &fin, &["cargo test foo".to_string()], &[]));
    }

    #[test]
    fn p2p_needs_pass_on_both_sides() {
        let base = vec![cr("cargo test bar", true)];
        let fin = vec![cr("cargo test bar", true)];
        assert!(oracle_ok(&base, &fin, &[], &["cargo test bar".to_string()]));
        assert!(!oracle_ok(
            &base,
            &[cr("cargo test bar", false)],
            &[],
            &["cargo test bar".to_string()]
        ));
    }

    #[test]
    fn a_missing_final_entry_fails_the_oracle() {
        let base = vec![cr("cargo test foo", false)];
        assert!(!oracle_ok(&base, &[], &["cargo test foo".to_string()], &[]));
    }

    #[test]
    fn no_oracle_fields_is_vacuously_ok() {
        assert!(oracle_ok(&[], &[], &[], &[]));
    }

    #[test]
    fn new_task_fields_default_empty() {
        let t: EvalTask = serde_json::from_str(r#"{"name":"x","goal":"y"}"#).unwrap();
        assert!(t.fail_to_pass.is_empty() && t.pass_to_pass.is_empty());
    }
}
