//! Ordered sink + live forward + JSONL append over the agent-event vocabulary.
//! Token totals live here only as passed-in counts; shared atomic accounting
//! waits for a consumer that needs it.
use agent_event::AgentEvent;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub struct TraceSink {
    inner: Mutex<Vec<AgentEvent>>,
    file: Option<(Arc<Mutex<std::fs::File>>, PathBuf)>,
    taps: Mutex<Vec<mpsc::Sender<AgentEvent>>>,
    // ponytail: one global lock; per-sink sharding if emit contention matters.
    order: Mutex<()>,
}

impl TraceSink {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
            file: None,
            taps: Mutex::new(Vec::new()),
            order: Mutex::new(()),
        }
    }

    /// Mirror every event as one JSON line at `path` (created/appended, never truncated).
    pub fn with_file(path: &Path) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            inner: Mutex::new(Vec::new()),
            file: Some((Arc::new(Mutex::new(file)), path.to_path_buf())),
            taps: Mutex::new(Vec::new()),
            order: Mutex::new(()),
        })
    }

    /// Ordered across live forward and durable push; dead/closed subscriber
    /// is silently ignored, never an error.
    pub fn emit(&self, event: AgentEvent) {
        let _order = self.order.lock().ok();
        if let Some((file, _)) = &self.file {
            if let Ok(line) = serde_json::to_string(&event) {
                if let Ok(mut f) = file.lock() {
                    // One write per event: body + newline in a single buffer.
                    let mut buf = line.into_bytes();
                    buf.push(b'\n');
                    let _ = f.write_all(&buf);
                }
            }
        }
        if let Ok(mut taps) = self.taps.lock() {
            taps.retain(|tx| {
                !matches!(
                    tx.try_send(event.clone()),
                    Err(mpsc::error::TrySendError::Closed(_))
                )
            });
        }
        if let Ok(mut guard) = self.inner.lock() {
            guard.push(event);
        }
    }

    /// Live tap. Bounded; drop-newest on full while history() stays lossless.
    pub fn tap(&self, capacity: usize) -> mpsc::Receiver<AgentEvent> {
        let _order = self.order.lock().ok();
        let (tx, rx) = mpsc::channel(capacity.max(1));
        if let Ok(mut taps) = self.taps.lock() {
            taps.push(tx);
        }
        rx
    }

    /// Lossless record for replay. Forks never inherit taps.
    pub fn history(&self) -> Vec<AgentEvent> {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn fork(&self) -> Self {
        match &self.file {
            Some((f, p)) => Self {
                inner: Mutex::new(Vec::new()),
                file: Some((f.clone(), p.clone())),
                taps: Mutex::new(Vec::new()),
                order: Mutex::new(()),
            },
            None => Self::new(),
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
    use agent_event::{Message, Role, RunOutcome};

    fn run_start(id: u64, goal: &str) -> AgentEvent {
        AgentEvent::RunStart {
            run_id: id,
            goal: goal.to_string(),
        }
    }

    #[test]
    fn order_live_matches_durable_and_full_tap_drops_newest() {
        let sink = TraceSink::new();
        let mut rx = sink.tap(2);
        for i in 0..5 {
            sink.emit(run_start(i, "g"));
        }
        let history = sink.history();
        assert_eq!(history.len(), 5);
        // Bounded tap kept the first 2; the rest dropped, history lossless.
        assert!(matches!(
            rx.try_recv().unwrap(),
            AgentEvent::RunStart { run_id: 0, .. }
        ));
        assert!(matches!(
            rx.try_recv().unwrap(),
            AgentEvent::RunStart { run_id: 1, .. }
        ));
        assert!(rx.try_recv().is_err());
        // Live order equals durable order for what was delivered.
        let live_ids: Vec<u64> = history
            .iter()
            .take(2)
            .map(|e| match e {
                AgentEvent::RunStart { run_id, .. } => *run_id,
                _ => u64::MAX,
            })
            .collect();
        assert_eq!(live_ids, vec![0, 1]);
    }

    #[test]
    fn dead_tap_ignored() {
        let sink = TraceSink::new();
        let rx = sink.tap(8);
        drop(rx);
        sink.emit(run_start(1, "g"));
        sink.emit(AgentEvent::RunEnd {
            outcome: RunOutcome::Passed,
            messages: vec![Message {
                role: Role::User,
                content: "hi".into(),
            }],
        });
        assert_eq!(sink.history().len(), 2);
    }

    #[test]
    fn fork_isolated() {
        let sink = TraceSink::new();
        let mut rx = sink.tap(8);
        let child = sink.fork();
        assert!(child.history().is_empty());
        child.emit(run_start(9, "child"));
        assert_eq!(child.history().len(), 1);
        assert!(sink.history().is_empty());
        // Fork inherited no taps: parent tap saw nothing from the child.
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn jsonl_readback_equals_history() {
        let path = std::env::temp_dir().join(format!("trace-{}-replay.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let events = vec![
            run_start(0, "g"),
            AgentEvent::TurnStart { turn: 1 },
            AgentEvent::RunEnd {
                outcome: RunOutcome::Passed,
                messages: vec![Message {
                    role: Role::Assistant,
                    content: "done".into(),
                }],
            },
        ];
        {
            let sink = TraceSink::with_file(&path).unwrap();
            for e in &events {
                sink.emit(e.clone());
            }
            assert_eq!(sink.history(), events);
        }
        // create+append never truncates: reopen and add one more line.
        {
            let sink = TraceSink::with_file(&path).unwrap();
            sink.emit(run_start(7, "again"));
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        let replayed: Vec<AgentEvent> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(&replayed[..3], &events[..]);
        assert_eq!(replayed[3], run_start(7, "again"));
        let _ = std::fs::remove_file(&path);
    }
}
