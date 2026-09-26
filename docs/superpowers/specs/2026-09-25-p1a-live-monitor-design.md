# P1a live read-only monitor — design

Date: 2026-09-25. Status: approved design slice; implementation not started.

## Purpose

Make `rof chat` observe one active goal while the terminal event pump remains
responsive. This slice proves the risky concurrency seam before the TUI is
allowed to steer runs, queue goals, or mutate configuration.

The shipped result is a read-only live monitor: ordered run activity appears
while the worker executes, the existing transcript and replay behavior remain
intact, and the terminal is restored on every exit path.

## Baseline

The current live console owns `App` and polls `TraceSink::events()`, but only
while it is waiting at the idle prompt. `main.rs` receives a queued goal from
`run_live`, awaits `run_goal_text` to completion, and only then re-enters the
pump. The orchestrator already emits the ordered `TraceEvent` values a live
view needs, and `App::on_event` is the existing transcript/counter reducer.

## Goals

1. Run one goal in a Tokio worker while the alternate-screen pump keeps
   drawing and reading keys.
2. Deliver trace events, lifecycle boundaries, and the terminal outcome in
   order through one live channel.
3. Show a bounded, read-only run-activity region and honest run state.
4. Preserve tailed/scrolled transcript behavior as new events arrive.
5. Preserve `run`, `eval`, and replay behavior.
6. Restore the terminal after success, worker error, channel loss, and quit.

## Non-goals

- Composer submission, steering, or queued goals.
- Slash-command dispatch or configuration changes while a goal is running.
- Per-implementer/reviewer round command boundaries.
- Token-delta transport.
- Diff and provider panes.
- Focus cycling, themes, completion, or preference persistence.
- Transcript, draft, queue, or credential persistence.

Those remain P1b–P4 in
`docs/superpowers/plans/2026-09-25-full-tui.md`.

## Chosen architecture

### One ordered live channel

Add a live-notification type beside the trace contract:

```text
LiveEvent
  Trace(TraceEvent)
  Boundary(Boundary)
  Finished(GoalFinished)
```

`TraceSink` owns an optional unbounded live sender. `TraceSink::emit` remains
the only event-emission seam:

1. acquire the sink's emission lock;
2. update token totals, JSONL output, and in-memory events as it does today;
3. forward `LiveEvent::Trace(event.clone())` when a live sender is attached;
4. release the lock.

The emission lock makes sink order and channel order the same order even when
an emitter is concurrent. A closed or detached channel is not an error; the
durable trace remains authoritative. No existing caller changes.

The live session attaches one sender before starting a goal and detaches it
after the goal reaches a terminal state. Replay, `run`, and eval never attach
one and therefore remain byte-for-byte unchanged on the wire and in the sink.

### Goal worker ownership

`main.rs` gets a clonable `GoalRunner` built from the existing setup data:
config, trace, and work root. Each invocation applies the current environment
knobs and stored logins, then rebuilds the context/executor/verifier
services exactly as the existing between-goals path does; it must not
snapshot the setup services, because `/model`, `/login`, and other
configuration are intentionally applied on the next goal. The worker also
builds a fresh `ToolRegistry` inside each invocation, so no new `Clone`
implementation is required for the tool registry.

The worker:

1. sends `Boundary::Started`;
2. runs the existing orchestrator loop with the existing checks and write
   gate;
3. converts the JSON result to a small `GoalFinished { passed, error }`;
4. sends `Boundary::Finished` and then `Finished`;
5. returns.

`GoalRunner` execution is silent with respect to stdout. The `run` command
keeps its existing result/report wrapper. The live chat records the concise
outcome in `App`; the full event evidence remains in `TraceSink` and its
JSONL file. This prevents worker prints from corrupting the alternate screen.

`Boundary` is goal-scoped in P1a. There is no per-round boundary
acknowledgement yet because P1a has no command that can be applied at one.

### Terminal session state machine

`src/tui/run.rs` keeps the terminal lifecycle and owns two modes:

- `Idle`: the existing prompt, slash actions, provider login, and one-slot
  goal queue.
- `Running`: a goal worker and a live-event receiver are active.

The idle Enter path injects a goal starter into the session. The starter
returns a worker handle and the live receiver remains owned by the pump. The
pump drains the receiver on every iteration whether or not a key arrives.

`LiveSession` is a small pure state holder. It owns the current mode, the
worker handle, the live receiver, and quit/stop intent. Rendering still
receives only `&App`; no channel, clock, environment, or worker state is read
from `draw`.

While running:

- characters, Backspace, and transcript scrolling retain their meanings;
- Enter does not start or queue a goal and records a concise
  `run in progress — composer is read-only in P1a` notice;
