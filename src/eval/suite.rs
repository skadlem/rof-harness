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
