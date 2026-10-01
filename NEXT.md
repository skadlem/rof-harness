# NEXT.md — resume here (ordered, gated)

Baseline: `cargo test --workspace` → 176 green, clippy clean. Tree clean. Read AGENTS.md + research/DECISIONS.md first. V1 baseline frozen as tag `v1-frozen` on `master`; this rewrite lives on branch `v2-core`.

1. **Live smoke leg — DONE 3/3** on Atria (`https://api.atria-asi.ai/v1/`, `Atria-Dawn-Preview`, bridged `ASTRIA_API_KEY`→`OPENAI_API_KEY` locally, key names only).
2. **Freeze Slice A — PARTIAL v1 seed.** `tb-slice-a.json` = 2 measured-local tasks (swd + layout-config-recreation2, full image digests + sha256 tests/solution, TB-2.1 @7131e43); `multiswe-rust.json` = 50 ids (round-robin 5/repo placeholder, HF @56ff018c). Re-freeze byte-identical ✅ + **winnability gate green** (eval `check_winnability`: artifacts→{patch|create}, tests + oracle present; live on both task dirs). BLOCKED: 21-expansion (v1 ids unrecoverable) + Mid-Range filter (needs public matrix).
3. **Oracle pre-flight — DONE 10/10** (job `2026-09-30__rof-preflight-v1-seed`). **Pilot v1 (Atria) — DONE 0/10**, root-caused to harness starvation (fixed). **Pilot v2 (DeepSeek official, fixed protocol) — DONE 0/10** (25–199s/trial, 0 infra, 1 handled truncation; 10/10 zero writes). Harness fully functional vs strict API; open links: history-resend budget burn + model drive. Gate: 0 infra failures ✅; ≤1.3× guardrail uncheckable at 0 solved.
4. **Full slice 3-rep matrix** vs Tier-B recomputed rivals (logs on disk, no re-runs). Gate: paired bootstrap + Wilson.
5. **Post-baseline only:** hunk-restore API (snapshot) → bets wiring (proof gating live) → ablation B→+A→+C.
6. **Deferred:** TUI, Modal/Daytona backends, Harbor upload export, test-depth knob.

Env needed throughout: model key (`OPENAI_API_KEY`), endpoint URL, docker + Harbor for stages 3–4.
