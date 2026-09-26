use super::live::LiveEvent;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
    /// Anything an agent did with the skill store, plus the harness handing a
    /// skill's text to a model. `op` is one of: `list` (index delivered to an
    /// agent), `view` (a body the model asked for), `reuse` (a body injected
    /// because the task named the skill), `propose` / `apply` (a manage op
    /// that wrote a proposal / changed the store directly).
    SkillOp {
        agent: String,
        op: String,
        /// Skill name ("" for a plain list).
        name: String,
        ok: bool,
        /// Bytes read or written, when the op has a size.
        #[serde(default)]
        bytes: u64,
    },
    /// A goal the quality pre-check flagged before a plan was paid for
    /// Informational: the round loop proceeds either way.
    GoalQuality {
        goal: String,
        note: String,
    },
    /// The harness bought one extra round after a task failed at its cap
    /// `reason` is what the retry was told.
    AutoPoke {
        task: String,
        reason: String,
    },
}

/// In-memory event stream, optionally mirrored to a JSONL file so each run
/// leaves durable, diffable evidence (the meta-harness compares runs).
///
/// The file handle is behind an `Arc<Mutex<..>>` shared by every fork: parallel
/// tasks write through different sinks in one process, and one event must stay
/// one line. (`writeln!` on a `File` issues two `write` calls — the body and
/// the newline — so two tasks could interleave into a single corrupt line;
/// measured live on a `--jobs 2` skill run.)
pub struct TraceSink {
    inner: Mutex<Vec<TraceEvent>>,
    file: Option<(Arc<Mutex<std::fs::File>>, PathBuf)>,
    /// Sum of `input_tokens + output_tokens` over every `ModelCall` emitted
    /// here. §4.3: the per-task budget reads this instead of rescanning the
    /// event stream, so spend accounting is O(1) per round, not O(rounds²).
    total: AtomicU64,
    /// Optional live subscriber. The durable trace stays authoritative, so a
    /// closed or absent sender is simply ignored, never an error.
    live: Mutex<Option<tokio::sync::mpsc::UnboundedSender<LiveEvent>>>,
    /// Serializes emission so a live subscriber can never observe `emit`ed
    /// events out of `events()` order. It spans the live send and the
    /// in-memory push; `extend` is durable-only by design and is not
    /// live-forwarded, so it needs no place in this order.
    emit_order: Mutex<()>,
}

