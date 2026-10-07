//! Test-only shared fixtures: argv helper, scripted provider, response
//! builders, and tempdir helpers for the `execute` tests.

use std::collections::VecDeque;
use std::path::PathBuf;

use provider_core::{AssistantMessage, LlmClient, Response, StopReason, ToolCallRef, Usage};

use crate::cli::Args;

pub(crate) fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|s| (*s).to_string()).collect()
}

pub(crate) struct ScriptClient {
    queue: std::sync::Mutex<VecDeque<Response>>,
    /// Mid-run probe: when set, the first `complete` records whether the
    /// dump file already holds JSON lines — proving the dump is
    /// incremental, not a post-run write.
    pub probe_dump: Option<PathBuf>,
    pub probe_hit: std::sync::Mutex<bool>,
}

impl ScriptClient {
    pub(crate) fn new(resps: Vec<Response>) -> Self {
        Self {
            queue: std::sync::Mutex::new(resps.into()),
            probe_dump: None,
            probe_hit: std::sync::Mutex::new(false),
        }
    }

    pub(crate) fn with_dump_probe(resps: Vec<Response>, dump: PathBuf) -> Self {
        Self {
            queue: std::sync::Mutex::new(resps.into()),
            probe_dump: Some(dump),
            probe_hit: std::sync::Mutex::new(false),
        }
    }
}

#[async_trait::async_trait]
impl LlmClient for ScriptClient {
    async fn complete(
        &self,
        _model: &str,
        _req: &provider_core::Request,
    ) -> Result<Response, provider_core::LlmError> {
        if let Some(dump) = &self.probe_dump {
            if let Ok(text) = std::fs::read_to_string(dump) {
                if text.lines().count() >= 1
                    && text
                        .lines()
                        .all(|l| serde_json::from_str::<serde_json::Value>(l).is_ok())
                {
                    *self.probe_hit.lock().unwrap() = true;
                }
            }
        }
        self.queue
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(provider_core::LlmError::Transport("script empty".into()))
    }
}

pub(crate) fn usage() -> Usage {
    Usage {
        input: 1,
        output: 1,
        cache_read: 0,
        cache_write: 0,
        reasoning: None,
        cost_usd: None,
    }
}

pub(crate) fn tool_resp() -> Response {
    Response {
        message: AssistantMessage {
            content: "editing".into(),
            tool_calls: vec![ToolCallRef {
                id: "c1".into(),
                name: "edit".into(),
                args: serde_json::json!({
                    "path": "note.txt",
                    "search": "hello",
                    "replace": "bye",
                }),
            }],
            thinking: None,
        },
        stop: StopReason::ToolUse,
        usage: usage(),
        latency_ms: 0,
        attempts: 1,
        raw_stop_reason: None,
        retry_usage: None,
    }
}

pub(crate) fn text_resp() -> Response {
    Response {
        message: AssistantMessage {
            content: "all good".into(),
            tool_calls: vec![],
            thinking: None,
        },
        stop: StopReason::Stop,
        usage: usage(),
        latency_ms: 0,
        attempts: 1,
        raw_stop_reason: None,
        retry_usage: None,
    }
}

/// One edit call with caller-chosen search/replace (large-fix fixtures).
pub(crate) fn edit_resp(path: &str, search: &str, replace: &str) -> Response {
    Response {
        message: AssistantMessage {
            content: "editing".into(),
            tool_calls: vec![ToolCallRef {
                id: "c1".into(),
                name: "edit".into(),
                args: serde_json::json!({
                    "path": path,
                    "search": search,
                    "replace": replace,
                }),
            }],
            thinking: None,
        },
        stop: StopReason::ToolUse,
        usage: usage(),
        latency_ms: 0,
        attempts: 1,
        raw_stop_reason: None,
        retry_usage: None,
    }
}

/// One whole-file write call (new-file deliverable fixtures).
pub(crate) fn write_resp(path: &str, content: &str) -> Response {
    Response {
        message: AssistantMessage {
            content: "writing".into(),
            tool_calls: vec![ToolCallRef {
                id: "c1".into(),
                name: "write".into(),
                args: serde_json::json!({
                    "path": path,
                    "content": content,
                }),
            }],
            thinking: None,
        },
        stop: StopReason::ToolUse,
        usage: usage(),
        latency_ms: 0,
        attempts: 1,
        raw_stop_reason: None,
        retry_usage: None,
    }
}

static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub(crate) fn tmp() -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("rof-test-{n}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub(crate) fn args_for(dir: &std::path::Path, steps: Option<u32>) -> Args {
    Args {
        goal: "update the note".into(),
        workdir: dir.to_path_buf(),
        model: "fake".into(),
        endpoint: None,
        api_key_env: "OPENAI_API_KEY".into(),
        headers: Vec::new(),
        allow_cmd: vec![],
        budget_steps: steps,
        budget_actions: None,
        budget_tokens: None,
        context_files: Vec::new(),
        max_tokens: None,
        dump_events: None,
        log_path: None,
        no_log: false,
        bets: false,
        incentives: agent_loop::IncentivesLevel::Full,
        proof_cmd: None,
        compaction: None,
        thinking_keep: None,
        collapse_hysteresis: None,
        allow_dirty_workdir: false,
    }
}
