# Knowledge index

Map of every note in this repo. Read this first; open only what the question needs.
Status: **active** (current, edit freely) · **record** (finished result; may be condensed, never given new claims) · **superseded → X** (read X instead) · **void** (premise was wrong).
Update this file in the same commit as adding, superseding or voiding a note.

## Start here

| Need | Read |
|---|---|
| Rules for working in this repo | [AGENTS.md](../AGENTS.md) |
| What to do next | [SESSION-NEXT.md](../SESSION-NEXT.md) (session handoff), then [research-next-steps.md](research-next-steps.md) (canonical state and deferred register) |
| Why something is the way it is | [decisions.md](decisions.md) (decision log, with measurements) |
| How the system works | [architecture.md](architecture.md) |
| Ordered plan | [roadmap.md](roadmap.md) |

## Plans

- [capability-phases](superpowers/plans/2026-11-11-capability-phases.md) — phased capability plan · **active** (0–4 done, 5 stopped, 6 evidence-gated)
- [envelope-phases](superpowers/plans/2026-11-11-envelope-phases.md) — response to the architecture review · **record** (P0+P1 shipped 2026-11-11; P2 never launched; envelope axis closed)
- [next-after-slice-a](superpowers/plans/2026-11-11-next-after-slice-a.md) — state after Slice A: 5/18; P1–P4 closed · **record** (2026-11-12)
- [output-fidelity](superpowers/plans/2026-11-11-output-fidelity.md) — archive access + Harbor controls · **record** (done)
- [tb41-option-screen](superpowers/plans/2026-11-24-tb41-option-screen.md) — three single-field arms (head-tail output, errors-only rollback, 32k responses) vs control on four TB4.1 CPU tasks, ~$4 · **proposed** (owner approved the adapter-check trial only)
- [roadmap](roadmap.md) — ordered plan of open work; history lives in decisions.md · **active**
- [research-next-steps](research-next-steps.md) — handoff + deferred-work register · **active**
- [harness-capability-plan](harness-capability-plan.md) — the twelve 2026-11-07 proposals · **record**
- [fix-plan](fix-plan.md) — five reliability fixes · **record** (all shipped)

## Decisions

- [decisions.md](decisions.md) — decision log · **active**
- [decisions/design-v2](decisions/design-v2.md) — brief: checklist lever vs envelope axis for the last $3.12 · **active** → verdict [decision-1152](reviews/decision-2026-11-11-1152.md)
- [decisions/design](decisions/design.md) — first brief for the $3.12 question · **superseded → design-v2** (verdict [decision-1134](reviews/decision-2026-11-11-1134.md))
- [research/DECISIONS.md](../research/DECISIONS.md) — gate log, one line per gate (local-only)

## Architecture and reference

- [architecture.md](architecture.md) — how the crates wire together · **active**
- [architecture/design](architecture/design.md) — premises brief for the architecture review · **record** → verdict [architecture-1032](reviews/architecture-2026-11-11-1032.md)
- [threat-model](threat-model.md) — what the tools enforce · **active**
- [harness-leaderboard-review](harness-leaderboard-review.md) — what the Harness Leaderboard measures · **record**

## Experiments (records)

- [current-benchmark-pilot](current-benchmark-pilot.md) — TB 4.1 ROF pilot, eight paid rows
- [harness-experiments](harness-experiments.md) — capability experiments log
- [phase-0-2-implementation](phase-0-2-implementation.md) · [phase-3-5-implementation](phase-3-5-implementation.md) — what each phase shipped
- [deepseek-official-pilot](deepseek-official-pilot.md) — **superseded → [current-benchmark-pilot](current-benchmark-pilot.md)** (benchmark-recency correction; stub)
- [steering-experiment](steering-experiment.md) — **void** (workspace-seeding bug, 2026-11-06; stub)
- 2026-11-11 Slice A session, in order:
  [oracle-preflight](experiments/2026-11-11-slice-a-oracle-preflight.md) →
  [triage](experiments/2026-11-11-triage.md) →
  [faithful](experiments/2026-11-11-slice-a-faithful.md) (5/18) →
  [vs-published](experiments/2026-11-11-slice-a-vs-published.md) →
  [envelope-arm](experiments/2026-11-11-slice-a-envelope-arm.md) (3.3× tokens, 0 new solves) →
  [three-misses](experiments/2026-11-11-three-misses.md) →
  [lit-semantic-deliverable](experiments/2026-11-11-lit-semantic-deliverable.md) ·
  [lit-loop-termination](experiments/2026-11-11-lit-loop-termination.md) →
  [zero-write-prevalence](experiments/2026-11-11-zero-write-prevalence.md) →
  [hf-within-task](experiments/2026-11-11-hf-within-task.md) (zero-write gate fails) →
  [allowlist-and-named-deliverable](experiments/2026-11-11-allowlist-and-named-deliverable.md)
