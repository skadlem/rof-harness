# P1b Steer and Queue Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add boundary-safe mid-run steering, one queued next goal, ordered command acknowledgements, and honest next-goal configuration deferral to the P1a live console.

**Architecture:** A UI-to-worker `UnboundedReceiver<RunCommand>` is created once per live session. `RunControl` drains it only at orchestrator round boundaries, returns a `BoundaryBatch` of the latest steer/queue slots plus ordered `ControlAck` values, and the `GoalRunner` loop starts at most one queued goal after the current goal's per-goal outcome. `App` owns busy mode, pending slots, deferred non-secret configuration, and the last acknowledgement; `draw(&App)` remains pure.

**Tech Stack:** Rust, Tokio unbounded channels/tasks, Ratatui, Crossterm, Serde, existing `Orchestrator`, `GoalRunner`, `LiveSession`, `TraceSink`, and StubClient. No new dependency.

## Global Constraints

- A command must never mutate an in-flight model call or tool invocation.
- Steer and queue each have exactly one pending slot; the latest submission replaces the earlier one and the displaced command receives a rejection acknowledgement.
- Commands drain only after a complete implementer/reviewer round and before the next implementer prompt; a terminal drain rejects a remaining steer and retains a queue unless stop was requested.
- `LiveEvent::GoalFinished` is a per-goal outcome; `LiveEvent::Finished` ends the interactive session and is the only event that releases `LiveSession`'s worker handle.
- Configuration/model commands are deferred to the next goal and never sent to the current worker. Login/logout/provider mutations are rejected while live and never place secrets in `App` or a channel.
- View-only slash commands remain immediate. P1a stop, force-exit, terminal restoration, replay read-only behavior, and P1a activity bounds must remain intact.
- Every task follows red-green-refactor, and every phase gate runs `cargo fmt -- --check`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Do not add persistence, diff/provider panes, token deltas, multi-session state, or a new dependency.

---

## File Map

- Create `src/engine/control.rs`: `RunCommand`, `BoundaryBatch`, `RunControl`, and `RunHooks`.
- Modify `src/obs/live.rs`: acknowledgement types and `Control`/`GoalFinished` live events.
- Modify `src/engine/mod.rs`: export the control module.
- Modify `src/engine/orchestrator.rs`: `run_loop_with_hooks` and boundary draining in both execution modes.
- Modify `src/main.rs`: `GoalRunner` session loop and command-receiver injection.
- Modify `src/tui/app.rs`: `BusyMode`, `DeferredConfig`, pending slots, acknowledgements, and boundary state.
- Modify `src/tui/cmd.rs`: `/busy` help/semantics needed by the running composer.
- Modify `src/tui/run.rs`: command channel, running composer submission, deferred-config routing, and worker continuation.
- Modify `src/tui/theme.rs`: busy/pending composer title helper while preserving P1a `composer_block`.
- Modify `src/tui/ui.rs`: busy/pending status and composer rendering.
- Create `tests/engine_control.rs`: pure `RunControl` ordering/replacement tests.
- Modify `tests/tui_live.rs`: App/UI control, worker continuation, and end-to-end stub tests.
- Modify `tests/tui_app.rs`: TestBackend pending/busy/deferred-config rendering.
- Modify `docs/STATUS.md`: measured P1b handoff.

---

### Task 1: Add the Live Command Protocol and App State

**Files:**
- Modify: `src/obs/live.rs`
- Modify: `src/tui/app.rs`
- Modify: `tests/tui_live.rs`

**Interfaces:**
- Produces `obs::{ControlKind, ControlStatus, ControlAck}`.
- Extends `LiveEvent` with `Control(ControlAck)` and `GoalFinished(GoalFinished)`.
- Produces `tui::app::{BusyMode, PendingControl, DeferredConfig}` and App methods for pending state, acknowledgements, and boundary transitions.

- [ ] **Step 1: Write failing App/protocol tests**

Add to `tests/tui_live.rs`:

