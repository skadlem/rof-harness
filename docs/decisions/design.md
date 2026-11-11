PREMISES:
  problem: A coding-agent harness (rof) seeking competitive capability on pinned external tasks, judged as native-reward solve rate at a matched model, with cost a guardrail (<=1.3x the reference's tokens per solved task). "Solved" is the task's own verifier reward file, never the model's claim. One owner-operator; no on-call. Current position: 2/10 solved on a 10-task pilot at the larger of two recipes ever run; rivals unresolved at n=2 tasks; $3.12 of authorized spend remains.
  rationale: The harness's own evidence says runs die on a self-imposed cumulative-token cap, not on task difficulty or context limits. A prior offline recompute showed token-death runs die at a median $0.075 = 1.9% of the approved per-instance $4 envelope, with cache reads 46.5% of the counter at the kill. A fresh 253-dump analysis (this session) confirms the direction without depending on any context-window claim: the cap that kills is cumulative across ~27-40 requests, so it cannot be a context rule at all. Two envelope experiments were already run; both are contested (details below). What would show the envelope matters: a matched arm where token-terminal deaths fall AND native rewards rise.
  decision: Which single experiment, if any, should consume the $3.12 of remaining authorized spend?
  options: |
    A. Envelope re-run at the 2026-practice band. Control 300k gross / 60 steps / 120 actions (the frozen recipe, the only one with a 253-dump corpus). Treatment 3,000,000 gross / 250 steps / 500 actions, with a hard $3 per-instance cap armed. 3-5 cells, 2 arms, 2 reps = 12-20 runs.
    B. Envelope re-run at the pre-registered amendment the arm's own doc names ("candidate amendment 3: 600k miss-plus-out + 120/240"). Control as in A; treatment 600k miss-plus-out / 120 steps / 240 actions. Same cells and reps.
    C. Do nothing on the envelope: treat the existing kill rule as final, spend nothing on budget, and put the remaining budget (or none) into a capability-side lever for semantic deliverable failures (the measured blocking class: a task whose only stable failing test in every row is "the deliverable must be an R script using rstan").
    D. Do nothing at all: bank the $3.12, publish the two completed phases, revisit when a slice with rival evidence exists.
  driver: Which option buys the most DECISION VALUE per dollar - i.e. what would most change what we do next - given that the two existing envelope experiments are internally strong but one is void and the other is n=4 cells? Not which option is cheapest, and not which is most likely to solve a task.
  constraints: |
    - $3.12 total remaining; spend is irreversible.
    - Measured cost per run: $0.10-0.11 on the proxy price table at 87-113 steps; $0.03-0.08 at 60 steps. 12 runs ~= $1.3-1.5; 20 runs ~= $2.2-2.8.
    - One change at a time per arm; rows of different budget shapes never pool; a solve is only the task's reward file.
    - No hidden-verifier feedback into runtime decisions.
    - Release pin is TB4.1.0 CPU; the two candidate cells below are not in that official dataset, so any result is "measured-local TB-family", never a TB4.1 subset.
    - Single owner runs spend; no parallel writers on a crate.
    - The endpoint used for the two weakest cells (vLLM gateway) has NO price-book entry, so its dollar counter is inert by design; dollars there are a labelled analysis proxy only.
  reversibility: Irreversible for dollars (spent is spent) and for corpus (a re-run adds rows that can never be pooled with the void 18). Reversible for code: every mechanism is already implemented and default-off. Undoing the code costs nothing; the spend is the only one-way door.
  evidence: |
    MEASURED, internal:
    - dphn-cap arm (18/18 rows, 2 reps x 3 cells, pre-registered): the joint arm (miss-plus-out, 120 steps/240 actions, SAME 300k cap value) produced 0 of 4 cell-reps up, no treatment-caused solve, and failed the guardrail at 1.79x and 3.05x tokens per test passed. The registered KILL RULE triggered: "the envelope axis is dead for this workload". Its own reading: at 120 steps the CAP VALUE binds, not the step envelope, so a true joint move must raise the cap value too.
    - budget-shape arm (glm, 4 near-miss cells, pre-registered): miss-plus-out alone FAILED its gate (0/4 solves both arms, arm cost 1.42x). Mechanism CONFIRMED: token-dead cells migrated to the step cap at 1.6-1.7x gross input. Structural read: "both caps bind nearly together; raising either alone just moves the wall to the other."
    - 253-dump replay (this session): 135 token-death runs; final-turn input median 11,099 tokens; replaying cache-read weight 0 at the same 300k gives median +16 turns but the STEP cap would win in 105 of 135; 77 of 144 token runs were still mutating the tree in their last 3 steps. 0 of 135 have a RECORDED miss-plus-out crossing - all Q2 figures are extrapolated.
    - 2026-practice refresh (2026-only sources, dated): mini-SWE-agent ships step_limit 250 / cost_limit $3.0; 2026 harness studies standardize on 250-300 steps and 6h walls; 300k tokens sits inside the 2026 "small single-module fix" band; our 30-step cap is ~8x below the 2026 default. NO 2026 source cleanly ablates step-cap height vs solve rate for coding agents. Budget-awareness prompting beats a 20%-higher cap (+18.6pp resolve at the same 50-call budget, arXiv:2511.17006v2 / COLM 2026).
    MEASURED, internal, contradicting the above:
    - README marks the dphn-cap 18-run arm "Void, re-measured pending": those runs predate the workspace-fidelity fix, and the agent was seeded from the task's Docker BUILD CONTEXT instead of the image's /app, "so on these cells the verifier tests it was measured against could not move". If true, the capability half of the kill ("no test moves") is unmeasured, while its mechanism half (the wall migrates; the cap value binds) is untouched because it does not depend on test movement.
    - The harness's own budget-awareness/wrap-up nudge was measured DEAD internally (--steer: no named-test change on either cell at 2.64x tokens per solved task), contradicting the external +18.6pp result - though the mechanisms differ (initiative steering vs continuous remaining-budget display, and the harness already sends a per-request budget line).
    - On video-processing the two substantive jump-detection tests failed in ALL SIX rows including a 113-step, 1.4M-token run; the only test ever gained was a trivial import check. On mcmc, the blocking test is semantic and stable across all arms.
    ASSUMED, not measured:
    - That any cell would convert on a faithful pipeline. The one void arm is the only joint-arm evidence that exists.
    - That the byte-identical re-run of a void arm on the fixed pipeline produces comparable rows (the fix changes what the verifier sees).
  cells: |
    In-container measured, frozen recipe (tests passed/total):
    - video-processing 2/5, 2/5 - headroom, but the two substantive tests failed in every arm including 113 steps.
    - kv-store-grpc 0/7, 0/7, 7/7 - bimodal, env-mutation family.
    - sanitize-git-repo 0/3, 3/3 - bimodal.
    - query-optimize 0/6, 0/6, 1/6 - noisy.
    - mcmc-sampling-stan 5/6 on this endpoint - near-miss but semantically blocked (stable failing test across all arms).
    - install-windows-3.11 1/4 and mteb-retrieve 1/2 - both carry known grading/infra hazards (root-owned artifact halt; verifier network errors).

CONTEXT:
  - Both completed phases (P0: fail-closed spend guard + agent-phase boundary record; P1: 253-dump envelope replay) are green and reviewed. They establish that the dollar guard now cannot silently be inert, and that the corpus is 253 dumps not the 83 the prior recompute describes.
  - The prior recompute's scripts are gone, so its numbers are conclusions without a rerun path; P1 re-derived the counter model on what survives (135/135 gross token deaths reproduce).
  - Two independent internal experiments already spent money on the envelope question. One is void; one failed its gate but confirmed the mechanism. Neither tested "raise BOTH walls, including the cap value" on a faithful pipeline.
  - The competing treatments differ in kind, not degree. Option B keeps the frozen counting shape and nearly doubles the allowance (600k miss-plus-out ~= 1.2M gross at the observed 58% cache share). Option A changes the counting unit to gross AND goes to 2026 practice (3M gross, 250 steps), which makes its rows incomparable to every existing row and to the 253-dump corpus.

RELATIONS:
  - A -> forecloses pooling with all existing rows (different unit, different envelope); enables comparison with 2026 practice (250 steps, 100k-2M band) and fills the literature's own stated gap (no coding-agent step-cap ablation).
  - A -> costs more per run (3-5x at 250 steps), so it buys fewer cells: 3 cells x 2 reps at most within $3.12.
  - B -> preserves the counting shape, so rows stay interpretable against the 253-dump corpus; but at 600k miss-plus-out with 120 steps the existing evidence says the cap value may bind again (the J arm used the whole cap and reached 87 then 113 steps).
  - B -> costs ~$0.10-0.15/run, so it buys 4-5 cells x 2 reps.
  - C -> leaves "is the envelope exhausted?" permanently ambiguous; the only joint-arm evidence stays void; but avoids spending on a lever whose mechanism half already says "wall migrates, tests do not".
  - D -> costs nothing and loses nothing measured; keeps the $3.12 for a slice where a rival comparison is possible, which is the actual product question.
  - REQ-DECISION-VALUE -> A or B : only a faithful-pipeline joint arm can separate "envelope exhausted" from "envelope untested".
  - REQ-COST -> A : 250-step runs at 3M tokens are the most expensive option per run and the least certain to finish inside $3.12.

REJECTED:
  - Repeat the miss-plus-out-only shape arm - rejected because it was run pre-registered on 4 cells and failed (0/4 solves, 1.42x cost), and the 253-dump replay independently shows the step cap wins in 105/135, so the shape alone cannot buy turns on most rows.
  - Repeat the J arm exactly (300k miss-plus-out + 120/240) - rejected because that arm's own report says the cap VALUE binds at 120 steps, so the same treatment would reproduce the same non-result.
  - Raise the token cap alone to the dollar-equivalent 16.3M - rejected because 54x the current value is far outside every 2026 practice band (100k-2M), which makes it uninterpretable as a capability experiment even if it produces solves.
  - Spend on a frontier-model run to isolate model class from harness - rejected because the product question fixes the SAME model, so that experiment cannot answer it.
  - Pool new rows with the void 18 - rejected: different verifier visibility, different estimand.

UNKNOWN:
  - Whether the README's void caveat really invalidates the dphn-cap test counts, or only the rows where the build-context seed differed from the image /app. Nobody has measured which of the 18 rows were affected.
  - Whether any available cell can convert. video-processing's substantive tests failed even at 113 steps; kv-store-grpc and sanitize-git-repo are bimodal for reasons nobody has diagnosed (budget, environment, or seed).
  - Whether the harness already implements the literature's winning mechanism (continuous budget display) or only a weaker variant - if the former, the external +18.6pp lever is already spent and only the cap height remains.
  - The true served context window of every endpoint used; the only hard bound is a 95,236-token prompt that was accepted.
  - Whether 250-step runs finish inside a $3.12 envelope at all; no run in the corpus exceeded 113 steps.
