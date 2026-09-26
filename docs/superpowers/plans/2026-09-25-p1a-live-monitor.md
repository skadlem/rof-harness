# P1a Live Read-Only Monitor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run one `rof chat` goal in a worker while the terminal pump remains active and renders ordered, bounded run activity.

**Architecture:** `TraceSink` keeps its existing durable behavior and gains an optional ordered live notification channel. `main.rs` supplies a `GoalRunner` that runs the existing orchestrator silently inside a Tokio task. `src/tui/run.rs` owns an idle/running session state machine, while `App` and `ui.rs` remain the only render state and a pure `&App` render path.

**Tech Stack:** Rust, Tokio unbounded channels/tasks, Ratatui, Crossterm, Serde, existing `TraceSink`/`Orchestrator`/`StubClient` infrastructure. No new dependency.

## Global Constraints

- P1a is a read-only live monitor. Composer submission, steering, queued goals, and mid-run configuration changes are out of scope.
- `draw` remains a pure function of `&App`; it performs no file, git, environment, clock, credential, or channel I/O.
- `TraceSink` remains the durable/evaluation source. Live notification is an optimization and never replaces JSONL or in-memory trace state.
- Live channel order must equal `TraceSink::events()` order for every emitted event.
- The worker rebuilds services from `apply_env` and stored logins for each goal, matching the existing between-goals contract.
- Replay, `run`, and `eval` behavior must remain unchanged.
- Secrets, credential text, and authentication responses must not enter `App`, traces, activity lines, or test snapshots.
- Each task follows red-green-refactor: write the failing test, run it, implement the minimum, run the focused test, then run the task gate.
- The phase gate is `cargo fmt -- --check`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.

---

## File Map

- Create `src/obs/live.rs`: `LiveEvent`, `Boundary`, and `GoalFinished` notification types.
- Modify `src/obs/mod.rs`: re-export the live notification types.
- Modify `src/obs/trace.rs`: optional live sender, emission ordering lock, attach/detach API, and order tests.
- Modify `src/tui/app.rs`: `RunMode`, bounded live activity, run goal/outcome state, and pure activity accessors.
- Modify `src/tui/run.rs`: `LiveSession`, worker handle type, idle/running pump state machine, and channel lifecycle.
- Modify `src/tui/theme.rs`: composer frame variant that can show the P1a read-only posture.
- Modify `src/tui/ui.rs`: bounded activity region and small-terminal layout.
- Modify `src/main.rs`: `GoalRunner`, silent worker execution, shared goal execution helper, and the new chat wiring.
- Modify `tests/tui_app.rs`: TestBackend activity/read-only layout coverage.
- Create `tests/tui_live.rs`: live session, App reducer, ordering, and stub-worker integration coverage.
- Modify `src/tools/mod.rs`: move the existing anchor test module to the file end for the Clippy preflight.
- Modify `src/eval/suite.rs`: replace the flagged `&vec![...]` with a slice literal.

---

### Task 1: Clear the Clippy Phase-Gate Baseline

**Files:**
- Modify: `src/tools/mod.rs` (move the existing `anchor_tests` module to EOF)
- Modify: `src/eval/suite.rs:103` (slice literal)
- Test: existing unit tests in both files

**Interfaces:**
- Consumes: no new interfaces.
- Produces: a tree where `cargo clippy --all-targets --all-features -- -D warnings` can be used as a real phase gate.

- [ ] **Step 1: Record the two current warnings**

Run:

```bash
cd /home/madiyar/rof-harness
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: failure naming exactly `clippy::items_after_test_module` in `src/tools/mod.rs` and `clippy::useless_vec` in `src/eval/suite.rs`.

- [ ] **Step 2: Move the existing test module without changing its tests**

In `src/tools/mod.rs`, remove the existing block beginning with `#[cfg(test)] mod anchor_tests {` and paste the identical block after the final `impl Tool for FsWriteTool` block. Keep the test names, bodies, imports, and assertions unchanged. Do not add `#[allow]`; this is a placement fix, not a lint suppression.

- [ ] **Step 3: Replace the flagged temporary vector**

In `src/eval/suite.rs`, change exactly:

```rust
&vec![cr("cargo test bar", false)],
```

to:

```rust
&[cr("cargo test bar", false)],
```

