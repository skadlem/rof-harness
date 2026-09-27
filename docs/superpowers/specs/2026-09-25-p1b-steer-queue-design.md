# P1b steer and queue — design

Date: 2026-09-25. Status: approved design scope; implementation not started.

## Goal

Extend the P1a live monitor so the user can submit guidance and one queued
next goal while a run is in flight. Every submission is visible as pending and
receives an ordered applied/rejected acknowledgement. Nothing mutates the
current model call or tool invocation.

P1b also gives configuration/model commands an honest next-goal semantic:
they are deferred by the UI while a run is live, applied before the next goal
starts, and never silently applied underneath the current worker.

## Baseline

P1a is landed in `ef19b2a` and its preceding commits:

- `TraceSink` emits ordered `LiveEvent` notifications alongside durable trace
  records.
- `App` owns bounded live activity and `RunMode` state.
- `LiveSession` reduces worker events into `App`.
- `run_live` owns the alternate-screen pump and starts one `GoalRunner` task
  through an injected starter closure.
- `Orchestrator` already has identifiable implementer/reviewer round
  boundaries in both pipeline and direct loops.
- Replay is read-only and does not create a worker or command channel.

## Decisions

1. **Steer is the default busy mode.** `/busy queue` selects queue mode;
   `/busy steer` selects steer mode; `/busy interrupt` arms the existing
   stop latch.
2. **One pending slot per kind.** A newer steer replaces the pending steer;
   a newer queued goal replaces the pending goal. Replaced submissions are
   acknowledged as rejected with a `replaced` reason rather than silently
   disappearing.
3. **Commands are drained only at safe boundaries.** The worker drains the
   UI command inbox after a complete implementer/reviewer round and before
   the next implementer prompt. A command received during the final round
   is drained at the terminal boundary: steer is rejected because no next
   round exists; queue is retained for the next goal.
4. **Queue starts after the current goal's terminal outcome.** The worker
   sends a per-goal `GoalFinished` event, then starts the queued goal in the
   same task if one exists. It sends the session-terminal `Finished` event
   only when no queued goal remains.
5. **Configuration is deferred to the next goal.** The UI records
   configuration actions while running and applies them in order immediately
   before the next `GoalRunner` starts. If a goal is already queued, the
   configuration applies to the goal after that queued goal; the status line
   says so. `/login`, `/logout`, and provider mutations are rejected while a
   run is live because they carry credential or registry state rather than
   simple next-goal configuration.
6. **View-only commands remain immediate.** `/help`, `/hotkeys`, `/models`,
   `/context`, `/trace`, and unknown-command feedback update `App` without
   entering the worker inbox.
7. **No persistence or new provider UI.** P1b does not add a preferences
   file, completion, diff/provider panes, token deltas, or multi-session
   state.

## Protocol types

### Live acknowledgements

Extend `src/obs/live.rs` with:

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

Add `LiveEvent::Control(ControlAck)` and `LiveEvent::GoalFinished(GoalFinished)`.
`LiveEvent::Finished` remains the session-terminal outcome. The distinction
is required because a queued goal keeps the worker task alive after the
current goal's `GoalFinished` event.

### Worker commands

Create `src/engine/control.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunCommand {
    Steer { id: u64, text: String },
    QueueGoal { id: u64, goal: String },
    Stop,
}
```

`RunControl` owns the command receiver and the worker's pending slots:

```rust
pub struct RunControl {
    receiver: tokio::sync::mpsc::UnboundedReceiver<RunCommand>,
    pending_steer: Option<RunCommand>,
    pending_goal: Option<RunCommand>,
    stop_requested: bool,
    pending_acks: Vec<ControlAck>,
}

impl RunControl {
    pub fn new(receiver: UnboundedReceiver<RunCommand>) -> Self;
    pub fn drain_boundary(&mut self) -> Vec<ControlAck>;
    pub fn take_queued_goal(&mut self) -> Option<String>;
    pub fn pending_steer(&self) -> Option<&str>;
    pub fn stop_requested(&self) -> bool;
}
```