```rust
#[test]
fn app_tracks_pending_control_and_acknowledges_it() {
    let mut app = App::new();
    app.begin_run("first");
    app.set_busy_mode(BusyMode::Queue);
    let id = app.submit_pending_goal("second goal");
    assert_eq!(app.pending_goal.as_ref().map(|p| p.id), Some(id));
    assert!(app.control_summary().contains("queued"));

    app.on_control_ack(ControlAck {
        id,
        kind: ControlKind::Queue,
        status: ControlStatus::Applied,
        note: "queued".into(),
    });
    assert!(app.pending_goal.is_none());
    assert!(app.transcript.iter().any(|line| line.contains("queued")));
}

#[test]
fn started_boundary_consumes_only_a_pending_queued_goal() {
    let mut app = App::new();
    app.begin_run("first");
    app.submit_pending_goal("second goal");
    app.on_live_boundary(Boundary::Started);
    assert_eq!(app.run_goal, "second goal");
    assert!(app.pending_goal.is_none());
    assert_eq!(app.run_mode, RunMode::Running);
}
```

The tests must initially fail to compile because the new types and methods do not exist.

- [ ] **Step 2: Run the red tests**

Run:

```bash
cargo test --test tui_live
```

Expected: compile errors naming the missing control protocol and App methods.

- [ ] **Step 3: Extend the live protocol**

