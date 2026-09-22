# rof v4 — Beating the Frontier Harness Design

**Date:** 2026-09-22 · **Status:** approved (all-of-the-above) · **Scope:** 7 bets, phased

## Goal

Keep rof's measured wedges (honesty substrate, 5.1× billed-token lead, per-check
comparability) and add the one capability mechanism from each frontier winner
that the field converged on in 2026: isolated exploration, symbol retrieval,
parallel attempts, outer verification, persistent memory, strong-model shell
freedom, multi-harness arena.

## Architecture (what changes, in one paragraph)

No new services, no new deps, no new traits. Each bet is a small module behind
an existing seam plus a config knob defaulting to off/1 so every arm stays
comparable: `agents/explorer.rs` (read-only cheap-model summarizer) feeds the
implementer's volatile tail; `context/symbols.rs` (regex def-map + 1-hop
expansion) feeds `Retriever`; `AppConfig.attempts` loops the task body and picks
cheapest-pass; `RoundServices.verify_guard` runs one post-hoc independent-judge
pass; `context/memory.rs` loads `AGENTS.md`/`LESSONS.md` into the stable head;
`tools` gains prefix-allowlisted shell for the exec tier; `rof arena` runs one
suite and prints score + billed (multi-harness diff via existing `compare`).

## The 7 bets (minimal form, trigger, success bar)

### 1. Isolated explorers (`src/agents/explorer.rs`)
Read-only agent: `fs.list/read`, `skills.list/view` only. Input: goal + file map.
Output: JSON `{summary, key_files:[{path,why}], quotes:[{path,line,text}]}` via
the cheap ContextService. Orchestrator runs it once per task (when
`ROF_EXPLORER=yes`), appends `key_files` to the implementer's volatile tail
through the existing assembler. Never writes, never runs shell.
Success: `reads`-deferral rounds drop; recall flat-or-up; tokens/task flat-or-down.

### 2. Symbol graph retrieval (`src/context/symbols.rs`)
Deterministic, no embeddings (field voted no). Parse `struct/fn/enum/trait/impl`
(Rust) + `def/class` (Python) via line regex into `Symbol{ name, path, line }`.
`expand(query_symbols, 1 hop)`: files defining a goal-named symbol + files whose
`use/crate::/import/from` line mentions it. Merged with keyword hits, keyword
still wins ties. Gated by the existing recall metric.
Success: recall up on click band; no prompt-size regression (still ≤12k volatile).

### 3. Parallel attempts + pick-best (`AppConfig.attempts`, `ROF_ATTEMPTS=N`)
N independent task bodies (each: baseline → implement → checks), sequential in
v1 (parallel is eval-jobs' job). Pick: first cheapest-pass; else last failure.
Each attempt traced (`AttemptStart/AttemptEnd`), billed folded. Default 1 =
byte-identical run.
Success: pass rate up at N=3 with billed/N still below hermes per-task.

### 4. Outer verify loop + inconclusive timeouts
`verify_guard` (in `RoundServices`): after an inner pass, one Reviewer call on
the verify model; veto → one re-prompt round with the veto note. Timeout/cancel
is `inconclusive`, not fail: eval re-checks the tree (`git diff` non-empty +
checks re-run once) before recording zero.
Success: veto catches pass-happy reviewer at least once per suite; zero
wall-clock cancellations recorded as fails with a passing tree on disk.

### 5. Persistent memory v1 (`src/context/memory.rs`)
Load order into stable head: `<workdir>/AGENTS.md` (project), `~/.rof/LESSONS.md`
(user), then session conventions. Caps: 4k chars each, head-truncated, traced as
`MemoryLoad{source,bytes}`. No vector store, no auto-write (skills proposals
stay the write path).
Success: conventions survive across tasks without per-task prompt edits.

### 6. Strong-model shell freedom (`tools` prefix allowlist)
`ProcRunTool` gains `allowed_prefixes`: exact match OR `starts_with(prefix+" ")`
for a small list (`ROF_ALLOW_PREFIXES`, e.g. `cargo test,pytest,python3 -m pytest`).
Weak tier keeps exact-only. Implementer may get `proc.run` in a new `exec-shell`
profile behind `ROF_SHELL=yes`; default unchanged.
Success: strong-model arms complete with fewer implementer rounds; no policy
bypass (symlink + `.git` gates still hold).

### 7. `rof arena` (score + billed, side by side)
`rof arena <suite> [--attempts N]`: runs the suite with the current binary,
prints per-task score + billed + label, writes report JSON. Multi-harness diff
reuses `rof compare a.json b.json`. No new infra, no cloud.
Success: one command reproduces the 5.1× chart from README.

## Global constraints
- Rust 1.70+, no new dependencies (`cargo build` offline-safe).
- Defaults preserve current behavior (all knobs off/1 → byte-identical traces).
- Every side effect through `ToolRegistry::call`; `.git` unreachable; deny-by-default.
- TDD per feature; `cargo test` + `cargo clippy` + `cargo fmt --check` green before each commit.
- Comparability sacred: model pair frozen per run; `RunLabel` extended, never broken.

## Non-goals (explicitly out)
Vector embeddings, general agentic frameworks, MCP servers, cloud sandbox,
auto-editing memory, parallel-intra-task thread pools, LLM summarizer changes.