`drain_boundary` drains all currently available commands in channel order,
keeps only the latest command of each kind, and records an `Applied` ack for
the kept command and a `Rejected { note: "replaced" }` ack for each displaced
command. `Stop` sets `stop_requested` and acknowledges that the stop was
accepted; the next goal boundary uses that flag to drop any queued goal. A
terminal drain rejects a remaining steer with `"no next round"` and keeps a
queue command for the next goal only when stop was not requested.

### Boundary hook

Add an engine-level hook so the orchestrator does not depend on the TUI:

```rust
pub struct RunHooks<'a> {
    pub control: Option<&'a mut RunControl>,
    pub live: Option<&'a UnboundedSender<LiveEvent>>,
}
```

Add `Orchestrator::run_loop_with_hooks`; the existing `run_loop` delegates with
`RunHooks::none()` so `run`, `eval`, and all existing tests retain their
behavior.

At the end of each implementer/reviewer round, after checks, reviewer
verdict, and any rollback, and before the next implementer prompt:

1. call `hooks.control.drain_boundary()`;
2. send each returned `ControlAck` through the live sender;
3. if a steer survives, append it to the next round's existing feedback
   context with a clear `USER STEER:` marker;
4. leave a queue command in the control object for the goal loop.

Direct mode and pipeline mode call the same hook. The hook is a no-op for
`run_loop` callers that do not supply one.

## Worker/goal lifecycle

`run_live` creates one command channel for the whole interactive session and
passes its receiver to the injected starter:

```rust
pub fn run_live<F>(
    trace: &TraceSink,
    start_goal: F,
) -> anyhow::Result<()>
where
    F: FnMut(
        String,
        UnboundedSender<LiveEvent>,
        UnboundedReceiver<RunCommand>,
    ) -> anyhow::Result<WorkerHandle>;
```

`GoalRunner::run_live` becomes a session loop rather than a single goal call:

1. take the initial goal and `RunControl` receiver;
2. send `Boundary::Started` and the live sink remains attached;
3. run `execute_with_control(goal, &mut control)`;
4. send `LiveEvent::GoalFinished` for that goal;
5. drain terminal commands once (steer is rejected, queue is retained
   unless `stop_requested` is set, in which case queue is rejected with
   `stopped`);
6. if `control.take_queued_goal()` returns a goal, repeat from step 2 with
   freshly rebuilt services;
7. otherwise send `LiveEvent::Finished` and return.

The live sender is never reattached between goals. The trace sink remains the
single durable source, and the same `RunControl` receiver carries commands
across goal boundaries.

A queue goal is started exactly once: `take_queued_goal` removes it before
the next `execute`. If a second queue command arrives before that start, the
latest replaces the pending slot and the earlier command is rejected.

If the user requests stop while a queue is pending, the queue is dropped and
its command receives a rejected acknowledgement with `stopped` unless the
user has already force-exited. A force exit still ends the pump and detaches
the worker; the P1a limitation about an in-flight tool child still applies.

## UI state and input

Add to `App`:

```rust
pub enum BusyMode {
    Steer,
    Queue,
    Interrupt,
}

pub struct PendingControl {
    pub id: u64,
    pub text: String,
}

pub enum DeferredConfig {
    Attempts(u8),
    Rounds(u32),
    Thinking(String),
    Effort(String),
    Caps(usize, usize),
    Model { slot: Option<String>, value: String },
}

pub busy_mode: BusyMode,
pub pending_steer: Option<PendingControl>,
pub pending_goal: Option<PendingControl>,
pub deferred_config: Vec<DeferredConfig>,
pub last_control_ack: Option<ControlAck>,
```

`DeferredConfig` is a non-secret, already-validated action representation
for attempts, rounds, thinking, effort, caps, and model selection. It contains
no credential text.

While a worker is running, Enter behaves as follows:

- text plus `BusyMode::Steer`: send `RunCommand::Steer`, store
  `pending_steer`, clear the composer, and show `steer pending (id)`;
- text plus `BusyMode::Queue`: send `RunCommand::QueueGoal`, store
  `pending_goal`, clear the composer, and show `goal queued (id)`;
- `/busy steer|queue|interrupt`: apply immediately to `App`; interrupt calls
  the existing stop request path and sends `RunCommand::Stop` so a queued
  goal is dropped at the next boundary;
- view-only slash commands: execute immediately;
- deferred configuration commands: append to `deferred_config` and show
  `applies to next goal`;
