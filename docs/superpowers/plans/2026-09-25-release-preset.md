# Release Preset Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `rof eval --release` = the release gate: reps=3, outer judge on, judge model required.

**Architecture:** One preset method on AppConfig (unit-testable) + one CLI flag. No other behavior changes.

**Tech Stack:** Rust only.

## Global Constraints

- `cargo fmt` before commit; affected suites green twice.
- Default runs byte-identical (`--release` is strictly opt-in).

---

### Task C1: Release preset

**Files:**
- Modify: `src/config/mod.rs` (`apply_release_preset`), `src/main.rs` (`--release` flag + wiring + banner)
- Test: in-file `#[cfg(test)]` in config (check for existing test mod first; add if absent)

**Interfaces:**
- Produces: `pub fn apply_release_preset(&mut self, reps: &mut usize) -> Result<(), String>`.

Semantics (locked): `--release` sets reps=3 (unless `--reps N` explicitly given — explicit wins), `verify_guard = true`, and ERRORS when no verify model is configured (`routing.verify_model` empty AND `ROF_VERIFY_MODEL` unset — note apply_env runs before this, so check the effective value). Error text names the fix: `release gate needs an independent judge: set verify_model or ROF_VERIFY_MODEL`. Banner line: `release gate: reps=3 verify_guard=on judge=<model>`.

- [ ] **Step 1: Failing tests** (config test mod):

```rust
#[test]
fn release_preset_requires_a_judge() {
    let mut cfg = AppConfig::default();
    let mut reps = 1;
    assert!(cfg.apply_release_preset(&mut reps).is_err());
    cfg.routing.verify_model = Some("judge-model".to_string());
    assert!(cfg.apply_release_preset(&mut reps).is_ok());
    assert_eq!(reps, 3);
    assert!(cfg.verify_guard);
}

#[test]
fn explicit_reps_survive_release() {
    let mut cfg = AppConfig::default();
    cfg.routing.verify_model = Some("j".to_string());
    let mut reps = 5;
    cfg.apply_release_preset(&mut reps).unwrap();
    assert_eq!(reps, 5);
}
```

- [ ] **Step 2: Fail** → no such method.
- [ ] **Step 3: Implement** + wire in the eval block (after jobs/reps parsing; `--release` detection like `reps_arg` — add `has_flag(args, "--release")` helper or inline `args.contains`).
- [ ] **Step 4: `cargo test --lib config` + eval suites** → PASS. Commit.

```bash
git add src/config/mod.rs src/main.rs
git commit -m "feat: rof eval --release gate (reps-3 + judge required)"
```

## Self-Review

- The gate is process (release arms run this flag); dashboards compare via RunLabel (judge named) — no report changes needed.
- Explicit `--reps` wins; document in `--help` if the eval block prints usage (check).

## Execution

Inline, single task. STATUS entry.
