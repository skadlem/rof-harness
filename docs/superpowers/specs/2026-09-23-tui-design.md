# rof interactive console (`rof chat`) — design spec

Date: 2026-09-23. Status: approved overview, pending spec review.
Parent session goal ordering note: exit criteria (bug02, full-20, rival re-runs)
take precedence over implementation; this spec is design-only, no code.

## Goal

An interactive terminal console for rof in the lineage of pi / hermes / claude
code: live chat driving the harness loop, run internals visible, mid-session
controls, provider login. Reference-grounded: every choice below names its
steal source. Research reports:
`subagent-artifacts/outputs/17021bb8-93d7-4300-b07d-678278e2de7a/research.md`
(web: claude, hermes, pi) and `context.md` (local: installed pi package +
hermes source) under the session `01a0cece-…` artifact store.

## Non-goals (v1)

Per-call approval prompts, `/compact`, `/bg`, `/plan`, checkpoints-as-UI,
voice, cron, session handoff, memory, multi-agent orchestration, custom layout
engine, mouse-first interaction. Rationale per item is in §Choices 7–9.

## Choices

### 1. Screen modes: fullscreen default, regular fallback (pi)

`rof chat` opens fullscreen (owns viewport, sticky composer/footer dock);
`rof chat --regular` plus a live `/display` toggle keeps native terminal
scrollback. Exiting fullscreen prints the final transcript or only a resume
hint (configurable). Never force alt-screen: pi's `regular`/`fullscreen`
runtime switch is the reference behavior.

### 2. Four areas + status bar (hermes bar, pi areas)

Transcript (scrollable) · run pane · status bar · composer. Status bar:
`model │ ctx used/max with color fill │ $cost │ elapsed │ compactions │
bg tasks`, context thresholds green<50 / yellow<80 / orange<95 / red≥95,
reflow full/compact/minimal at 76/52 columns. All numbers derive from the
existing trace totals (`ContextMetrics`), the same ones `rof compare` uses.

### 3. Mode-as-chrome (claude + pi, hermes badge)

Composer border color encodes thinking level (pi); a distinct token marks
plan/verify-guard states (claude `planMode`); armed verify-guard or overridden
caps pin a persistent badge in the bar (hermes ⚠ YOLO). v1 ships dark/light;
custom theme JSON (`themes/<name>.json`, hot-reload) is the v2 path.

### 4. Slash palette (claude, pi registry, hermes single-registry)

`/` opens a fuzzy-filtered menu with argument hints; commands parse only at
message start. One registry drives help, completion, and dispatch. The skill
store auto-registers `/skill:<name>` entries (skill names + descriptions
already exist). Command set: `/model /models /login /logout /attempts /rounds
/thinking /effort /caps /retry /approve /reject /context /undo /diff /trace
/help /quit` (+ `/display /busy /hotkeys` below).

### 5. Steering: busy-input modes (hermes)

`busy_input_mode: interrupt | queue | steer` (config + `/busy` toggle):
interrupt preserves completed work and parks long shell jobs instead of killing
them; queue sends as next turn; steer injects at the next tool boundary.
Ctrl-C = interrupt under the current mode; double-press = force-exit.

### 6. Auth (claude flow, pi storage, hermes pools)

`/login [provider]` → key capture → one cheap verify call →
`~/.rof/credentials` (0600, same discipline as `~/.rof/go.key`). Env keys take
precedence and skip login (claude behavior); `!cmd` key sources accepted (pi);
`/models` shows plugged-in providers with live ok/unverified/auth-failed
status; `/logout` removes. Model identity is always explicit
(`/model <provider>/<model-id>`, Tab-completed) — no aliases.

### 7. Session powers from existing systems (UX steals only)

`/undo` → `TreeService` rollback; `/diff` → write-gate `diff --stat` with
truncation notice; `/context` → `ContextMetrics` gauge; resume → append-only
trace replay (no session DB v1). `/compact` skipped (no chat-loop summarizer).

### 8. Composer ergonomics (hermes, pi)

`Shift+Enter`/`Alt+Enter` newline with Kitty-protocol honesty + documented
fallbacks; `@path` fuzzy reference + Tab completion; multi-line paste preview;
`Ctrl+G` `$EDITOR`; `Ctrl+S` in-memory prompt stash (never disk); `!cmd`
zero-cost shell escape through the `proc.run` allowlist (shortcut, not bypass).

### 9. Customisation = three files (pi scheme)

`~/.rof/config` (providers + `[tui]`), `keybindings.json` (named action-ids,
`[]` disables, `/hotkeys` shows live), `themes/` (v2). `/` alone lists commands
with current values. Everything TUI-settable is headless-settable via env;
no TUI-only state.

### 10. Renderer purity + headless parity (pi + hermes modes)

Renderer is a pure function of `(event, state)`, unit-tested against recorded
traces with no terminal; unknown events render dim `·`, never crash.
`rof chat --print` and `--format stream-json` expose the same event stream to
scripts.

## Architecture

New `src/tui/` module (renderer + input + registry) on the existing
`TraceSink` event stream; one `mpsc` inbox into `Session`; all mutations
(knobs, model swaps, retries, follow-up goals) apply at round boundaries only.
New deps: `ratatui` + `crossterm`. No orchestrator redesign. Fallback: one
failed ratatui spike → inline streaming REPL (grind rule).

## Testing

Renderer unit tests on recorded traces; command-registry tests (parse,
validation errors stay inline); headless `--print` golden test; `cargo test` +
`clippy -D warnings` + `fmt` green; reviewer pass before commit (session rule).

## Acceptance

(a) `rof chat` drives a goal end-to-end with all panes live; (b) every slash
command works mid-session at round boundaries; (c) `/login` → `/models` →
`/model` switches providers; (d) resume from trace after crash; (e) zero
behavior change to `run`/`eval` paths (full suite green).
