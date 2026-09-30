# NEXT.md — resume here (ordered, gated)

Baseline: `cargo test --workspace` → 176 green, clippy clean. Tree clean. Read AGENTS.md + research/DECISIONS.md first. V1 baseline frozen as tag `v1-frozen` on `master`; this rewrite lives on branch `v2-core`.

1. **Live smoke leg** — `LIVE_SMOKE=1 ROF_TEST_BASE_URL=<base> cargo test -p provider-openai --test live_smoke -- --ignored`. Needs endpoint + key in env. Zero CI spend. Gate: 3/3 live pass.
2. **Freeze Slice A** — task ids + tags + digests into `crates/eval/slices/` (TB session-window-debug + Multi-SWE Rust 40–60 ids, still unenumerated). Gate: byte-identical re-freeze.
3. **Oracle pre-flight (free) → 10-instance pilot** — check ≤1.3× tokens-per-solved vs v1 baseline (still unread from v1 logs). Gate: 0 infra failures.
4. **Full slice 3-rep matrix** vs Tier-B recomputed rivals (logs on disk, no re-runs). Gate: paired bootstrap + Wilson.
5. **Post-baseline only:** hunk-restore API (snapshot) → bets wiring (proof gating live) → ablation B→+A→+C.
6. **Deferred:** TUI, Modal/Daytona backends, Harbor upload export, test-depth knob.

Env needed throughout: model key (`ASTRIA_API_KEY` et al), endpoint URL, docker + Harbor for stages 3–4.