- `/login`, `/logout`, and provider mutations: reject with
  `available between goals`, without changing credentials or config;
- the first q/Esc/Ctrl-C stop key also sends `RunCommand::Stop`; the
  existing P1a second-key force-exit behavior is unchanged;
- empty text: no-op.

A new submission replaces the corresponding pending slot in `App` and on the
worker side. The status/composer chrome shows busy mode, pending steer/goal,
deferred configuration count, and the last acknowledgement. `ControlAck`
updates clear the matching pending slot and add one visible transcript line.

`GoalFinished` returns the run pane to a settled state while the worker
remains present if a queue continues. `App::on_live_boundary(Started)`
consumes the local `pending_goal` slot when one exists, calls
`begin_run` with that goal, and clears the previous activity view. The
initial goal still calls `begin_run` in the pump before its starter runs.

## Deferred configuration ordering

Configuration actions are applied in this order immediately before a new
`GoalRunner` is created:

1. drain `App.deferred_config` in submission order;
2. apply each through the existing `apply_action`/env behavior;
3. clear the list;
4. start the goal.

If a queue goal is already registered, configuration submitted after that
queue is applied before the goal after the queued one. The UI says
`applies after queued goal` rather than implying immediate effect. No
configuration is forwarded to the current worker.

## Error handling

| Failure | Behavior |
|---|---|
| Command channel closed | Worker finishes current goal; queued commands are not invented |
| Worker drops the command receiver | Current call completes; pending UI slots show rejected on the next pump tick |
| Steer received at terminal boundary | Rejected ack `no next round` |
| Queue received at terminal boundary | Retained and started after the current `GoalFinished` |
| Configuration rejected | Transcript and status show the exact reason; env/config unchanged |
| Login/provider command while live | Rejected; no secret enters `App` or the channel |
| Stop with pending queue | Queue dropped with `stopped` ack unless force-exit already ended the session |
| Worker error | Existing generic failure outcome; terminal restoration unchanged |

## Testing strategy

### Control and boundary tests

- Command drain preserves submission order.
- Latest steer/queue replaces the earlier slot and rejects the displaced
  command.
- Steer is absent from the current round and present in the next round prompt.
- A terminal steer is rejected; a terminal queue survives.
- Queue is taken exactly once and starts a freshly built goal.
- Acks are ordered and terminal outcomes are not confused with per-goal
  `GoalFinished` events.

### App/UI tests

- Enter in steer mode creates a pending slot and does not start a second
  goal.
- Enter in queue mode creates a pending goal.
- `/busy` switches mode and updates the composer title.
- Config commands while live are visibly deferred and apply to the next
  goal only.
- Rejected config/login/provider commands leave state unchanged.
- TestBackend rendering shows busy mode, pending slots, ack, and deferred
  configuration without leaking secret text.
- Existing P1a activity, stop, force-exit, and replay tests remain green.

### Integration tests

Use `StubClient` and a temporary root to run two goals through the same
worker session. Assert the first `GoalFinished`, queued-goal
`Boundary::Started`, second `GoalFinished`, and terminal `Finished`, with
steer visible only in the second goal's prompt context. No network, PTY, or
credential store access is required.

Run formatting, the full suite, and Clippy with warnings denied before the
phase is accepted.

## Files

- Create: `src/engine/control.rs`
- Modify: `src/obs/live.rs`
- Modify: `src/engine/orchestrator.rs`
- Modify: `src/main.rs`
- Modify: `src/tui/app.rs`
- Modify: `src/tui/cmd.rs`
- Modify: `src/tui/run.rs`
- Modify: `src/tui/ui.rs`
- Modify: `src/tui/theme.rs` if the busy/pending composer title needs a
  non-breaking helper
- Create/modify: `tests/tui_live.rs`, `tests/tui_app.rs`
- Update: `docs/STATUS.md` after the phase gate

No new dependency is required; Tokio, Ratatui, Crossterm, and Serde are
already present.

## Non-goals

- Token-delta streaming.
- Multiple simultaneous runs or multi-session management.
- Diff/provider panes, themes, completion, or preference persistence.
- Credential capture or provider mutation while a run is live.
- Remote attachment, browser UI, or a debugger.
- Persisting pending steer, queue, or configuration across restarts.