- [ ] **Step 4: Run the preflight gate**

Run:

```bash
cargo fmt -- --check
cargo test --lib
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all three commands exit 0, with no new warning.

- [ ] **Step 5: Commit the mechanical preflight**

```bash
git add src/tools/mod.rs src/eval/suite.rs
git commit -m "chore: clear clippy phase-gate warnings"
```

---

### Task 2: Add the Ordered Live Notification Contract

**Files:**
- Create: `src/obs/live.rs`
- Modify: `src/obs/mod.rs`
- Modify: `src/obs/trace.rs`
- Test: unit tests at the bottom of `src/obs/trace.rs`

**Interfaces:**
- Produces `obs::live::{Boundary, GoalFinished, LiveEvent}`.
- Produces `TraceSink::attach_live(UnboundedSender<LiveEvent>)` and `TraceSink::detach_live()`.
- Preserves `TraceSink::emit(&TraceEvent)`, `events()`, `fork()`, `total_tokens()`, and JSONL behavior.

- [ ] **Step 1: Write failing trace-seam tests**

Add these tests to `src/obs/trace.rs`:

```rust
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
```

The test module must import `super::live::LiveEvent` (or use the re-exported path) so the intended public type is exercised.

- [ ] **Step 2: Run the new tests to verify they fail**

Run:

```bash
cargo test --lib obs::trace::tests::live_sender_receives_the_same_event_order_as_the_sink
cargo test --lib obs::trace::tests::detaching_live_notifications_keeps_the_durable_sink
```

Expected: compilation fails because `LiveEvent` and `TraceSink::attach_live` do not exist yet.

- [ ] **Step 3: Create the notification types**

Create `src/obs/live.rs` with:

```rust
use super::trace::TraceEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    Started,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalFinished {
    pub passed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum LiveEvent {
    Trace(TraceEvent),
    Boundary(Boundary),
    Finished(GoalFinished),
}
```

Update `src/obs/mod.rs` to declare and re-export the module:

```rust
pub mod live;
pub mod trace;
pub use live::{Boundary, GoalFinished, LiveEvent};
pub use trace::{TraceEvent, TraceSink};
```

- [ ] **Step 4: Add the optional sender and emission lock**

In `TraceSink`, add these fields and initialize them as `None`/`Mutex::new(())` in `new`, `with_file`, and `fork`:

```rust
live: Mutex<Option<tokio::sync::mpsc::UnboundedSender<LiveEvent>>>,
emit_order: Mutex<()>,
```

Add:

```rust
pub fn attach_live(&self, tx: tokio::sync::mpsc::UnboundedSender<LiveEvent>) {
    if let Ok(mut guard) = self.live.lock() {
        *guard = Some(tx);
    }
}

pub fn detach_live(&self) {
    if let Ok(mut guard) = self.live.lock() {
        *guard = None;
    }
}
```

At the top of `emit`, acquire `emit_order` and hold it until the method returns. Keep the existing total/file/memory work, then forward the clone and push the original in that order:

```rust
let _order = self.emit_order.lock().ok();
```

Place the live forward immediately before the existing in-memory push:

```rust
let live = self
    .live
    .lock()
    .ok()
    .and_then(|guard| guard.clone());
if let Some(tx) = live {
    let _ = tx.send(LiveEvent::Trace(ev.clone()));
}
if let Ok(mut guard) = self.inner.lock() {
    guard.push(ev);
}
```

Do not forward events from `extend`; it is a durable sink merge used by evaluation, not a live worker emission.

- [ ] **Step 5: Run the focused tests and the trace suite**

Run:

```bash
cargo fmt -- --check
cargo test --lib obs::trace
cargo test --lib obs::live
```

Expected: the two new tests and all existing trace tests pass.

- [ ] **Step 6: Commit the notification seam**

```bash
git add src/obs/live.rs src/obs/mod.rs src/obs/trace.rs
git commit -m "feat(obs): add ordered live notifications"
```

---

### Task 3: Add the Pure App Run State and Live Session Reducer

**Files:**
- Modify: `src/tui/app.rs`
- Modify: `src/tui/run.rs` (state types only; terminal wiring is Task 6)
- Create: `tests/tui_live.rs`

**Interfaces:**
- Consumes: `obs::{Boundary, GoalFinished, LiveEvent}`.
- Produces `tui::app::RunMode` with `Idle`, `Running`, `Stopping`, `Finished`, and `Failed`.
- Produces `App::begin_run(&str)`, `App::set_stopping()`, `App::on_live_boundary(Boundary)`, `App::on_live_finished(&GoalFinished)`, and `App::activity_tail(usize) -> Vec<String>`.
- Produces `tui::run::WorkerHandle` and `tui::run::LiveSession` with `new`, `begin`, `drain`, `request_stop`, `stop_requested`, `is_running`, and `reset`. `drain` has the exact return type `Option<GoalFinished>`.

- [ ] **Step 1: Write failing App/session tests**

Create `tests/tui_live.rs` with these behaviors:

```rust
use rof::obs::{Boundary, GoalFinished, LiveEvent, TraceEvent};
use rof::tui::app::{App, RunMode};
use rof::tui::run::LiveSession;
use tokio::sync::mpsc::unbounded_channel;

#[test]
fn live_events_update_activity_and_lifecycle() {
    let mut app = App::new();
    app.begin_run("fix the parser");
    assert_eq!(app.run_mode, RunMode::Running);
    app.on_event(&TraceEvent::StateTransition {
        from: "ready".into(),
        to: "implementing".into(),
    });
    assert_eq!(app.activity_tail(10).len(), 1);
    assert!(app.activity_tail(10)[0].contains("implementing"));

    app.on_live_boundary(Boundary::Finished);
    app.on_live_finished(&GoalFinished {
        passed: true,
        error: None,
    });
    assert_eq!(app.run_mode, RunMode::Finished);
    assert!(app.status_line().contains("finished"));
}

#[test]
fn live_activity_is_bounded_by_lines_and_characters() {
    let mut app = App::new();
    app.begin_run("bounded");
    for i in 0..260 {
        app.on_event(&TraceEvent::StateTransition {
            from: format!("state-{i}"),
            to: format!("next-{i}"),
        });
    }
    assert!(app.activity_tail(1000).len() <= 200);
    assert!(app.activity_chars <= 32_000);
}

#[tokio::test]
async fn live_session_reports_finished_once() {
    let (tx, rx) = unbounded_channel();
    let mut app = App::new();
    let mut session = LiveSession::new(rx);
    let handle = tokio::spawn(async move {
        Ok(GoalFinished {
            passed: true,
            error: None,
        })
    });
    session.begin("goal", handle);
    assert!(session.is_running());

    tx.send(LiveEvent::Trace(TraceEvent::StateTransition {
        from: "a".into(),
        to: "b".into(),
    })).unwrap();
    tx.send(LiveEvent::Boundary(Boundary::Started)).unwrap();
    tx.send(LiveEvent::Boundary(Boundary::Finished)).unwrap();
    tx.send(LiveEvent::Finished(GoalFinished {
        passed: true,
        error: None,
    })).unwrap();

    assert!(session.drain(&mut app).is_some());
    assert!(!session.drain(&mut app).is_some());
    assert_eq!(app.run_mode, RunMode::Finished);
}
```

`drain` returns `None` while the worker is active and `Some(GoalFinished)` exactly once for a terminal outcome. The test above must not depend on a real model or terminal.

- [ ] **Step 2: Run the new tests to verify they fail**

Run:

```bash
cargo test --test tui_live
```

Expected: compilation fails because `RunMode`, the App methods, and `LiveSession` do not exist.

- [ ] **Step 3: Add bounded App state**

In `src/tui/app.rs`, import `std::collections::VecDeque` and define:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Idle,
    Running,
    Stopping,
    Finished,
    Failed,
}

const ACTIVITY_MAX_LINES: usize = 200;
const ACTIVITY_MAX_CHARS: usize = 32_000;
```

Add fields to `App`:

```rust
pub run_mode: RunMode,
pub activity: VecDeque<String>,
pub activity_chars: usize,
pub run_goal: String,
pub run_outcome: Option<String>,
```

Initialize them in `Default` with `RunMode::Idle`, an empty deque, zero chars, an empty goal, and `None` outcome.

Add `push_activity_line(&mut self, line: String)` that truncates a single line to `ACTIVITY_MAX_CHARS`, appends it, then removes `front()` entries while either bound is exceeded, subtracting their character lengths. Add `activity_tail(&self, height: usize) -> Vec<String>` that returns the last `height` lines in chronological order.

Update `on_event` to compute the rendered line once, push it to `transcript`, update counters as today, and call `push_activity_line` when `run_mode` is `Running` or `Stopping`.

Add `begin_run`, `set_stopping`, `on_live_boundary`, and `on_live_finished`. `begin_run` clears the activity/outcome, sets the goal, and sets `Running`. `set_stopping` changes `Running` to `Stopping` without touching the goal. `on_live_boundary(Started)` sets `Running`; `Finished` leaves the current mode unchanged until the outcome arrives. `on_live_finished` sets `Finished` when `passed` is true and `Failed` otherwise, stores a concise outcome line, and appends it to the transcript.

Extend `status_line` with the run mode only when it is not `Idle`, preserving the existing counters and replay prefix text.

- [ ] **Step 4: Add the pure LiveSession reducer**

In `src/tui/run.rs`, define:

```rust
pub type WorkerHandle =
    tokio::task::JoinHandle<anyhow::Result<GoalFinished>>;

pub struct LiveSession {
    receiver: tokio::sync::mpsc::UnboundedReceiver<LiveEvent>,
    worker: Option<WorkerHandle>,
    stop_requested: bool,
}
```

Implement `new`, `begin`, `is_running`, `request_stop`, `stop_requested`, `reset`, and `drain`. `drain` must loop on `receiver.try_recv()`, apply `LiveEvent::Trace` through `App::on_event`, apply boundaries/outcomes through the App methods, and return `Some(GoalFinished)` exactly once. After draining, if the stored worker handle is finished without a `Finished` event, return `Some(GoalFinished { passed: false, error: Some("worker exited without a terminal outcome".into()) })` and mark the App failed. `request_stop` must not abort the worker.

- [ ] **Step 5: Run the focused tests and existing TUI tests**

Run:

```bash
cargo fmt -- --check
cargo test --test tui_live --test tui_app
cargo test --lib tui::app
```

Expected: all new reducer tests and all existing TUI tests pass.

- [ ] **Step 6: Commit the pure state layer**

```bash
git add src/tui/app.rs src/tui/run.rs tests/tui_live.rs
git commit -m "feat(tui): add live run state reducer"
```

---

### Task 4: Extract a Silent Goal Runner

**Files:**
- Modify: `src/main.rs`
- Test: `#[cfg(test)]` module in `src/main.rs`

**Interfaces:**
- Consumes: `AppConfig`, `Arc<TraceSink>`, `PathBuf`, `build_services`, `Orchestrator`, and the live notification types.
- Produces `GoalRunner::from_setup(&Setup) -> GoalRunner`, `GoalRunner::execute(&str) -> anyhow::Result<GoalResult>`, and `GoalRunner::run_live(String, UnboundedSender<LiveEvent>) -> anyhow::Result<GoalFinished>`.
- Preserves `run_goal_text` output and exit behavior for the `run` command.

- [ ] **Step 1: Write a failing stub-worker test**

Add a `#[cfg(test)] mod tests` at the end of `src/main.rs` with a test that serializes provider-environment access, creates a unique temporary directory, constructs a `GoalRunner` with `AppConfig::default()`, `TraceSink::new()`, and the temporary root, starts a live channel, and asserts that the runner emits `Boundary::Started` and `Finished`:

```rust
#[tokio::test]
async fn goal_runner_reports_a_terminal_live_outcome() {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _env = ENV_LOCK.lock().unwrap();
    for key in ["ROF_TOKEN", "OR_TOKEN", "ROF_CHAT_BASE"] {
        std::env::remove_var(key);
    }
    let root = std::env::temp_dir().join(format!("rof-p1a-runner-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let runner = GoalRunner {
        cfg: AppConfig::default(),
        trace: Arc::new(TraceSink::new()),
        root: root.clone(),
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let result = runner
        .run_live("stub smoke goal".into(), tx)
        .await
        .unwrap();
    assert!(matches!(rx.try_recv(), Ok(LiveEvent::Boundary(Boundary::Started))));
    assert!(matches!(rx.try_recv(), Ok(LiveEvent::Boundary(Boundary::Finished))));
    let delivered = match rx.try_recv().unwrap() {
        LiveEvent::Finished(finished) => finished,
        other => panic!("expected Finished, got {other:?}"),
    };
    assert_eq!(result, delivered);
    let _ = std::fs::remove_dir_all(root);
}
```

The returned value and the `Finished` notification must be the same `GoalFinished` value; the test intentionally does not assert whether the stub goal passed. The `ENV_LOCK` prevents another main unit test from selecting a real provider while this test removes the provider variables.

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo test --bin rof goal_runner_reports_a_terminal_live_outcome
```

Expected: compilation fails because `GoalRunner`, `GoalResult`, and `GoalFinished` are not yet wired.

- [ ] **Step 3: Add the owned runner types**

Near `Setup`, add:

```rust
#[derive(Clone)]
struct GoalRunner {
    cfg: AppConfig,
    trace: Arc<TraceSink>,
    root: PathBuf,
}

#[derive(Debug)]
struct GoalResult {
    value: serde_json::Value,
    passed: bool,
    error: Option<String>,
}
```

Implement `GoalRunner::from_setup` by cloning `s.cfg`, `s.trace`, and `s.root`.

- [ ] **Step 4: Extract the existing execution body**

Move the current body of `run_goal_text` that applies env, exports stored logins, builds services, builds checks, creates `Orchestrator`, and awaits `run_loop` into:

```rust
async fn execute(&self, goal: &str) -> anyhow::Result<GoalResult> {
    let mut cfg = self.cfg.clone();
    apply_env(&mut cfg);
    rof::tui::auth::store().export_missing_env();
    let (context, executor, verify) = build_services(&cfg, false);
    let checks = std::env::var("ROF_CHECK")
        .map(|c| c.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let expect_writes = cfg.expect_writes;
    let registry = ToolRegistry::with_defaults(
        self.root.clone(),
        cfg.permissions.clone(),
        cfg.skills.clone(),
    );
    let orch = Orchestrator::new(cfg, self.trace.clone(), context, executor, verify);
    let value = orch
        .run_loop(
            &Session::new(goal.to_string())
                .with_checks(checks)
                .expecting_writes(expect_writes),
            &registry,
            &self.root,
        )
        .await;
    let passed = value["passed"].as_bool().unwrap_or(false);
    let error = value
        .get("error")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok(GoalResult { value, passed, error })
}
```

Keep `run_goal_text` as the printing wrapper: call `GoalRunner::from_setup(s).execute(goal).await?`, print `goal` and the serialized result, call `print_report`, print trace events when requested, and preserve the existing exit/failure messages exactly.

- [ ] **Step 5: Add the live wrapper**

Implement:

```rust
async fn run_live(
    &self,
    goal: String,
    tx: tokio::sync::mpsc::UnboundedSender<LiveEvent>,
) -> anyhow::Result<GoalFinished> {
    let _ = tx.send(LiveEvent::Boundary(Boundary::Started));
    let result = self.execute(&goal).await?;
    let finished = GoalFinished {
        passed: result.passed,
        error: result.error,
    };
    let _ = tx.send(LiveEvent::Boundary(Boundary::Finished));
    let _ = tx.send(LiveEvent::Finished(finished.clone()));
    Ok(finished)
}
```

The worker must not print. It may ignore a closed channel because the durable trace remains authoritative.

- [ ] **Step 6: Run the runner test and the run/eval smoke tests**

Run:

```bash
cargo fmt -- --check
cargo test --bin rof goal_runner_reports_a_terminal_live_outcome
cargo test --test eval_suite --test loop
```

Expected: the stub runner test passes and the existing execution suites remain green.

- [ ] **Step 7: Commit the runner extraction**

```bash
git add src/main.rs
git commit -m "feat(chat): add silent goal runner"
```

---

### Task 5: Render the Bounded Activity Region

**Files:**
- Modify: `src/tui/theme.rs`
- Modify: `src/tui/ui.rs`
- Modify: `src/tui/app.rs` (only if a small accessor is missing)
- Modify: `tests/tui_app.rs`

**Interfaces:**
- Consumes: `App::activity_tail`, `App::run_mode`, `App::run_goal`, and `App::run_outcome` from Task 3.
- Produces: a pure four-region vertical layout: transcript, run activity, status, composer.
- Preserves: `theme::composer_block(&str)` and all existing replay rendering.

- [ ] **Step 1: Write failing TestBackend layout tests**

Add tests to `tests/tui_app.rs`:

```rust
#[test]
fn live_activity_and_read_only_composer_render() {
    use ratatui::{backend::TestBackend, Terminal};
    use rof::tui::{app::App, ui::draw};
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::new();
    app.begin_run("watch the run");
    app.on_event(&rof::obs::TraceEvent::StateTransition {
        from: "ready".into(),
        to: "implementing".into(),
    });
    terminal.draw(|f| draw(f, &app)).unwrap();
    let out: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol().to_string())
        .collect();
    assert!(out.contains("run activity"), "{out}");
    assert!(out.contains("implementing"), "{out}");
    assert!(out.contains("read-only"), "{out}");
}

#[test]
fn small_terminal_keeps_transcript_status_and_composer() {
    use ratatui::{backend::TestBackend, Terminal};
    use rof::tui::{app::App, ui::draw};
    let backend = TestBackend::new(60, 9);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::new();
    app.transcript.push("tail line".into());
    app.begin_run("small");
    terminal.draw(|f| draw(f, &app)).unwrap();
    let out: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol().to_string())
        .collect();
    assert!(out.contains("tail line"), "{out}");
    assert!(out.contains("status"), "{out}");
    assert!(out.contains("composer"), "{out}");
}
```

- [ ] **Step 2: Run the layout tests to verify they fail**

Run:

```bash
cargo test --test tui_app live_activity_and_read_only_composer_render
cargo test --test tui_app small_terminal_keeps_transcript_status_and_composer
```

Expected: FAIL because the activity region and read-only composer title do not exist.

- [ ] **Step 3: Add the read-only composer frame**

Keep the existing function unchanged and add:

```rust
pub fn composer_block_with_state(thinking: &str, read_only: bool) -> Block<'static> {
    let title = if read_only {
        "composer · read-only (P1a)".to_string()
    } else if thinking.trim().is_empty() {
        "composer".to_string()
    } else {
        format!("composer · {}", thinking.trim())
    };
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(AMBER))
}
```

Make `composer_block(thinking)` delegate to `composer_block_with_state(thinking, false)` so existing callers and tests stay byte-identical.

- [ ] **Step 4: Compose the activity row**

In `src/tui/ui.rs`, compute a safe activity height before splitting:

```rust
let activity_height = f.area().height.saturating_sub(7).min(6);
let rows = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
        Constraint::Min(1),
        Constraint::Length(activity_height),
        Constraint::Length(3),
        Constraint::Length(3),
    ])
    .split(f.area());