In `src/obs/live.rs`, add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Steer,
    Queue,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlStatus {
    Applied,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlAck {
    pub id: u64,
    pub kind: ControlKind,
    pub status: ControlStatus,
    pub note: String,
}
```

Add `Control(ControlAck)` and `GoalFinished(GoalFinished)` to `LiveEvent`. Keep
`Finished(GoalFinished)` as the session-terminal variant. Re-export the new
types from `src/obs/mod.rs`.

- [ ] **Step 4: Add App control state**

In `src/tui/app.rs`, add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyMode {
    Steer,
    Queue,
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingControl {
    pub id: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeferredConfig {
    Attempts(u8),
    Rounds(u32),
    Thinking(String),
    Effort(String),
    Caps(usize, usize),
    Model { slot: Option<String>, value: String },
}
```

Add `busy_mode`, `pending_steer`, `pending_goal`, `deferred_config`,
`next_control_id`, and `last_control_ack` to `App`. Initialize them without
changing the existing replay defaults. Add:

- `set_busy_mode(BusyMode)`;
- `submit_pending_steer(&str) -> u64` and `submit_pending_goal(&str) -> u64`,
  replacing the matching slot and returning the new id;
- `defer_config(DeferredConfig)`;
- `on_control_ack(ControlAck)`, clearing the matching pending slot and
  appending a concise transcript line;
- `control_summary() -> String`, returning a compact pending/busy/deferred
  description for the status row;
- updated `on_live_boundary(Boundary::Started)` that consumes a pending
  queued goal, calls `begin_run` for it, and otherwise only marks the run
  running;
- `on_goal_finished(&GoalFinished)` as the per-goal form of the existing
  outcome reducer, while the existing `on_live_finished` remains usable for
  the session-terminal P1a path.

- [ ] **Step 5: Run focused tests and existing P1a tests**

Run:

```bash
cargo fmt -- --check
cargo test --test tui_live --test tui_app
cargo test --lib
```

Expected: the new tests and all existing P1a reducer/replay tests pass.

- [ ] **Step 6: Commit the protocol/state layer**

```bash
git add src/obs/live.rs src/obs/mod.rs src/tui/app.rs tests/tui_live.rs
git commit -m "feat(tui): add live control state"
```

---

### Task 2: Add `RunControl` and the Engine Boundary Hook

**Files:**
- Create: `src/engine/control.rs`
- Modify: `src/engine/mod.rs`
- Modify: `src/engine/orchestrator.rs`
- Create: `tests/engine_control.rs`
- Modify: `tests/loop.rs` only for the boundary-prompt regression if the existing fake client is reused.

**Interfaces:**
- Produces `engine::control::{RunCommand, BoundaryBatch, RunControl, RunHooks}`.
- Produces `Orchestrator::run_loop_with_hooks`; existing `run_loop` remains the no-hook wrapper.
- Consumes `obs::{ControlAck, ControlKind, ControlStatus, LiveEvent}`.

- [ ] **Step 1: Write failing pure control tests**

Create `tests/engine_control.rs` with tests for:

```rust
#[tokio::test]
async fn control_drain_keeps_latest_and_rejects_replacements() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut control = RunControl::new(rx);
    tx.send(RunCommand::Steer { id: 1, text: "old".into() }).unwrap();
    tx.send(RunCommand::Steer { id: 2, text: "new".into() }).unwrap();
    tx.send(RunCommand::QueueGoal { id: 3, goal: "first queued".into() }).unwrap();
    tx.send(RunCommand::QueueGoal { id: 4, goal: "latest queued".into() }).unwrap();

    let batch = control.drain_boundary(false);
    assert_eq!(batch.steer.as_ref().map(|s| s.text.as_str()), Some("new"));
    assert_eq!(batch.goal.as_ref().map(|g| g.goal.as_str()), Some("latest queued"));
    assert!(batch.acks.iter().any(|a| a.id == 1 && a.status == ControlStatus::Rejected));
    assert!(batch.acks.iter().any(|a| a.id == 3 && a.status == ControlStatus::Rejected));
    assert_eq!(batch.acks.iter().filter(|a| a.status == ControlStatus::Applied).count(), 2);
}

#[tokio::test]
async fn terminal_drain_rejects_steer_but_keeps_queue() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut control = RunControl::new(rx);
    tx.send(RunCommand::Steer { id: 1, text: "too late".into() }).unwrap();
    tx.send(RunCommand::QueueGoal { id: 2, goal: "next".into() }).unwrap();
    let batch = control.drain_boundary(true);
    assert!(batch.steer.is_none());
    assert!(batch.acks.iter().any(|a| a.id == 1 && a.note.contains("no next round")));
    assert_eq!(control.take_queued_goal().map(|g| g.goal), Some("next".into()));
}
```

Also test `Stop`: after sending `RunCommand::Stop { id: 5 }`,
`stop_requested()` is true
and the control drops a pending queue at the next terminal boundary.

- [ ] **Step 2: Run the red control tests**

Run:

```bash
cargo test --test engine_control
```

Expected: compilation fails because `RunControl` and its types do not exist.

- [ ] **Step 3: Implement `RunControl`**

Create `src/engine/control.rs` with these public command types:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunCommand {
    Steer { id: u64, text: String },
    QueueGoal { id: u64, goal: String },
    Stop { id: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryBatch {
    pub steer: Option<(u64, String)>,
    pub goal: Option<(u64, String)>,
    pub acks: Vec<ControlAck>,
}
```

Add private pending slots and `RunControl`. `drain_boundary(terminal)` must
drain `try_recv()` in channel order, keep the latest steer/goal, emit ordered
`ControlAck` values, reject a terminal steer with `no next round`, and retain
a queue only when `stop_requested()` is false. `take_queued_goal()` removes and
returns the pending goal exactly once.

- [ ] **Step 4: Implement `RunHooks`**

Define:

```rust
pub struct RunHooks<'a> {
    pub control: Option<&'a mut RunControl>,
    pub live: Option<&'a tokio::sync::mpsc::UnboundedSender<LiveEvent>>,
}
```

Provide `RunHooks::none()` and `apply_boundary(&mut self, feedback: &mut String,
terminal: bool)`. It drains the control, sends each ack as
`LiveEvent::Control`, and appends a surviving steer as
`USER STEER: {text}` to the feedback string that the next implementer prompt
already consumes.

- [ ] **Step 5: Add the orchestrator hook**

Add `run_loop_with_hooks(&mut self, session, tools, workdir, hooks)` and make
the existing `run_loop` call it with `RunHooks::none()`. In both pipeline and
direct loops, call `hooks.apply_boundary(&mut feedback, false)` after the
reviewer/rollback work and before incrementing to the next round. Ensure the
terminal path calls `apply_boundary(&mut feedback, true)` before returning, including
early error returns.

- [ ] **Step 6: Add the boundary prompt regression**

Extend the existing deterministic loop fake so a first-round steer submitted
before the second round appears in the second implementer prompt and not in the
first. The test must use `StubClient`/the existing fake client only, no network.

- [ ] **Step 7: Run engine gates**

```bash
cargo fmt -- --check
cargo test --test engine_control --test loop
cargo test --lib
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 8: Commit the boundary hook**

```bash
git add src/engine/control.rs src/engine/mod.rs src/engine/orchestrator.rs tests/engine_control.rs tests/loop.rs
git commit -m "feat(engine): add boundary control hook"
```

---

### Task 3: Make the Worker a Two-Goal Session

**Files:**
- Modify: `src/main.rs`
- Modify: `src/tui/run.rs`
- Modify: `src/tui/app.rs` only if per-goal outcome state needs a small method.
- Modify: `tests/tui_live.rs`
- Modify: the `main.rs` unit test module.

**Interfaces:**
- `run_live` starter receives `(String, UnboundedSender<LiveEvent>, UnboundedReceiver<RunCommand>)`.
- `GoalRunner::run_live(goal, live_tx, command_rx)` loops goals.
- `LiveSession` distinguishes `GoalFinished` from session-terminal `Finished`.

- [ ] **Step 1: Write failing continuation tests**

Add a deterministic test that starts a worker with a command channel, sends
a queue command, collects the live events, and asserts this exact ordering:
`Boundary::Started`, first `GoalFinished`, second `Boundary::Started`, second
`GoalFinished`, then `Finished`. Count the per-goal outcomes with:

```rust
let goal_finishes = events
    .iter()
    .filter(|event| matches!(event, LiveEvent::GoalFinished(_)))
    .count();
assert_eq!(goal_finishes, 2);
assert!(matches!(events.first(), Some(LiveEvent::Boundary(Boundary::Started))));
assert!(matches!(events.last(), Some(LiveEvent::Finished(_))));
```

Use `StubClient` and a two-round configuration so the queue command is drained
at a real boundary. Add a test that `RunCommand::Stop` drops the queued goal
and does not start a second worker.

- [ ] **Step 2: Run the red continuation tests**

```bash
cargo test --test tui_live
cargo test --bin rof
```

Expected: compile failures for the new starter signature and `GoalFinished`
handling.

- [ ] **Step 3: Change the session starter signature**

In `src/tui/run.rs`, create one `(command_tx, command_rx)` channel in
`run_live` and pass both the live sender and command receiver to the injected
starter. Change the starter bound to:

```rust
F: FnMut(
    String,
    UnboundedSender<LiveEvent>,
    UnboundedReceiver<RunCommand>,
) -> anyhow::Result<WorkerHandle>
```

- [ ] **Step 4: Implement the `GoalRunner` loop**

In `src/main.rs`, add `execute_with_control(goal, &mut RunHooks)` and make
`run_live` loop over the initial goal and `control.take_queued_goal()`. For each
goal:

1. send `Boundary::Started`;
2. execute the existing goal body with the hook;
3. send `LiveEvent::GoalFinished`;
4. if stop was requested, reject/drop the queue;
5. otherwise take the queue goal and rebuild services for the next iteration;
6. send `LiveEvent::Finished` only when no queue remains.

- [ ] **Step 5: Update `LiveSession` state transitions**

`LiveSession::drain` must route `LiveEvent::Control` to
`App::on_control_ack` and `LiveEvent::GoalFinished` to the per-goal App
reducer, while keeping the worker handle. Only `LiveEvent::Finished` clears
the worker handle. `App::on_live_boundary(Started)` consumes the pending
queued goal and calls `begin_run` for it.

- [ ] **Step 6: Run the focused suite**

```bash
cargo fmt -- --check
cargo test --test tui_live --test tui_app
cargo test --bin rof
cargo test --test loop
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 7: Commit the worker loop**

```bash
git add src/main.rs src/tui/run.rs src/tui/app.rs tests/tui_live.rs
git commit -m "feat(chat): continue queued goals in one worker"
```

---

### Task 4: Route Running Composer Input and Deferred Configuration

**Files:**
- Modify: `src/tui/run.rs`
- Modify: `src/tui/cmd.rs`
- Modify: `src/tui/app.rs`
- Modify: `tests/tui_live.rs`

**Interfaces:**
- Produces `pub enum RunningSubmit { Sent(ControlKind), Deferred, Rejected(String), Ignored }`.
- Produces `pub fn submit_running_input(&mut App, &UnboundedSender<RunCommand>, &str) -> RunningSubmit`.
- Produces `pub enum RunningActionOutcome { View, Deferred, Rejected(String), BusyMode(BusyMode), Stop }` and `handle_running_action(&mut App, Action, &UnboundedSender<RunCommand>, &str) -> RunningActionOutcome`.
- Keeps the P1a `handle_running_key` stop/read-only contract intact.
- Uses `RunCommand::{Steer, QueueGoal, Stop}` and the new `App` state.

- [ ] **Step 1: Write failing running-input tests**

Add these concrete no-PTY tests:

```rust
#[test]
fn running_steer_submit_is_pending_and_clears_draft() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new();
    app.begin_run("busy");
    app.set_busy_mode(BusyMode::Steer);
    assert_eq!(submit_running_input(&mut app, &tx, "focus on the parser"), RunningSubmit::Sent(ControlKind::Steer));
    assert!(matches!(rx.try_recv(), Ok(RunCommand::Steer { .. })));
    assert!(app.input.is_empty());
    assert!(app.pending_steer.is_some());
}

#[test]
fn running_queue_submit_is_pending_and_clears_draft() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new();
    app.begin_run("busy");
    app.set_busy_mode(BusyMode::Queue);
    assert_eq!(submit_running_input(&mut app, &tx, "next goal"), RunningSubmit::Sent(ControlKind::Queue));
    assert!(matches!(rx.try_recv(), Ok(RunCommand::QueueGoal { .. })));
    assert!(app.input.is_empty());
    assert!(app.pending_goal.is_some());
}
```

Add a third test that calls `handle_running_action` with
`Action::Attempts(2)` and asserts `Deferred` plus an increased
`deferred_config`, then with `Action::Login(None)` and asserts `Rejected`
with `available between goals`. Add a fourth test that calls
`handle_running_action` with `Action::Busy("queue")` and asserts the App mode
changes without sending a worker command. Use an unbounded command channel
and a real `App`; no PTY.

- [ ] **Step 2: Run the red tests**

```bash
cargo test --test tui_live
```

- [ ] **Step 3: Implement running submissions**

In `run.rs`, keep the running branch before the P1a stop/draft handling. On
Enter:

- trim but do not dispatch an empty string;
- send `RunCommand::Steer` or `RunCommand::QueueGoal` according to
  `App::busy_mode`;
- record the returned id in the matching App slot;
- clear the composer only after a successful channel send;
- show `steer pending (id)` or `goal queued (id)`.

On the first q/Esc/Ctrl-C stop key, also send `RunCommand::Stop { id }` using
the next App control id; preserve the
P1a second-key force-exit behavior and final draw.

- [ ] **Step 4: Implement action routing**

Classify parsed actions while running:

- view-only: apply immediately to App;
- `Busy`: set `BusyMode` or arm stop;
- attempts/rounds/thinking/effort/caps/model: convert to
  `DeferredConfig` and append an `applies to next goal` transcript line;
- login/logout/provider add/rm: reject with `available between goals`;
- retry/approve/reject: keep the existing explicit unavailable response.

Before the pump starts a goal (idle or queued continuation), apply
`deferred_config` in order through the existing action/env path and clear it.
If a queue is already pending, label the message `applies after queued goal`.

> **Superseded during implementation.** A queued goal is started by the
> worker task, not by the pump, so the console can never run code "just
> before" it — the env write happens at submission instead, and the queued
> goal picks the setting up. The label is `applies to the next goal` in every
> case. See the design doc's "Deferred configuration ordering" section.

- [ ] **Step 5: Update `/busy` help/semantics**

Keep the closed `Action::Busy` parser. Change the help/status copy so it says
`steer` is default, `queue` stores one next goal, and `interrupt` arms the stop
path. Do not expose credentials in the message.

- [ ] **Step 6: Run focused tests and gates**

```bash
cargo fmt -- --check
cargo test --test tui_live --test tui_app --test tui_cmd
cargo test --test loop
cargo test --lib
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 7: Commit input routing**