- [reread-census](experiments/2026-11-12-reread-census.md) — re-reads are 3.5–3.6% of tool calls (59–61 of 1,698; recount of a lost script); collapse does not explain the token deaths · **record**
- [deliverable-forcing-arm](experiments/2026-11-12-deliverable-forcing-arm.md) — step-15 directive is read and ignored, 0 of 4; control re-run solved despite the step-15 flag · **record**
- [easy-miss-variance](experiments/2026-11-13-easy-miss-variance.md) — 0 of 9 reps on the three published-easy misses; they are stable misses, `dna-insert` 1 in 5 · **record**
- [exec-observation-bugs](experiments/2026-11-13-exec-observation-bugs.md) — exec mangled 78 of 1,698 banked commands and hid 79 silent failures; both fixed, effect on solve rate unmeasured · **record**
- [slice-a-misses-rerun](experiments/2026-11-13-slice-a-misses-rerun.md) — 2 of 13 misses solve on the fixed binary; one is the earlier infra fix, one is variance, none attributable to the exec fixes · **record**
- [failed-trace-audit](experiments/2026-11-14-failed-trace-audit.md) — 47 of 52 failures are token deaths, 2 claim verification; 20% of requests are a lone helper-script write that the eval prompt instructs · **record**
- [miss-diagnosis](experiments/2026-11-15-miss-diagnosis.md) — the three stable misses reach the answer and run out before writing it; the model paces by steps and misreads the token budget line · **record**
- [budget-line-ab](experiments/2026-11-23-budget-line-ab.md) — requests 1/4, legacy 0/4 on two Slice A tasks, with one pass that did not repeat · **record**
- [no-evidence-census](experiments/2026-11-24-no-evidence-census.md) — 286 logged runs: the output cut drops 21% of exit verdicts, 1.5% of replies waste 8,000 reasoning tokens, rollback saves little; time, spend and repeat caps never bind · **record**
- [local-harness-checks](experiments/2026-11-23-local-harness-checks.md) — request payload, archive recovery and rollback measurements · **record**

## Reviews (independent verdicts)

- [architecture-2026-11-11-1032](reviews/architecture-2026-11-11-1032.md) · [decision-2026-11-11-1134](reviews/decision-2026-11-11-1134.md) · [decision-2026-11-11-1152](reviews/decision-2026-11-11-1152.md)

## Root files

- [README.md](../README.md) — project overview and current measurements
- [NOTES_FOR_PI.md](../NOTES_FOR_PI.md) — older handoff (state as of 2026-11-06) · **superseded → research-next-steps + next-after-slice-a** (stub)
- [SESSION-NEXT.md](../SESSION-NEXT.md) — session handoff: state, decisions, next task · **active**
- `NEXT.md`, `folder-hierarchy.md` — local-only (git-excluded); NEXT.md is the old session log, now a stub · **superseded → roadmap + research-next-steps** (full text: `target/docs-housekeeping-originals/NEXT.md`)

## Research (`research/`, read-only input, local-only)

Never edit these; record conclusions in decisions. Paper notes: [research/papers/INDEX.md](../research/papers/INDEX.md) (73 notes).

### Decision gate log

- [DECISIONS](../research/DECISIONS.md) — gate log (one line each: decision + provenance)

### Crate design studies

- [crate-agent-budget](../research/crate-agent-budget.md) — Crate `agent-budget` — The AND-Gate Budget Owner
- [crate-agent-event](../research/crate-agent-event.md) — Crate `agent-event` — The LIVE Event Vocabulary for a Minimal Rust Coding-Agent Core
- [crate-agent-log](../research/crate-agent-log.md) — the `agent-log` crate — a DURABLE Item log for a minimal Rust coding-agent core
- [crate-agent-loop](../research/crate-agent-loop.md) — The `agent-loop` Crate — Driver Shapes Across Five Systems, and the Loop WE Should Build
- [crate-provider-core](../research/crate-provider-core.md) — the `provider-core` crate — LLM client trait, request/response types, retry, streaming
- [crate-tool-core](../research/crate-tool-core.md) — The `tool-core` Crate — Tool Trait, Registry, `CallStatus`, and the Permission Gate