impl TraceSink {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
            file: None,
            total: AtomicU64::new(0),
            live: Mutex::new(None),
            emit_order: Mutex::new(()),
        }
    }

    /// Mirror every event as one JSON line at `path` (created/appended).
    pub fn with_file(path: &Path) -> std::io::Result<Self> {
        let file = Self::open(path)?;
        Ok(Self {
            inner: Mutex::new(Vec::new()),
            file: Some((Arc::new(Mutex::new(file)), path.to_path_buf())),
            total: AtomicU64::new(0),
            live: Mutex::new(None),
            emit_order: Mutex::new(()),
        })
    }

    fn open(path: &Path) -> std::io::Result<std::fs::File> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        OpenOptions::new().create(true).append(true).open(path)
    }

    /// Start mirroring emitted events to `tx`. A second attach replaces the
    /// first.
    pub fn attach_live(&self, tx: tokio::sync::mpsc::UnboundedSender<LiveEvent>) {
        // `emit_order` first, then `live` (never the reverse): an in-flight
        // `emit` finishes its live send and memory push before the swap, so
        // the subscriber is attached to a consistent view of both.
        let _order = self.emit_order.lock().ok();
        if let Ok(mut guard) = self.live.lock() {
            *guard = Some(tx);
        }
    }

    /// Stop live notification; the durable trace is unaffected.
    pub fn detach_live(&self) {
        // Same lock order as `attach_live`; see the note there.
        let _order = self.emit_order.lock().ok();
        if let Ok(mut guard) = self.live.lock() {
            *guard = None;
        }
    }

    /// Fresh in-memory sink writing to the same file (per-task isolation
    /// without interleaving the aggregate stream). The file lock is shared, so
    /// concurrent tasks serialize their writes instead of racing them.
    pub fn fork(&self) -> Self {
        match &self.file {
            Some((f, p)) => Self {
                inner: Mutex::new(Vec::new()),
                file: Some((f.clone(), p.clone())),
                // A fork counts only what it emits itself; the parent's total
                // already holds the events being handed over.
                total: AtomicU64::new(0),
                // A fork never inherits the parent's live subscriber: one
                // channel, one subscriber.
                live: Mutex::new(None),
                emit_order: Mutex::new(()),
            },
            None => Self::new(),
        }
    }

    pub fn emit(&self, ev: TraceEvent) {
        // One lock for the whole method: the live forward and the in-memory
        // push must appear in the same order to every observer.
        let _order = self.emit_order.lock().ok();
        if let TraceEvent::ModelCall {
            input_tokens,
            output_tokens,
            ..
        } = &ev
        {
            self.total
                .fetch_add(*input_tokens + *output_tokens, Ordering::Relaxed);
        }
        if let Some((file, _)) = &self.file {
            if let Ok(line) = serde_json::to_string(&ev) {
                if let Ok(mut f) = file.lock() {
                    // One write per event: body + newline in a single buffer.
                    let mut buf = line.into_bytes();
                    buf.push(b'\n');
                    let _ = f.write_all(&buf);
                }
            }
        }
        let live = self.live.lock().ok().and_then(|guard| guard.clone());
        if let Some(tx) = live {
            let _ = tx.send(LiveEvent::Trace(ev.clone()));
        }
        if let Ok(mut guard) = self.inner.lock() {
            guard.push(ev);
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().map(|g| g.is_empty()).unwrap_or(true)
    }

    pub fn events(&self) -> Vec<TraceEvent> {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// In-memory only: the source sink already wrote these lines to file.
    pub fn extend(&self, events: Vec<TraceEvent>) {
        if let Ok(mut guard) = self.inner.lock() {
            for ev in &events {
                if let TraceEvent::ModelCall {
                    input_tokens,
                    output_tokens,
                    ..
                } = ev
                {
                    self.total
                        .fetch_add(*input_tokens + *output_tokens, Ordering::Relaxed);
                }
            }
            guard.extend(events);
        }
    }

    /// Total model-call tokens emitted into this sink (input + output). The
    /// budget counter (§4.3) reads this; a fork starts from zero.
    pub fn total_tokens(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
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
    use crate::obs::LiveEvent;

    /// The live channel is an optimization of the durable trace, never a
    /// replacement: whatever a subscriber sees must equal `events()`. The
    /// invariant covers events delivered through `emit`; `extend` is
    /// durable-only by design and is not live-forwarded.
    #[test]
    fn live_sender_receives_the_same_event_order_as_the_sink() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = TraceSink::new();
        sink.attach_live(tx);

        sink.emit(TraceEvent::StateTransition {
            from: "ready".into(),
            to: "implementing".into(),
        });
        sink.emit(TraceEvent::StateTransition {
            from: "implementing".into(),
            to: "reviewing".into(),
        });

        let delivered: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|event| match event {
                LiveEvent::Trace(event) => serde_json::to_string(&event).unwrap(),
                other => panic!("unexpected non-trace notification: {other:?}"),
            })
            .collect();
        let stored: Vec<String> = sink
            .events()
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .collect();
        assert_eq!(delivered, stored);
    }

    /// Detaching only stops notification; the durable sink keeps recording.
    #[test]
    fn detaching_live_notifications_keeps_the_durable_sink() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = TraceSink::new();
        sink.attach_live(tx);
        sink.detach_live();
        sink.emit(TraceEvent::StateTransition {
            from: "ready".into(),
            to: "implementing".into(),
        });
        assert!(rx.try_recv().is_err());
        assert_eq!(sink.len(), 1);
    }

    /// Threads sharing one sink, not just separate forks: `emit` holds
    /// `emit_order` across the live send and the memory push, so a single
    /// subscriber still sees exactly the durable order.
    #[test]
    fn concurrent_emitters_on_one_sink_keep_live_and_durable_order_equal() {
        const THREADS: usize = 4;
        const PER_THREAD: usize = 50;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = Arc::new(TraceSink::new());
        sink.attach_live(tx);

        let start = Arc::new(std::sync::Barrier::new(THREADS));
        let mut threads = Vec::new();
        for t in 0..THREADS {
            let sink = Arc::clone(&sink);
            let start = Arc::clone(&start);
            threads.push(std::thread::spawn(move || {
                start.wait();
                for i in 0..PER_THREAD {
                    sink.emit(TraceEvent::ToolCall {
                        agent: format!("agent-{t}"),
                        tool: format!("tool-{i}"),
                        ok: true,
                        latency_ms: (t * PER_THREAD + i) as u64,
                    });
                }
            }));
        }
        for t in threads {
            t.join().unwrap();
        }

        let delivered: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|event| match event {
                LiveEvent::Trace(event) => serde_json::to_string(&event).unwrap(),
                other => panic!("unexpected non-trace notification: {other:?}"),
            })
            .collect();
        let stored: Vec<String> = sink
            .events()
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .collect();

        assert_eq!(stored.len(), THREADS * PER_THREAD);
        assert_eq!(delivered, stored);
    }

    /// A subscriber that went away (aborted run, dropped monitor) must not
    /// break emission: the live send fails, the durable trace still records.
    #[test]
    fn emitting_after_the_receiver_is_dropped_keeps_the_durable_sink() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = TraceSink::new();
        sink.attach_live(tx);
        drop(rx);

        sink.emit(TraceEvent::StateTransition {
            from: "ready".into(),
            to: "implementing".into(),
        });

        assert_eq!(sink.len(), 1);
    }

    /// A fork is a separate subscriber scope: the parent's channel must not
    /// see the child's events.
    #[test]
    fn fork_does_not_inherit_the_parent_live_subscriber() {
        let path =
            std::env::temp_dir().join(format!("rof-trace-fork-live-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        // Exercise the file-backed fork arm used by the evaluation runner.
        let parent = TraceSink::with_file(&path).unwrap();
        parent.attach_live(tx);
        let child = parent.fork();

        child.emit(TraceEvent::ToolCall {
            agent: "implementer".into(),
            tool: "apply".into(),
            ok: true,
            latency_ms: 1,
        });

        assert!(rx.try_recv().is_err(), "parent saw a child event");
        assert_eq!(child.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

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

    /// Parallel tasks write through their own forks; one event must stay one
    /// line. Measured live: `writeln!` on a File issued two writes per event
    /// (body, newline) and two tasks interleaved into one corrupt line.
    #[test]
    fn concurrent_forks_write_one_line_per_event() {
        let path =
            std::env::temp_dir().join(format!("rof-trace-race-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let sink = TraceSink::with_file(&path).unwrap();
        let mut threads = Vec::new();
        for t in 0..8 {
            let child = sink.fork();
            threads.push(std::thread::spawn(move || {
                for i in 0..50 {
                    child.emit(TraceEvent::ToolCall {
                        agent: "implementer".into(),
                        tool: format!("tool-{i}"),
                        ok: t % 2 == 0,
                        latency_ms: i as u64,
                    });
                }
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(
            lines.len(),
            400,
            "every event is its own line: {}",
            lines.len()
        );
        for l in &lines {
            serde_json::from_str::<TraceEvent>(l)
                .unwrap_or_else(|e| panic!("corrupt line: {e}\n{l}"));
        }
        let _ = std::fs::remove_file(&path);
    }
}