```bash
git add src/tui/run.rs src/tui/cmd.rs src/tui/app.rs tests/tui_live.rs
git commit -m "feat(tui): route live steer and queue input"
```

---

### Task 5: Render Busy, Pending, and Deferred State

**Files:**
- Modify: `src/tui/theme.rs`
- Modify: `src/tui/ui.rs`
- Modify: `src/tui/app.rs`
- Modify: `tests/tui_app.rs`

**Interfaces:**
- Consumes `App::{busy_mode, pending_steer, pending_goal, deferred_config, control_summary, last_control_ack}`.
- Preserves P1a `composer_block(&str)` and the read-only title behavior.

- [ ] **Step 1: Write failing TestBackend tests**

Add tests for:

- running composer shows `steer` or `queue` mode;
- pending steer/goal ids are visible;
- deferred configuration count is visible;
- rejected login/provider text is visible without a key;
- idle and replay status remain unchanged.

- [ ] **Step 2: Run the red rendering tests**

```bash
cargo test --test tui_app
```

- [ ] **Step 3: Add a live composer title helper**

Keep `composer_block(thinking)` unchanged. Add a helper that accepts the
thinking label, busy mode label, and pending summary, producing a title such as
`composer · queue · goal queued (7)` while the existing P1a read-only helper
continues to work for the no-control case.

