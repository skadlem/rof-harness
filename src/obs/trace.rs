use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Every orchestrator / agent / tool step emits one of these.
/// The eval layer aggregates cost/latency/reliability from this stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TraceEvent {
    SessionStart {
        session_id: String,
        goal: String,
    },
    ModelCall {
        agent: String,
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        latency_ms: u64,
        #[serde(default)]
        cost_usd: Option<f64>,
        #[serde(default)]
        cached_input_tokens: u64,
        /// HTTP attempts for this call (1 = no retry).
        #[serde(default)]
        attempts: u64,
    },
    /// A model call that ultimately failed — the reliability signal that
    /// success-rate alone hides.
    ModelError {
        agent: String,
        error: String,
    },
    ToolCall {
        agent: String,
        tool: String,
        ok: bool,
        latency_ms: u64,
    },
    StateTransition {
        from: String,
        to: String,
    },
    ReviewVerdict {
        pass: bool,
        feedback: String,
    },
    /// A task stopped early because it crossed its token ceiling.
    BudgetExceeded {
        task: String,
        tokens: u64,
        limit: u64,
    },
}

/// In-memory event stream, optionally mirrored to a JSONL file so each run
/// leaves durable, diffable evidence (the meta-harness compares runs).
pub struct TraceSink {
    inner: Mutex<(Vec<TraceEvent>, Option<std::fs::File>)>,
    file_path: Option<PathBuf>,
}

impl TraceSink {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new((Vec::new(), None)),
            file_path: None,
        }
    }

    /// Mirror every event as one JSON line at `path` (created/appended).
    pub fn with_file(path: &Path) -> std::io::Result<Self> {
        let file = Self::open(path)?;
        Ok(Self {
            inner: Mutex::new((Vec::new(), Some(file))),
            file_path: Some(path.to_path_buf()),
        })
    }

    fn open(path: &Path) -> std::io::Result<std::fs::File> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        OpenOptions::new().create(true).append(true).open(path)
    }

    /// Fresh in-memory sink writing to the same file (per-task isolation
    /// without interleaving the aggregate stream).
    pub fn fork(&self) -> Self {
        match &self.file_path {
            Some(p) => Self::with_file(p).unwrap_or_else(|_| Self::new()),
            None => Self::new(),
        }
    }

    pub fn emit(&self, ev: TraceEvent) {
        if let Ok(mut guard) = self.inner.lock() {
            let (events, file) = &mut *guard;
            if let Some(f) = file {
                if let Ok(line) = serde_json::to_string(&ev) {
                    let _ = writeln!(f, "{line}");
                }
            }
            events.push(ev);
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.0.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().map(|g| g.0.is_empty()).unwrap_or(true)
    }

    pub fn events(&self) -> Vec<TraceEvent> {
        self.inner.lock().map(|g| g.0.clone()).unwrap_or_default()
    }

    /// In-memory only: the source sink already wrote these lines to file.
    pub fn extend(&self, events: Vec<TraceEvent>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.0.extend(events);
        }
    }
}

impl Default for TraceSink {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_jsonl_lines_and_keeps_memory() {
        let path = std::env::temp_dir().join(format!("rof-trace-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let sink = TraceSink::with_file(&path).unwrap();
            sink.emit(TraceEvent::ModelCall {
                agent: "p".into(),
                model: "m".into(),
                input_tokens: 1,
                output_tokens: 2,
                latency_ms: 3,
                cost_usd: Some(0.001),
                cached_input_tokens: 512,
                attempts: 2,
            });
            // fork writes to the same file without touching the parent's vec
            let child = sink.fork();
            child.emit(TraceEvent::StateTransition {
                from: "a".into(),
                to: "b".into(),
            });
            assert_eq!(sink.len(), 1);
            assert_eq!(child.len(), 1);
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "each event is one line: {text}");
        for l in lines {
            serde_json::from_str::<TraceEvent>(l).expect("each line parses");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_does_not_truncate() {
        let path = std::env::temp_dir().join(format!("rof-trace-app-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        for _ in 0..2 {
            let sink = TraceSink::with_file(&path).unwrap();
            sink.emit(TraceEvent::SessionStart {
                session_id: "s".into(),
                goal: "g".into(),
            });
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
        let _ = std::fs::remove_file(&path);
    }
}