### Reference-harness studies

- [agentic-ai-core](../research/agentic-ai-core.md) — Agentic-AI Core for a Minimal Rust Coding Agent: What the Research Actually Says
- [agentic-ai-core-atria](../research/agentic-ai-core-atria.md) — Agentic-AI Core for a Minimal Rust Coding Agent: The Research Verdict, Classic + 2024–2026 · **companion to agentic-ai-core**
- [deepseek-harness-core](../research/deepseek-harness-core.md) — DeepSeek Harness (`dsh`) Core — Research for a Minimal Rust Coding-Agent Core
- [hermes-agent-core](../research/hermes-agent-core.md) — Hermes-Agent Core Loop — Research for a Minimal Rust Coding-Agent Core
- [pi-core-418](../research/pi-core-418.md) — Pi Core Agent Loop — Research for a Minimal Rust Coding-Agent Core
- [pi-core-418-atria](../research/pi-core-418-atria.md) — Pi Core Agent Loop — DEEPER Companion Report · **companion to pi-core-418**
- [refs-BRIEF](../research/refs-BRIEF.md) — shared contract for the six ref lanes (2026-10-05)
- [refs-gap-matrix](../research/refs-gap-matrix.md) — six ref lanes merged: per-crate verdicts, ranked steals, skip ledger
- [refs-mini-vs-rof](../research/refs-mini-vs-rof.md) — mini-swe-agent v2.4.6 vs rof's 15 crates
- [refs-pi-agent-vs-rof](../research/refs-pi-agent-vs-rof.md) — pi packages/agent + protocol + client + server + telemetry @ v1.0.2 vs rof (15 crates)
- [refs-pi-durable-vs-rof](../research/refs-pi-durable-vs-rof.md) — refs-pi-durable-vs-rof — pi durability + session state vs rof (lane: session/state durability)
- [refs-pi-harness-vs-rof](../research/refs-pi-harness-vs-rof.md) — pi coding-agent spine (HEAD b2b5c42f) vs rof
- [refs-pi-provider-vs-rof](../research/refs-pi-provider-vs-rof.md) — refs-pi-provider-vs-rof — pi provider/integration layer (v1.0.2) vs our 6 in-scope crates
- [refs-unreal-vs-rof](../research/refs-unreal-vs-rof.md) — unreal-agent vs rof — lane report: rows the core report does not answer
- [refs-wave1-evidence](../research/refs-wave1-evidence.md) — offline gates for wave-2 (S-2 tail-keep, S-3 view paging, #9 test boundary), 2026-10-05
- [rof-v1-actual-architecture](../research/rof-v1-actual-architecture.md) — ROF v1 — Actual Architecture (from source, 2026-09-28 tree) · **historical (v1 tree)**
- [unreal-agent-core](../research/unreal-agent-core.md) — Unreal-Agent Core Loop — Research for a Minimal Rust Coding-Agent Core

### Literature

- [bets-prior-art](../research/bets-prior-art.md) — Prior-Art Check: Three Proposed Coding-Agent Mechanisms (Transactional Edits / Claim-First / Speculative Action Executio
- [harness-steals-2026h2](../research/harness-steals-2026h2.md) — implementation-level steal list from 2026 open agent harnesses (2026 H2)
- [harness2-teardown](../research/harness2-teardown.md) — Google Harness² (arXiv:2610.14224) — primary-source teardown
- [lit-2026-11-ERRATA](../research/lit-2026-11-ERRATA.md) — ERRATA — verification of the 2026-11-10 lane reports
- [lit-2026-11-SYNTHESIS](../research/lit-2026-11-SYNTHESIS.md) — Literature synthesis 2026-11-10 — what the new papers say, and what could help rof · **read with lit-2026-11-ERRATA**
- [lit-2026-11-context-engineering](../research/lit-2026-11-context-engineering.md) — Lane B — Context engineering (what enters the window, what leaves it) — papers 2026-09 .. 2026-11 · **read with lit-2026-11-ERRATA**
- [lit-2026-11-eval-verifiers](../research/lit-2026-11-eval-verifiers.md) — Lane D — eval methodology, verifiers, and test-time compute allocation — papers 2026-09 .. 2026-11 · **read with lit-2026-11-ERRATA**
- [lit-2026-11-harness-architecture](../research/lit-2026-11-harness-architecture.md) — LANE A — harness / scaffold architecture and loop control — papers 2026-09 .. 2026-11 · **read with lit-2026-11-ERRATA**
- [lit-2026-11-tools-inference](../research/lit-2026-11-tools-inference.md) — LANE C — TOOLS, EXECUTION ENVIRONMENTS, AND INFERENCE-SIDE ECONOMICS — papers 2026-09 .. 2026-11 · **read with lit-2026-11-ERRATA**
- [lit-sweep-2026-10-19](../research/lit-sweep-2026-10-19.md) — Literature sweep 2026-10-19 — synthesis and decision
- [lit-sweep-2026-11-14](../research/lit-sweep-2026-11-14.md) — Literature sweep 2026-11-14 — no paper shows subagents lifting a weak model at matched tokens
- [miss-diagnosis-2026-11-15-dna-assembly](../research/miss-diagnosis-2026-11-15-dna-assembly.md) — Full per-run read of `dna-assembly` behind the miss diagnosis (oracle solution read; task contaminated)
- [miss-diagnosis-2026-11-15-dna-insert](../research/miss-diagnosis-2026-11-15-dna-insert.md) — Full per-run read of `dna-insert` behind the miss diagnosis (oracle solution read; task contaminated)
- [miss-diagnosis-2026-11-15-make-mips-interpreter](../research/miss-diagnosis-2026-11-15-make-mips-interpreter.md) — Full per-run read of `make-mips-interpreter` behind the miss diagnosis (oracle solution read; task contaminated)
- [meta-harness-current-systems](../research/meta-harness-current-systems.md) — Current live "meta-harness-like" systems (as of 2026-10-13)
- [meta-harness-published](../research/meta-harness-published.md) — Published Work on Meta-Harnesses for LLM Coding Agents
- [papers-2026h2-eval-methodology](../research/papers-2026h2-eval-methodology.md) — what a 21-task agent slice can actually resolve (eval methodology, 2025-2026)
- [papers-2026h2-harness-levers](../research/papers-2026h2-harness-levers.md) — harness-side capability levers that do NOT need a stronger model — new papers, 2026-08-15..2026-10-19

### Decision audits

- [decision-audit-context-economics](../research/decision-audit-context-economics.md) — External audit of context-economics decisions (C1–C3)
- [decision-audit-eval-metrics](../research/decision-audit-eval-metrics.md) — Decision Audit: Eval-metrics claims (C7, C8) vs external evidence
- [decision-audit-loop-incentives](../research/decision-audit-loop-incentives.md) — External audit of loop-incentive decisions (C4, C5, C6)
- [decision-audit-provider-economics](../research/decision-audit-provider-economics.md) — Provider-economics decision audit — C9 (pricing convention), C10 (retry billing), C11 (reasoning_content passback)

### Audits & diagnostics

- [accounting-recompute](../research/accounting-recompute.md) — is the 300k token cap binding far below the approved $4 guardrail? (wave-1 offline recompute, 2026-10-05)
- [apppath-parity](../research/apppath-parity.md) — `/app/...` path parity — the largest file-tool denial class was a bug, not a policy
- [audit-tools](../research/audit-tools.md) — Audit: `tools-std` + `tool-core` permission gate vs. production agent toolsets and sandboxing models
- [audit-trace](../research/audit-trace.md) — Observability/tracing practice audit — `crates/trace` + `crates/agent-event`
- [audit-verify](../research/audit-verify.md) — Audit of the `verify` crate + `TestTool` against real coding-agent verification gating
- [cost-decomposition](../research/cost-decomposition.md) — cost-decomposition — attributing the measured run dollars (offline, 2026-10-05)
- [diag-snapshot-anomaly](../research/diag-snapshot-anomaly.md) — Snapshot anomaly: sanitize-git-repo-r1 dies on `snapshot baseline`
- [incontainer-exec-policy](../research/incontainer-exec-policy.md) — In-container exec policy: the allowlist is a host guard, and it is capping the in-container arm
- [wire-fidelity-2026-11-06](../research/wire-fidelity-2026-11-06.md) — wire-fidelity diff — our `provider-openai` client vs `deepseek-ai/deepseek-recipe`
- [glm-audit-2026-11-07](../research/glm-audit-2026-11-07/README.md) — from-code audit at `b0e3dd2` ([roadmap](../research/glm-audit-2026-11-07/ROADMAP.md), [loop-context](../research/glm-audit-2026-11-07/loop-context.md), [tools-snapshot](../research/glm-audit-2026-11-07/tools-snapshot.md), [eval-provider](../research/glm-audit-2026-11-07/eval-provider.md), [coverage](../research/glm-audit-2026-11-07/coverage.md)) · **record** (code findings mostly still true; roadmap overtaken by Slice A, see research-next-steps)

### Ablation arms

- [budget-shape-ab1](../research/budget-shape-ab1.md) — Budget-shape A/B — arm 1 (glm-bs, 2026-10-14)
- [dphn-cap-ab1](../research/dphn-cap-ab1.md) — dphn-cap arm 1 — does moving BOTH walls convert token-wall deaths into work?
- [effort-ab1](../research/effort-ab1.md) — ARM dphn-effort — does the same model at higher reasoning effort convert the semantic failures?
- [goal-checklist-ab1](../research/goal-checklist-ab1.md) — Goal-checklist arm 1 — does a completion checklist convert the 5/6 cell?
- [nudge-spec](../research/nudge-spec.md) — Mid-run verification nudge — spec (no code, priced test later)

### Benchmarks & comparison

- [bench-landscape](../research/bench-landscape.md) — External benchmark landscape — picking gates for the Rust coding-agent harness
- [compare-protocol](../research/compare-protocol.md) — Minimal-spend protocol — Rust core vs hermes/pi/v1 (TB Slice A)
- [models-2026h2-shortlist](../research/models-2026h2-shortlist.md) — flash-tier replacement models for the in-container TB arm (2026 H2)
- [rivals-grid-v1](../research/rivals-grid-v1.md) — Rivals comparison grid v1 — what is comparable in the stored measurement data
- [rivals-grid-v1-results](../research/rivals-grid-v1-results.md) — Rivals grid v1 — results (the legal tables, computed)

### Forward-looking briefs

- [future-budget-termination](../research/future-budget-termination.md) — Budgets, auto-submit-at-cap, and near-cap nudges — rof vs published harnesses · **superseded → refresh-budget-termination**
- [future-cache-economics](../research/future-cache-economics.md) — Future cache economics — provider caching pricing/mechanics, achievable cache-hit rates, and a per-request cost function · **superseded → refresh-cache-economics**
- [future-candidate-racing](../research/future-candidate-racing.md) — Speculative / parallel candidate execution for LLM agents (rof Bet C)
- [future-cost-comparison-norms](../research/future-cost-comparison-norms.md) — Future cost-comparison norms — comparing dollar cost when some rows log no tokens · **superseded → refresh-cost-comparison-norms**
- [future-infra-failure-taxonomy](../research/future-infra-failure-taxonomy.md) — Infra-failure taxonomy for agent benchmarks — how SWE-bench, Terminal-Bench, HELM, AppWorld separate infrastructure from
- [future-iterated-protocols](../research/future-iterated-protocols.md) — Naming, reporting, and fairness conventions for iterated (multi-pass + feedback) agent protocols
- [future-multiswe-rust](../research/future-multiswe-rust.md) — Multi-SWE-bench Rust subset — eval conventions, infra caveats, oracle pre-flight, calibration
- [future-reward-hacking-judging](../research/future-reward-hacking-judging.md) — Reward-hacking judging on the Terminal-Bench 2.1 leaderboard — what gets flagged, how agents cheat, and how rof stays cl
- [future-sources-freshness](../research/future-sources-freshness.md) — SOURCE FRESHNESS AUDIT — 10 future-*.md briefs
- [future-task-scaffold-dif](../research/future-task-scaffold-dif.md) — Task x harness interaction — DIF/IRT framing, statistical treatment, reporting guidance
- [future-vision-tool-probing](../research/future-vision-tool-probing.md) — Tool-mediated visual probing (text-only + scripts) vs VLM agents on layout/design-recreation
- [refresh-budget-termination](../research/refresh-budget-termination.md) — 2026 agent budgets, wall caps, and at-cap termination for long-horizon coding/terminal tasks
- [refresh-cache-economics](../research/refresh-cache-economics.md) — Prompt-Caching Pricing — Current Rates + Page Dates
- [refresh-cost-comparison-norms](../research/refresh-cost-comparison-norms.md) — Refreshed cost-comparison norms — 2026 sources only