```

Render `app.activity_tail(rows[1].height as usize)` in `theme::pane("run activity")`. When the region has height, use `"waiting for run"` only when the activity deque is empty; never call a clock, git, or channel. Call `theme::composer_block_with_state(&app.thinking, app.run_mode == RunMode::Running || app.run_mode == RunMode::Stopping)` for the composer.

Import `RunMode` in `ui.rs`.

- [ ] **Step 5: Run the layout and replay tests**

Run:

```bash
cargo fmt -- --check
cargo test --test tui_app --test tui_render
cargo test --test tui_live
```

Expected: new activity/read-only tests, existing layout tests, and replay tests pass.

- [ ] **Step 6: Commit the live view**

```bash
git add src/tui/theme.rs src/tui/ui.rs src/tui/app.rs tests/tui_app.rs
git commit -m "feat(tui): render live activity region"
```

---

### Task 6: Wire the Worker into the Terminal Pump

**Files:**
- Modify: `src/tui/run.rs`
- Modify: `src/main.rs`
- Modify: `tests/tui_live.rs`

**Interfaces:**
- Consumes: `LiveSession`, `LiveEvent`, `TraceSink::attach_live/detach_live`, and `GoalRunner::run_live`.
- Produces: `run_live<F>(trace: &TraceSink, start_goal: F) -> anyhow::Result<()>` where
  `F: FnMut(String, UnboundedSender<LiveEvent>) -> anyhow::Result<WorkerHandle>`.
- Produces: `RunningKeyOutcome::{ReadOnlyNotice, StopArmed, Ignored}` and
  `pub fn handle_running_key(&mut App, code: KeyCode) -> RunningKeyOutcome`
  for deterministic running-mode key tests.
- Removes the old main-side `while let LiveOut::Goal` loop; one `run_live` call owns the whole live session.

- [ ] **Step 1: Write failing session-wiring tests**

Add a pure test for the running-mode key contract without a PTY:

```rust
use crossterm::event::KeyCode;
use rof::tui::app::App;
use rof::tui::run::{handle_running_key, RunningKeyOutcome};

