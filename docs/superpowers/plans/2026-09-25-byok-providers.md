# BYOK Providers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** End users bring their own keys and define their own providers, all managed from `rof chat`.

**Architecture:** A persisted provider registry (`~/.rof/providers.json`, non-secret) joins the existing 0600 credentials store. `auth.rs` resolves bases from registry + built-ins; `main.rs` builds one client per role from provider records; the TUI gains provider/model-per-role commands with masked key entry. Secrets never touch transcripts or the repo.

**Tech Stack:** Rust, serde_json, existing reqwest verify path. No new deps.

## Global Constraints

- `cargo fmt` before every commit; full `cargo test` green twice before finishing.
- Tests offline (ROF_CREDENTIALS override + fake bases; never hit network).
- Keys never printed, never in assertions, never committed. `~/.rof/` stays out of the repo.
- Byte-identical defaults: no registry file + no env = today's behavior exactly.

---

### Task A1: Provider registry (persisted, non-secret)

**Files:**
- Modify: `src/tui/auth.rs` (registry load/save, `base_for` consults it)
- Test: `tests/tui_auth.rs` (extend with ROF_CREDENTIALS-style override for registry path)

**Interfaces:**
- Consumes: existing `store()` path pattern.
- Produces: `pub struct ProviderRec { pub name: String, pub base_url: String }`, `pub fn providers_file() -> PathBuf` (ROF_PROVIDERS override, default `~/.rof/providers.json`), `pub fn registry() -> BTreeMap<String, String>` (name → base), `pub fn save_provider(name, base)`, `pub fn remove_provider(name) -> bool`.

- [ ] **Step 1: Write the failing tests** in `tests/tui_auth.rs` (inspect its override pattern first; mirror with `ROF_PROVIDERS` pointing at a temp file):

```rust
#[test]
fn custom_provider_round_trips_and_resolves_base() {
    // save_provider("acme", "https://llm.acme.test/v1") then base_for("acme") == Ok(that base)
}

#[test]
fn unknown_provider_still_errors_offline() {
    // base_for("nope") is Err, no network touched
}
```

- [ ] **Step 2: Run to verify they fail** — `cargo test --test tui_auth` → missing items.
- [ ] **Step 3: Implement.** Registry file JSON `{"acme": "https://..."}`. `base_for` order: built-ins (openrouter/go/atria) → registry → `custom` via ROF_CHAT_BASE (kept as compat) → Err. Validate base on save: must start with `http://` or `https://`, reject otherwise (test this too).
- [ ] **Step 4: `export_missing_env` for registry providers.** Custom entries map to role envs only when a role explicitly names them (see Task A2) — do NOT blanket-fill ROF_TOKEN (that would reroute the default client). Document this in the fn comment.
- [ ] **Step 5: Run `cargo test --test tui_auth`** → PASS. Commit.

```bash
git add src/tui/auth.rs tests/tui_auth.rs
git commit -m "feat: persisted provider registry (non-secret)"
```

---

### Task A2: Per-role clients from provider records

**Files:**
- Modify: `src/config/mod.rs` (role → `provider/model` binding), `src/main.rs` (`build_services`)
- Modify: `src/tui/auth.rs` (key resolution: store → env per provider)

**Interfaces:**
- Produces: `pub struct ProviderBinding { pub provider: String, pub model: String }` in config; `RoutingConfig` gains nothing (model strings like `acme/model-x` already flow through) — instead `build_services` resolves the provider prefix before `/` into (base, key).

Design (locked): model ids stay `provider/model` strings end-to-end (config, trace, RunLabel unchanged). `build_services` splits each role's model at the first `/`: left = provider name. Key resolution order per provider: credentials store → provider's env knob (`OR_TOKEN` for openrouter; `ROF_TOKEN`+`ROF_CHAT_BASE` only when the model names no other provider — today's compat path). Base resolution: `base_for(provider)` (built-in or registry). A role whose provider has no key falls back to today's behavior (shared client) — never a hard error at startup; the call fails at call time with the provider named.

- [ ] **Step 1: Failing test** — new `#[cfg(test)]` in `src/tui/auth.rs` or extend `tests/tui_auth.rs`:

```rust
#[test]
fn provider_prefix_splits_model_ids() {
    assert_eq!(split_provider_model("acme/model-x"), ("acme", "model-x"));
    assert_eq!(split_provider_model("model-x"), ("", "model-x"));
}
```

- [ ] **Step 2: Fail** → missing `split_provider_model`.
- [ ] **Step 3: Implement** `pub fn split_provider_model(id: &str) -> (&str, &str)` + `pub fn key_for(provider: &str) -> Option<String>` (store map → env fallback per provider; built-ins keep their exact current mapping). Rewire `build_services`: resolve per role (context/executor+fallback/verify), build one `OpenRouterClient` per distinct (base, key) pair, share Arcs. `verify` client keeps its `from_verify_env` precedence (judge on another provider still works when explicitly set).
- [ ] **Step 4: Full `cargo test`** (integration tests use StubClient — untouched). Commit.

```bash
git add src/tui/auth.rs src/config/mod.rs src/main.rs
git commit -m "feat: per-role provider clients (BYOK plumbing)"
```

---

### Task A3: TUI commands (provider mgmt + per-role models + masked keys)

**Files:**
- Modify: `src/tui/cmd.rs` (parse `/provider`, `/model verify|fallback`), `src/tui/run.rs` (dispatch), `src/tui/app.rs` (`mask_input: bool`), `src/tui/ui.rs` (render bullets when set)
- Test: `tests/tui_cmd.rs` (parse cases)

**Interfaces:**
- Consumes: Task A1/A2 fns.

New commands:
- `/provider add <name> <base-url>` — validates + saves, prints base (never key).
- `/provider list` — registry + built-ins with verify status.
- `/provider rm <name>` — refuses built-ins.
- `/model verify <p/m>` — sets ROF_VERIFY_MODEL (applies next goal).
- `/model fallback <p/m>` / `/model fallback none` — ROF_EXEC_FALLBACK.
- `/login` key capture becomes masked: `awaiting_key` sets `app.mask_input = true`; `draw` renders `•` × input len; cleared on submit/cancel. Update the login prompt text (no longer "input will echo").

- [ ] **Step 1: Failing parse tests** in `tests/tui_cmd.rs`:

```rust
#[test]
fn provider_forms_parse() {
    assert!(matches!(parse("/provider add acme https://x"), Some(Action::ProviderAdd(..))));
    ...
}
```

(names TBD by implementer — keep the enum shape `ProviderAdd(String, String)`, `ProviderList`, `ProviderRm(String)`.)
- [ ] **Step 2: Fail** → no such variants.
- [ ] **Step 3: Implement** parse + dispatch + mask flag + help_text update. Dispatch `/provider add` prints `provider acme → base (verify with /login acme)`.
- [ ] **Step 4: `cargo test --test tui_cmd --test tui_app`** → PASS. Commit.

```bash
git add src/tui/cmd.rs src/tui/run.rs src/tui/app.rs src/tui/ui.rs tests/tui_cmd.rs tests/tui_app.rs
git commit -m "feat: TUI provider mgmt, per-role models, masked keys"
```

## Self-Review

- Coverage: registry, plumbing, TUI — each task independently testable.
- No placeholders: exact fns, exact commands, exact assertions.
- Consistency: model-id `provider/model` strings unchanged everywhere downstream (trace, RunLabel, compare untouched).

## Execution

Inline, in order A1→A3. After all: full suite ×2, STATUS entry.