- [ ] **Step 4: Extend pure UI rendering**

In `ui.rs`, use the App control summary in the status row and live composer
title. Do not read env, channels, git, or credentials. Keep the activity pane
and P1a small-terminal geometry unchanged.

- [ ] **Step 5: Run rendering and regression tests**

```bash
cargo fmt -- --check
cargo test --test tui_app --test tui_render --test tui_live
cargo test --lib
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 6: Commit the control-aware view**

```bash
git add src/tui/theme.rs src/tui/ui.rs src/tui/app.rs tests/tui_app.rs
git commit -m "feat(tui): show live control state"
```

---

### Task 6: End-to-End Stub Verification, PTY Smoke, and STATUS

**Files:**
- Modify: `tests/tui_live.rs`
- Modify: `docs/STATUS.md`

**Interfaces:**
- Consumes the complete P1b worker/control/render contract.
- Produces measured evidence and the P1b handoff.

- [ ] **Step 1: Add the two-goal stub integration test**

Use a scripted `StubClient` fixture that forces a boundary, submits a steer
before the second round, submits a queued goal, and records the prompt seen by
each implementer call. Assert:

1. the first prompt has no `USER STEER`;
2. the second prompt has exactly the submitted steer text;
3. the current goal emits `GoalFinished` before the queued goal's `Started`;
4. the second goal emits its own `GoalFinished`;
5. the session ends with `Finished`;
6. control acknowledgements are ordered and the queue is started once.

No network, PTY, or credential store access.

- [ ] **Step 2: Run the integration test**

```bash
cargo test --test tui_live
```

- [ ] **Step 3: Run the full phase gate**

```bash
cargo fmt -- --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
git diff --check
```

Expected: all tests pass, Clippy has zero warnings, and no whitespace errors.

- [ ] **Step 4: Run the offline PTY smoke**

Use a real PTY with a 24x100 terminal, a temp work root, empty
`ROF_CREDENTIALS`, and `StubClient`. Submit a goal, submit a steer while it
runs, switch to `/busy queue`, submit a second goal, and confirm the final
transcript shows pending/applied/queued acknowledgements and both goal
outcomes. Exit with the P1a stop path; record the exact command and result.
Do not use a network model or claim a TB result.

- [ ] **Step 5: Update STATUS and commit**

Append a dated P1b entry containing the live command protocol, boundary
semantics, next-goal config behavior, test/Clippy counts, PTY result, and
remaining P3/P4 work. Then:

```bash
git add tests/tui_live.rs docs/STATUS.md
git commit -m "docs: record P1b steer queue handoff"
```

## Definition of Done

- A live composer submission creates exactly one pending steer or queue slot.
- Commands cannot affect the current in-flight model/tool call.
- Acks are ordered and distinguish applied, rejected, replaced, and stopped.
- Steer reaches only the next implementer prompt.
- A queued goal starts once, after the current per-goal outcome, with fresh
  services.
- Deferred configuration applies only to the next goal and is visible before
  application; credential/provider mutations are rejected while live.
- P1a stop/force-exit/terminal restoration and replay remain green.
- Full formatting, tests, and Clippy-with-warnings-denied pass.
- STATUS records measured P1b evidence without claiming P3 panes or persistence.