#[test]
fn running_mode_does_not_dispatch_composer_text() {
    let mut app = App::new();
    app.begin_run("busy");
    app.input.push_str("/model provider/model");
    let outcome = handle_running_key(&mut app, KeyCode::Enter);
    assert_eq!(outcome, RunningKeyOutcome::ReadOnlyNotice);
    assert_eq!(app.input, "/model provider/model");
    assert!(app.transcript.iter().any(|line| line.contains("read-only")));
}
```

- [ ] **Step 2: Run the wiring test to verify it fails**

Run:

```bash
cargo test --test tui_live running_mode_does_not_dispatch_composer_text
```

Expected: FAIL because the running key path does not exist.

- [ ] **Step 3: Replace polling with the live session**

Change `run_live` to create one `(tx, rx)` channel, attach `tx.clone()` to the sink, construct `LiveSession::new(rx)`, and detach in a guard-like cleanup path before returning. The terminal setup/teardown must remain around `run_live_inner`, and every `?` path must still execute `disable_raw_mode` and `LeaveAlternateScreen`.

At the top of each loop iteration, call `session.drain(&mut app)`. Handle the terminal outcome before polling keys:

- a successful `Finished` outcome returns to `Idle` and keeps the activity pane;
- a failed outcome records the failure, returns to `Idle`, and does not start another goal automatically;
- a stop request after a completed outcome returns from the session.

- [ ] **Step 4: Add running-mode key semantics**

In the pump loop, if `session.is_running()`:

- ordinary characters and Backspace edit the draft only;
- Enter calls `app.transcript.push("run in progress — composer is read-only in P1a")` and does not call `cmd::parse` or `apply_action`;
- `q`/Escape/Ctrl-C on an empty composer calls `session.request_stop()` and `app.set_stopping()`;
- Up/Down/PageUp/PageDown/Home/End retain transcript scrolling.

Only the existing idle branch may call `apply_action`, verify login, or start a goal.

- [ ] **Step 5: Inject the starter from `main.rs`**

Change the chat arm to one call:

```rust
let s = setup(load_config(cfg_path.as_deref())?)?;
run_live(&s.trace, |goal, tx| {
    let runner = GoalRunner::from_setup(&s);
    Ok(tokio::spawn(async move { runner.run_live(goal, tx).await }))
})?;
Ok(())
```

Remove the old `while let LiveOut::Goal(g) = ... { run_goal_text(...).await?; }` loop. Leave the `--replay` arm unchanged.

- [ ] **Step 6: Run the live session and execution suites**

Run:

```bash
cargo fmt -- --check
cargo test --test tui_live --test tui_app --test tui_cmd --test tui_render
cargo test --bin rof goal_runner_reports_a_terminal_live_outcome
cargo test --test loop
```

Expected: all focused tests pass, and the existing loop/read-request behavior remains green.

- [ ] **Step 7: Commit the wired live monitor**

```bash
git add src/tui/run.rs src/main.rs tests/tui_live.rs
git commit -m "feat(chat): run goals beside the live terminal pump"
```

---

### Task 7: Integration Verification and Handoff

**Files:**
- Modify: `docs/STATUS.md`
- Test: `tests/tui_live.rs` and existing suites

**Interfaces:**
- Consumes: all P1a tasks above.
- Produces: a verified, documented read-only live monitor with no claim of P1b steering or P3 panes.

- [ ] **Step 1: Add the stub multi-round integration assertion**

Extend `tests/tui_live.rs` with a test that uses the existing `StubClient` and public `Orchestrator` directly (the binary-private `GoalRunner` is covered by its `main.rs` unit test):

```rust
#[tokio::test]
async fn stub_worker_delivers_the_same_trace_order() {
    use rof::config::AppConfig;
    use rof::engine::{Orchestrator, Session};
    use rof::llm::{ContextService, ExecutorService, StubClient};
    use rof::tools::ToolRegistry;
    use std::sync::Arc;

    let root = std::env::temp_dir().join(format!("rof-p1a-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let trace = Arc::new(TraceSink::new());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    trace.attach_live(tx.clone());
    let worker_trace = trace.clone();
    let worker_root = root.clone();
    let handle = tokio::spawn(async move {
        let cfg = AppConfig::default();
        let context = ContextService::new(Arc::new(StubClient), "context-stub".into());
        let executor = ExecutorService::new(Arc::new(StubClient), "executor-stub".into(), None);
        let verify = ExecutorService::new(Arc::new(StubClient), "verify-stub".into(), None);
        let registry = ToolRegistry::with_defaults(
            worker_root.clone(),
            cfg.permissions.clone(),
            cfg.skills.clone(),
        );
        let orch = Orchestrator::new(cfg, worker_trace, context, executor, verify);
        orch.run_loop(
            &Session::new("stub integration goal".into()),
            &registry,
            &worker_root,
        )
        .await
    });
    handle.await.unwrap();
    tx.send(LiveEvent::Boundary(Boundary::Finished)).unwrap();
    tx.send(LiveEvent::Finished(GoalFinished {
        passed: true,
        error: None,
    })).unwrap();

    let mut live = Vec::new();
    while let Ok(event) = rx.try_recv() {
        live.push(event);
    }
    let stored = trace.events();
    let live_traces: Vec<TraceEvent> = live
        .iter()
        .filter_map(|event| match event {
            LiveEvent::Trace(event) => Some(event.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        live_traces.iter().map(|e| serde_json::to_string(e).unwrap()).collect::<Vec<_>>(),
        stored.iter().map(|e| serde_json::to_string(e).unwrap()).collect::<Vec<_>>()
    );
    assert!(matches!(live.last(), Some(LiveEvent::Finished(_))));
    trace.detach_live();
    let _ = std::fs::remove_dir_all(root);
}
```

Use the temporary work root and `StubClient`; do not call a network model. The main-private `GoalRunner` wrapper is tested separately in Task 4.

- [ ] **Step 2: Run the integration test**

Run:

```bash
cargo test --test tui_live
```

Expected: PASS with the live trace order equal to the durable sink order.

- [ ] **Step 3: Run the complete phase gate**

Run:

```bash
cargo fmt -- --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
git diff --check
```

Expected: 0 test failures, 0 warnings under `-D warnings`, and no whitespace errors.

- [ ] **Step 4: Perform the terminal smoke**

From a real terminal, run a stub/offline session with provider variables removed:

```bash
env -u ROF_TOKEN -u OR_TOKEN -u ROF_CHAT_BASE \
  ROF_TRACE=/tmp/p1a-smoke-trace.jsonl \
  cargo run -- chat
```

Submit a short goal, confirm that activity appears while it runs, confirm Enter is visibly read-only, and quit with `q`/`Ctrl-C`. Repeat once with a forced worker error if the stub fixture supports it. Record the commands and observed result in `docs/STATUS.md`; do not claim a network/model result from this smoke.

- [ ] **Step 5: Update STATUS and commit the handoff**

Append a dated P1a entry to `docs/STATUS.md` containing:

- the live-channel and worker ownership change;
- the activity/read-only contract;
- the stub integration result;
- the exact test/Clippy counts;
- the remaining P1b–P4 work.

Then run:

```bash
git add docs/STATUS.md
git commit -m "docs: record P1a live monitor"
```

## Definition of Done

- A goal runs in a worker while the terminal pump renders live events.
- `TraceSink` and the live channel deliver the same ordered trace events.
- Activity is bounded and rendered only from `App`.
- Running-mode input cannot submit, dispatch, or mutate configuration.
- Normal completion, worker error, channel loss, and quit restore the terminal.
- Replay remains read-only and unchanged.
- `run` and `eval` retain their existing outputs and behavior.
- The two pre-existing Clippy warnings are fixed separately from feature work.
- Full formatting, tests, and Clippy-with-warnings-denied pass.
- STATUS records the measured P1a result without claiming P1b interaction.
