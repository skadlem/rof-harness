# TUI Replay + Full-TUI Vision Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Seekable replay now (the permanent debugging tool); a written vision + phased plan for the pi/hermes-class full TUI (not built yet).

**Architecture:** D1 extends `replay()` with an indexed event list, cursor, and filter — all pure over `App`+event vec (testable with TestBackend). D2 is a spec doc in `docs/superpowers/specs/` plus a phased plan file. No orchestrator changes in either.

**Tech Stack:** Rust, ratatui TestBackend for tests. No new deps.

## Global Constraints

- `cargo fmt` before commits; tui suites green twice.
- Replay stays read-only (no goal execution from replay).
- D2 builds nothing — spec + phases only. Any code is a plan failure.

---

### Task D1: Seekable replay

**Files:**
- Modify: `src/tui/run.rs` (`replay()` — index, cursor keys, filter), `src/tui/app.rs` if cursor state belongs in App (it does: `replay_idx: usize, replay_filter: String` — keeps draw pure)
- Test: `tests/tui_app.rs` (cursor/filter pure fns) + a TestBackend draw test reusing the file's `screen()` helper

**Interfaces:**
- Produces: `pub fn replay_filter(events: &[TraceEvent], query: &str) -> Vec<usize>` (pure, in render.rs or app.rs — implementer's choice, name locked), cursor step/clamp logic on App.

Behavior (locked): replay loads all events, renders transcript up to cursor (default: end). Keys: `j/Down` +1, `k/Up` −1, `G` end, `g` start, `/` + text sets filter (only matching lines shown; empty clears), `q` quits. Status line shows `replay i/N [filter]`. Help line on entry lists keys.

- [ ] **Step 1: Failing tests**:

```rust
#[test]
fn replay_filter_selects_matching_lines() {
    // two SessionStart + one ReviewVerdict events; query "verdict" → 1 index
}

#[test]
fn replay_cursor_clamps() {
    // App replay_idx steps beyond ends clamp; filter + cursor compose
}
```

(Write against the real TraceEvent shapes used in tui_app.rs.)
- [ ] **Step 2: Fail** → missing items.
- [ ] **Step 3: Implement.** Keep the existing per-line replay path (replay-inert composer untouched).
- [ ] **Step 4: `cargo test --test tui_app --test tui_cmd --test tui_render`** → PASS. Commit.

```bash
git add src/tui/run.rs src/tui/app.rs src/tui/render.rs tests/tui_app.rs
git commit -m "feat: seekable replay (cursor + filter)"
```

---

### Task D2: Full-TUI vision spec (no code)

**Files:**
- Create: `docs/superpowers/specs/2026-09-25-full-tui.md`
- Create: `docs/superpowers/plans/2026-09-25-full-tui.md` (phases only, no implementation)

**Content (locked scope):** Target UX (pi/hermes/claude-code class): live run pane with streaming model/tool events, composer usable mid-run (steer/queue at round boundaries), slash commands from the existing registry, `/providers` from the BYOK plan, transcript + status + diff panes, replay reuse for post-mortem. Architecture: orchestrator event channel (the Bunny-#2 direction — mpsc + worker future, commands applied at round boundaries), App as the single render state (already the pattern), ratatui widget split. Explicit non-goals: no step-debugger, no multi-session, no remote attachment. Phases: P1 event channel + live read-only pane; P2 mid-run input (steer/queue); P3 panes (diff, providers, metrics); P4 polish (themes, completion, persistence). Dependencies noted: planner removal (done), BYOK provider switching (Plan A) must precede P3.

- [ ] **Step 1: Write the spec** (reference the actual current files: run.rs console loop, app.rs state, render.rs lines, auth.rs store).
- [ ] **Step 2: Write the phased plan** (phase entry/exit criteria, no code).
- [ ] **Step 3: Commit.**

```bash
git add docs/superpowers/specs/2026-09-25-full-tui.md docs/superpowers/plans/2026-09-25-full-tui.md
git commit -m "docs: full-TUI vision spec + phased plan"
```

## Self-Review

- D1 independently shippable; D2 references real files, invents no APIs.
- No placeholders: keys, behaviors, phases all explicit.

## Execution

Inline, D1 then D2. STATUS entry.
