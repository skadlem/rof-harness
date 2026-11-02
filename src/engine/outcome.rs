//! Typed engine/evaluation boundary. Model artifacts remain JSON; verdicts,
//! checks and execution failures cannot silently default during handoff.
use super::CheckResult;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutcome {
    pub task: String,
    pub passed: bool,
    pub rounds: u32,
    pub feedback: String,
    pub check_results: Vec<CheckResult>,
    #[serde(flatten)]
    pub details: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunOutcome {
    pub passed: bool,
    pub rounds: u32,
    pub tasks: Vec<TaskOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_error: Option<String>,
    #[serde(flatten)]
    pub details: Map<String, Value>,
}

impl RunOutcome {
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            passed: false,
            rounds: 0,
            tasks: Vec::new(),
            error: Some(error.into()),
            recording_error: None,
            details: Map::new(),
        }
    }

    pub(crate) fn decode(value: Value) -> Self {
        if value.get("tasks").is_none() {
            if let Some(error) = value.get("error").and_then(Value::as_str) {
                return Self::failed(error);
            }
        }
        match serde_json::from_value::<Self>(value) {
            Ok(mut result) => {
                if result.error.is_some() {
                    result.passed = false;
                }
                result
            }
            Err(error) => Self::failed(format!("invalid engine outcome: {error}")),
        }
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("engine outcomes contain only serializable values")
    }
}