- slash text stays composer text and dispatches nothing;
- `q`, Escape, or Ctrl-C on an empty composer arms `Stopping` and shows that
  the current model/tool call is not preemptible;
- the session exits after the worker's terminal outcome arrives.

A channel closed before `Finished`, a spawn failure, or a worker error moves
the session to a visible failed state and still restores the terminal.

### `App` remains the only render state

`App` gains:

- `run_mode` (`Idle`, `Running`, `Stopping`, `Finished`, `Failed`);
- a bounded deque of rendered activity lines;
- the current goal label;
- the terminal outcome line;
- the read-only-running flag used by the composer title.

`on_event` remains the sole trace reducer. For live mode it updates the
existing transcript and counters, then appends the rendered line to the
bounded activity deque. The deque is capped by both event count and rendered
characters, so a long run cannot grow `App` without bound. Replay continues
to use its existing cursor/filter refresh path.

Transcript scrolling is already an offset from the tail, so appending while
scrolled does not move the user's window. `End` returns to the tail. P1a does
not add independent scrolling for the activity region; it shows the most
recent bounded lines.

### Rendering

`ui::draw` keeps the existing pure `&App` contract and adds one vertically
stacked `run activity` region between the transcript and status rows. On small
terminals the region receives a minimal height and existing transcript,
status, and composer rows are never dropped.

While running, the region title and composer title state that input is
read-only. Rendered content contains only existing `render_line` output and
lifecycle text; no secrets or provider credentials are introduced.

## Error handling

| Failure | UI state | Terminal behavior |
|---|---|---|
| Worker cannot start | `Failed` with the start error | restore once |
| Model/tool step fails | normal `ModelError` event; run continues or reports failure | unchanged |
| Worker future returns an error | `Failed` with the error | restore once |
| Live channel closes before outcome | `Failed` with channel-closed notice | restore once |
| `TraceSink` file write fails | unchanged durable-trace best effort | unchanged |
| Live receiver is dropped | worker keeps running; sink discards notifications | unchanged |
| Quit during a call | `Stopping` until outcome | no call abort |

## Testing

### Trace seam

- Attached sink receives the same events in the same order as `events()`.
- Token totals and JSONL output remain correct with a sender attached.
- Detached and closed senders keep existing trace behavior.
- A sink without a live sender creates no channel and changes no output.

### Session reducer

- Idle goal submission enters `Running` exactly once.
- Trace, boundary, and finished messages transition state deterministically.
- A finished message is applied once; later messages are ignored.
- Channel loss and worker error produce `Failed` without leaving `Running`.
- Stop intent survives until a terminal outcome.

These tests use plain channels and a deterministic fake worker; no PTY or
network is required.

### App and rendering

- Live events update transcript, counters, and bounded activity.
- The activity bound evicts oldest lines.
- A scrolled transcript does not jump when a new event arrives; `End`
  returns to the tail.
- Running mode renders activity, outcome state, and a read-only composer.
- Wide and small-height TestBackend fixtures contain no overlapping or
  missing required rows.
- Replay tests remain green and receive no live state.

### Integration

- A stub-client multi-round goal runs through the worker and produces the
  same ordered trace events in the sink and live receiver.
- `run`, `eval`, and `--replay` behavior remain unchanged.
- A manual `rof chat` smoke confirms terminal restoration on success and
  visible failure without a network dependency.

## Phase-gate preflight

The vision requires Clippy with warnings denied. The current baseline has two
pre-existing warnings unrelated to P1a:

- `clippy::items_after_test_module` in `src/tools/mod.rs`;
- `clippy::useless_vec` in `src/eval/suite.rs`.

Fix them first in a separate mechanical commit. Do not mix unrelated cleanup
into the P1a feature commits.

## Implementation sequence

1. Mechanical Clippy preflight.
2. `LiveEvent` and optional `TraceSink` forwarding with order tests.
3. Pure `LiveSession` state machine with fake-worker tests.
4. `GoalRunner` extraction and silent live-worker execution.
5. `App` live state/bounded activity and pure rendering.
6. Wire the injected goal starter into `run_live` and `main`.
7. Integration tests, manual smoke, full gates, and STATUS update.

Each step is independently testable. Commit boundaries follow the sequence so
the concurrency seam can be reviewed before the view is widened.

## Acceptance criteria

P1a is complete only when all of the following are evidenced:

1. A stub multi-round goal runs in a worker while the pump renders events.
2. Sink order and live-channel order are identical.
3. The activity view is bounded and sourced only from `App`.
4. The composer cannot submit or dispatch anything while running.
5. Quit and every failure path restore the terminal exactly once.
6. Replay remains read-only and unchanged.
7. `run` and `eval` outputs remain unchanged.
8. Formatting, the full test suite, and Clippy with warnings denied pass.
