# Literature: harness interventions for semantic deliverable-form failures (2026)

**Date:** 2026-11-11 · **Scope:** coding agents only; 2026 work only unless explicitly marked **[pre-2026]**.
**Provenance labels:** **measured** (source + number), **inferred** (derived from measured), **chosen** (no anchor).

---

## 0. The decision this informs

*Which harness intervention should ROF add to convert "solves the problem, ships the wrong artefact form" failures — where one test such as `test_r_script_created_and_used_rstan` fails stably in every row across three budget arms — into passes, at what token cost, and is that intervention's benefit conditional on budget?*

**Restated as a build question:** given a frozen task statement and a verifier that already names the missing property, is the cheapest measured win (a) extract the deliverable requirements up front, (b) gate submission on an executable predicate plus one bounded repair, (c) feed more repository evidence at edit time, or (d) nothing — because the agent already knows the requirement and simply never checks it?

The literature answers (d) is wrong and (c) is nearly worthless; (a)+(b) are the same mechanism viewed at two points in time, and the measured cost of (b) is *negative* in tokens.

---

## 1. Summary

Four independent 2026 measurements agree on the shape of this failure and on what moves it.

First, the failure class is large and is **not** a knowledge problem: MTAC-IFBench (arXiv 2609.14992, 2026-09-14) measures a **constraint satisfaction rate of 80.4%** against a **constraint-instance solve rate of 12.7%** for its best model (Table 2) — agents obey four of five stated constraints and still fail to satisfy all of them on 87% of tasks. On the functional side the same paper reports BSR 98.0 vs F-ISR 37.0 (Table 3): the code works, the deliverable does not.

Second, the interventions that supply *executable* requirements and *gate on them* are the only ones with replicated, ablation-isolated gains. ECLoop (arXiv 2607.28815, 2026-07-30) compiles per-task evidence conditions and blocks edit/submit until they are satisfied: **+11.8 pp** (56.2→68.0) on 500 SWE-bench Verified instances, and its own ablation shows the structured form beats a natural-language spec by **10 pp** (58 vs 68, Table 4). It does this while *reducing* tokens by 12.1%.

Third, post-hoc repair without front-loaded requirements is near-worthless: Measuring Where Intent Gets Lost (arXiv 2610.37830, 2026-10-27) finds that extractor/critic, reviewer/ensemble and rulebook-repair stages added **−1.0 to +0.7 points** after generation, while capturing the requirement before generation moved pass rate from 39.3 to 72.9 (Table 2).

Fourth, **budget is the wrong lever and scaffolding does not compound with it.** Causal attribution of inference-time resources (arXiv 2610.28834, 2026-10-20) measures the marginal value of computation falling to **0.007**, and **21 of 36 pairwise interactions significantly negative, none positive** (Table 5). This is the mechanistic explanation for the caller's null result on budget arms.

**The one thing to do differently:** stop treating this as a budget or context problem and implement requirement-derived, machine-checkable completion conditions evaluated against the *final artefact*, with a hold-and-repair path. The measured analogue (ECLoop) costs less than baseline, but no 2026 paper has applied such conditions to artefact-form properties, which is precisely the untested step.

---

## 2. Landscape

| Approach family | Representative 2026 work (id, date) | Mechanism | How evaluated | Main limitation for *this* problem |
|---|---|---|---|---|
| **Requirement → executable condition set, gating commit** | ECLoop, 2607.28815, 2026-07-30 | Per-task conditions ⟨predicate, evidence, event-pattern⟩; commitment actions postponed while gap ≠ ∅; 3 holds then audited fallback | 500 SWE-bench Verified × 2 harnesses × 2 models + 100-instance ablation | Conditions are *trajectory-event* predicates (callers inspected, tests checked), **not** artefact properties |
| **Requirement → checklist, LLM-judged** | OctoBench 2601.10343 (2026-01-15); MTAC-IFBench 2609.14992 (2026-09-14) | 7,098 / 91-per-task binary checklist items; ISR = all-or-nothing | Benchmark + feedback-correction experiment (OctoBench Table 5) | Judge-mediated; no token cost reported; feedback arm measured on an already-failing subset |
| **Front-loaded requirement elicitation** | Measuring Where Intent Gets Lost, 2610.37830, 2026-10-27 | Ask/elicit the full interface spec before generating | 600 contracts, 500 held out, 4 generators | Needs an interlocutor; smart contracts, not repo coding |
| **Verifier-in-the-loop as reranker** | Agentic Rubrics, 2601.04171, 2026-01-07 | Rubric agent builds `rubrics.yaml` from the repo; binary-weighted score reranks 16 rollouts | Best@16 on SWE-bench-style tasks | Selection only (16× generation); no repair |
| **Repository evidence packaging** | RepoNorm 2610.07757 (2026-10-06); RepoNav 2609.08355 (2026-09-08); Context-Augmented 2605.08112 (2026-04-27) | Structure retrieved context into norms / file-centred navigation / product decisions | 121 tasks / 560 instances / 8 tasks | Compliance rises; **functional** gain not significant (RepoNorm) or unmeasured (RepoNav) |
| **Tool-description optimisation** | MCP descriptions 2602.14878 (2026-05-31); AT-AT 2610.34679 (2026-10-26) | LLM rewrites tool docs from traces | MCP benchmark / τ² + TerminalBench-2 | Median +5.85 pp with 16.7% regressions; AT-AT significant in 1 of 3 domains |
| **Harness minimalism / anti-scaffolding** | How Much Harness, 2609.40303, 2026-09-30 | Ladder of planning/delegation/memory/search arms | MLE-bench fixed30, 24 h | ML-engineering tasks; no deliverable-form arm |
| **Resource attribution (negative interactions)** | BRA, 2610.28834, 2026-10-20 | 2³ factorial over computation/information/feedback at frozen failed checkpoints | 194 function + 126 repo-level failures, 6 models | Measures marginal value *given a failure*, not end-to-end solve rate |

---

## 3. Key papers

### Angle 1 — Requirement / deliverable extraction

#### 3.1 OctoBench: scaffold-aware instruction following — 2601.10343
**Source:** arXiv:2601.10343v2, preprint (no venue listed), v1 2026-01-15, v2 2026-01-16.
**Problem:** agents complying with some of many concurrent instructions.
**Method:** 3 scaffold types, 34 environments, **217 instances, 7,098 checklist items** (avg 32.7/instance) (Table 1). Compliance is judged per item; **ISR is "a strict, all-or-nothing metric that counts an instance as successful only if all verifiable checklist items pass"**, CSR is item-level (§3.3).
**Result (measured):** best model Claude-Opus-4.5 **ISR 28.11 / CSR 85.64** avg@3; Gemini-3-Pro 14.68/80.94; ChatGLM-4.6 12.73/80.38 (Table 2). Feedback-correction experiment (§4.3.2, Table 5): failed checks converted to structured error feedback injected as explicit constraints — ChatGLM-4.6 ISR **21.37→38.17 (+16.79)**, CSR 87.13→89.82 (+2.69); Claude-Opus-4.5 **38.40→45.60 (+7.20)**, CSR 88.98→90.60 (Table 5).
**Cost:** token/call cost **not reported**. Per-instance timeout typically 30 min; 3 judge models.
**Limitation:** Table 5 is computed on *partial-failure instances only*, so the +16.79 is conditioned on already-failing runs and **overstates** a full-benchmark delta **[inferred from §4.3.2 wording]**. Checklist items are LLM-judged (>95% human-audited as objective/binary — reported).
**Applicability:** this is the closest thing to a *quantified* "tell the agent which deliverable checks it failed, once" in a coding-agent setting — the direct analogue of a bounded repair driven by `test_r_script_created_and_used_rstan`.

#### 3.2 MTAC-IFBench: instruction-following in multi-turn agentic coding — 2609.14992
**Source:** arXiv:2609.14992v1, preprint, 2026-09-14.
**Problem:** process/constraint compliance, explicitly separated from functional correctness.
**Method:** 100 instances, 7.04 turns, **91.33 constraints/instance** (Table 1); 6 primary + 18 secondary categories including "Format mandates that outputs adhere to structural templates or markup schemas", "File Encoding for specified character encodings", "Orchestration enforces the sequencing … of multi-step actions".
**Result (measured):** under Claude Code, GLM-5.2 **CSR 80.4 / C-ISR 12.7**; Claude-Opus-4.6 78.6/8.7; Qwen3.5-27B 60.6/1.1 (Table 2) — "Even the leading LLM, GLM-5.2, fails to follow about 20% of the constraints." Functional view (Table 3): Claude-Opus-4.6 **BSR 98.0, FSR 90.2, F-ISR 37.0**. Harness swap (Table 5): same model, Claude Code → OpenCode, **CSR 80.4→76.8 and C-ISR 12.7→6.7**.
**Cost:** interventions not reported; judge-agent cost described only qualitatively. Curation cost "$2,400" (data, not intervention).
**Limitation:** benchmark only — **no intervention with a measured delta**. Evaluator agreement 0.921/0.939/0.887 (Table 4) bounds how much of the 12.7% is judge noise.
**Applicability:** provides the denominator for the caller's problem: ~80% of *individual* constraints already pass, ~13% of *instances* are fully compliant. A single stable failing test is exactly the C-ISR regime.

#### 3.3 Measuring Where Intent Gets Lost in Agentic Smart Contract Synthesis — 2610.37830
**Source:** arXiv:2610.37830v1, preprint, 2026-10-27.
**Problem:** where specification information is lost between intent and artefact.
**Method:** 600 contracts (100 dev, 500 held out); compares description richness, elicitation-first pipelines, and post-generation repair stages against a reference model.
**Result (measured):** information is lost **before** generation: reference model catches 65.9% from a short description vs 87.5% from a complete document (gap 21.6 pts [15.1, 29.3]). Generated contracts: short description 55.6%, interviewer 79.4%, **checklist 76.0%**, complete document 91.0%. Post-generation stages "add at most about one point": extractor/critic **−1.0**, reviewer/ensemble **−2.5 / 0.0**, rulebook repair **−0.2**, client triage **+0.7**; security audit-and-refine **−12.8 pts [−16.3, −9.5]**. Table 2: one-shot 53.9, original 39.3, **elicitation-first 72.9**. Failure taxonomy: **45% missing function / wrong argument types, 20% unobserved value change** vs 29% wrong outcome, 6% wrong value change — i.e. **65% interface/observability, 35% logic [inferred sum from §5]**.
**Cost:** not given in dollars/tokens.
**Limitation:** smart contracts with a *client* who can answer questions; the caller's task statement is static and unanswerable. Single-system, own benchmark.
**Applicability:** strongest evidence that **post-hoc critics/repairs do not recover a requirement that was never captured** — which is the failure mode of "just run the tests again and fix what fails" for semantic deliverable gaps.

### Angle 2 — Executable completion predicates / verifier-gated repair

#### 3.4 ECLoop: evidence-conditioned execution layer — 2607.28815 ★ strongest
**Source:** arXiv:2607.28815v1, preprint, 2026-07-30.
**Problem:** agents commit to edits/submission before the information the task needs has actually been gathered.
**Method:** compiles per-task evidence conditions from the issue + repository structure (AST, call graph, class hierarchy). Each condition φ_i = ⟨b_i, v_i, R_i, sat_i⟩ where **R_i "specifies the event pattern that must appear in the runtime agent trajectory"**; satisfaction is "computed from the runtime agent trajectory … rather than from the model claiming that it completed an investigation". Information-gathering actions proceed freely; **commitment actions (edits, final submission) are gated** while the unsatisfied gap U_t(a_t) ≠ ∅. **Up to three holds per commitment target**, then an audited fallback proceeds and records the unsatisfied conditions. Only the *remaining* gap is shown to the agent.
**Result (measured):**
- Table 1, mini-swe-agent v2, **500 SWE-bench Verified instances**: GPT-5-mini **56.2→68.0 (+11.8 pp, RRU 26.9%)**; MiniMax-M2.5 **75.8→80.6 (+4.8 pp)**; both p<0.001.
- Table 2, Codex CLI: GPT-5-mini **40.4→50.8 (+10.4)**; MiniMax-M2.5 **74.8→79.8 (+5.0)**.
- Table 4 (ablation, 100 instances, GPT-5-mini): baseline 47 → full **68**; **−commitment check 58**; −guidance 63; −state update 59; **natural-language spec 58**.
- Regressions: "newly resolves 33 and 68 failures for MiniMax-M2.5 and GPT-5-mini, respectively, while regressing on only **9 successes in each case**" → **9/379 = 2.4%** and **9/281 = 3.2%** of previously-solved runs broken [inferred division].
- Cost (Table 3): tokens **179K→157K (−12.1%)**, $0.047→$0.042 (−10.2%) for GPT-5-mini/mini-swe-agent; **−5.4%** tokens for Codex CLI. Includes the per-task specification-compilation call.
**Limitation:** conditions are *evidence-gathering* predicates over trajectory events. The paper explicitly does not express "file at path X", "uses library Y", or "specific invocation" — **artefact properties fall outside the described scheme [confirmed by direct query to the paper's text]**.
**Applicability:** this is the architecture to copy — pre-run condition compilation, gate the *submission* action, one bounded hold path, cheap. What must be replaced is the condition language: predicates must evaluate against the final workspace, not the trajectory.

#### 3.5 Where Should a Stuck Agent Spend Its Next Tokens? — 2610.28834
**Source:** arXiv:2610.28834v1, preprint, 2026-10-20.
**Problem:** causal value of computation vs information vs feedback at a failed checkpoint.
**Method:** Banzhaf Resource Attribution — controlled 2³ factorial (Computation C, Information I, Feedback F) with Möbius decomposition. **194 function-level failures + 126 SWE-bench Verified failures**; six models (Qwen3-4B/14B/32B, DeepSeek-V3.2, GPT-5.6, Claude Sonnet 4.5).
**Result (measured):** computation value **0.131 → 0.007** (Table 4). Information **0.06–0.14**; feedback **0.007–0.033** overall but **0.362–0.402** on the 40 hardest checkpoints. Dose-response: **256 tokens give 61% and 512 tokens 82% of the 2048-token gain; 4096 tokens add nothing (0.600 vs 0.610)**. At repository level with an **audited specification**, I = **0.048, 0.083, 0.088, 0.103** (Table 1), while **retrieval = −0.002 to 0.009**. Substitution: **21 of 36 pairwise interactions significantly negative, none positive**; Γ_CI −0.10 to −0.14 (Table 5). Models self-select feedback in **95–100%** of cases yet realise only **10–27%** of oracle gain, **worse than random by 0.029–0.064** (p<0.001, §5.5).
**Cost:** attribution study; no intervention cost.
**Limitation:** measures marginal value *conditional on a frozen failure*, not end-to-end solve rate. Function-level tasks are MBPP+/HumanEval+, not deliverable-form tasks.
**Applicability:** two direct hits. (i) Budget is not the lever — computation value is ~0.007 at the point of failure, matching the caller's observed null across three budget arms. (ii) **Retrieved code is worth ~zero at repository level; an audited specification of what the code must do is worth 0.048–0.103.** That is the information-theoretic case for encoding the deliverable requirement rather than more repo context.

#### 3.6 Agentic Rubrics as Contextual Verifiers — 2601.04171
**Source:** arXiv:2601.04171v1, preprint, 2026-01-07.
**Problem:** execution-free verification of patches against specification, not just tests.
**Method:** an expert rubric agent interacts with the sandboxed repo to synthesise `rubrics.yaml` along four axes — **File Change, Spec Alignment, Integrity, Runtime** — with weights w_i ∈ {1,2,3}; an LLM judge scores binary s_i; S = Σw_i s_i / Σw_i. Used to **rerank K=16 rollouts**; no repair loop is described.
**Result (measured):** Figure 2/§4.1 — Qwen3-32B **Best@16 40.6%** vs Patch Classifier 37.1% (+3.5) and Agentic Patch Similarity 35.0% (+4.6); Qwen3-Coder-30B **54.2%** vs 50.2% (+4.0). Rubric vs ground-truth test **ROC-AUC 0.886, PR-AUC 0.722** (§4.2.1). Critical for this question: **when tests pass but rubric < 0.7, 46% of those patches are low-utility** (§4.2.2/Figure 5) — a direct measurement that *passing tests do not imply a correct deliverable*, with subcategories including "Over-Specified 4.0%", "Spec Conflict 3.8%", "Test Mismatch 1.8%". Ablation: removing repository access costs **4.0 points** (Qwen3-32B) and **1.4 points** (Qwen3-Coder-30B) (§5.2). Flakiness: **2%** (Sonnet-4.5) vs **9%** (Qwen3-32B) (A.4).
**Cost:** Table 4/A.3 — **$0.293/instance, 22.9 calls** for rubrics (vs $0.736 for patch similarity, $0.515 for test generation).
**Limitation:** reranking only; 16× rollout cost dominates; no measured repair path.
**Applicability:** the rubric's "Spec Alignment" and "File Change" axes are literally the deliverable-form predicate; but as a *reranker* it cannot fix a single-rollout harness. The 46% number is the best available estimate of how much a tests-only verifier misses.

#### 3.7 The Verification Horizon: No Silver Bullet for Coding Agent Rewards — 2606.26300 (dissenting)
**Source:** arXiv:2606.26300v1, preprint, 2026-06-24 (Binghai Wang et al.).
**Problem:** verification is now harder than generation; every verifier is a proxy for intent.
**Method/claim:** characterises signals on scalability, faithfulness, robustness; studies test, rubric, user-as-verifier, and agent verifiers.
**Result (measured):** with hack monitoring, average **"hacked resolved" 28.57% → 0.56%**, hack rate **37.76% → 1.31%**, clean resolved **40.22% → 60.53%** (Table 3). Behaviour–success association: **evaluation-harness tampering 41.47% resolved (−18.52 vs 59.99% baseline)**, **test-oracle tampering 47.29% (−12.70)** (Table 2). Evaluator-filtered RFT 23.52 vs random 21.61 at 9,139 samples (Table 10). Conclusion: "**no fixed reward function can remain effective as policy capability continues to grow; and verification must co-evolve with the generator.**"
**Limitation for us:** quantifies **false positives** (verifier passing wrong work) but reports **no headline number for false negatives** (verifier failing correct work) — the paper confirms only qualitative discussion in §2.1 and §5.4.
**Applicability:** cautions against treating a test-derived predicate as ground truth, and explains why a *stable* failing test is valuable (it has not yet been gamed) — but supplies no false-rejection rate.

#### 3.8 Self-Execution Simulation Improves Coding Models — 2604.03253 (dissent on verifier-in-the-loop)
**Source:** arXiv:2604.03253v1, preprint, 2026-03-11 (header also says April 7, 2026).
**Method:** train the model to predict stdout of (code, stdin) — SFT on NLEX + RLVR — then use that simulation to select or fix candidates.
**Result (measured):** train-time gains large (CruxEval-O Qwen2.5-7B pass@1 48.5→75.5, Fig. 3). **Inference-only: the scaffold *hurts*** — Qwen3-32B DMC pass@1 **−10.6**, LCB-IO **−20.1**; CWM −4.8 / −7.4 (Table 4).
**Applicability:** a harness cannot bolt on self-verification that the model was never trained for and expect a gain. Any ROF repair loop must trigger on an **external, deterministic** predicate, not on model self-assessment.

### Angle 3 — Repository evidence bundles at edit time

#### 3.9 Acquiring and Verifying Repository Norms for Coding Agents — 2610.07757
**Source:** arXiv:2610.07757v1, preprint, 2026-10-06.
**Method:** extracts "repository norms" — "requirements, recommendations, and supported source-code conventions that guide how changes should be implemented, tested, and documented" — as n = (r, s, c, e); delivered as a static Markdown package (Accepted-Only by default). 121 tasks, 14 versions, 160 reference norms (Table 1).
**Result (measured):** Norm Compliance Rate vs raw repo context: **DeepSeek +10.77, Qwen +7.42, Gemini +10.03** (Table 2), with the "Contribution" sub-metric **+45.44 / +34.94 / +31.64**. **But functional pass-rate Δ is +9.09 / +7.44 / +5.79 pp, and "no comparison significant after Holm"** (Table 3); joint Δ +4.96 / +2.48 / +0.83. Precision of accepted-only norms 258/300 = 86.00% (Table 4).
**Cost:** acquisition **134.52M input / 7.67M output tokens, 2.14 h**; full condition run 463.83M/12.70M, 67.12 h (Table 5).
**Limitation:** norms capture *conventions and requirements*, and improve compliance with them, but the capability gain is not statistically established.
**Applicability:** the best evidence that **structured, requirement-shaped repo context raises form compliance without (significantly) raising functional success** — i.e. exactly the split the caller observes. Also a warning: packaging the requirement improves the metric that measures the requirement, which may be circular.

#### 3.10 RepoNav: file-centred navigation — 2609.08355
**Source:** arXiv:2609.08355v1, preprint, 2026-09-08.
**Method:** post-retrieval interface that serialises top files into [ANCHORS] / [GLIMPSE] / [CANDIDATE_TARGETS] plus a `list_symbols` tool, instead of flat snippet lists.
**Result (measured):** LocBench, 560 instances, 7 models: average **Function Acc@5 0.484 → 0.557** (snippet → RepoNav), File Acc@5 0.753 → 0.774 (Table 1). GPT-OSS-120B: Acc@5 52.13 vs 32.61 file-only (Table 2). **Trace tokens 62.4k vs 31.7k (+97%)**, 25.3 s vs 15.5 s. SWE-QA-Bench totals 82.90 vs 81.15 (+1.76) (Table 3).
**Limitation:** localisation metrics only; **no end-to-end solve rate**.
**Applicability:** better repo evidence buys localisation at ~2× tokens and does not touch deliverable form.

#### 3.11 Context-Augmented Code Generation — 2605.08112
**Source:** arXiv:2605.08112v1, preprint, 2026-04-27.
**Method:** retrieval of team decisions/personas/specs via 8 tools at spec-generation time and mid-build consultation.
**Result (measured):** "Decision Compliance" **19/41 (46%) → 39/41 (95%), +49 pp**; blocking violations **5 → 0**; tasks at 100% **2/8 → 6/8** (Tables 2–3). Per-decision (Table 5): baseline is **100% on code-visible decisions and 0–33% on context-dependent ones**.
**Cost:** $4.13 → $5.28 (+28%), tokens **3,902K → 3,867K (−1%)**; cost per merge-ready task $2.07 → $0.66.
**Limitation:** **8 tasks, 41 decision points, 48 runs**, vendor preprint; the authors state the effect "is the combined effect of all three" factors and **do not isolate them**. Smallest sample in this set; treat as directional.
**Applicability:** the largest compliance delta found anywhere in 2026 (46→95 on context-dependent decisions), at +28% cost and flat tokens — but it supplies *organisational decisions*, not *machine-checkable deliverable predicates*, and is uncontrolled.

### Angle 4 — Tool-description / interface optimisation

#### 3.12 MCP Tool Descriptions Are Smelly — 2602.14878
**Source:** arXiv:2602.14878v3, preprint (cites an ACM June 2026 date; DOI placeholder), 2026-05-31.
**Method:** six smells (Unclear Purpose, Missing Usage Guidelines, Unstated Limitation, Opaque Parameters, Underspecified/Incomplete, Exemplar Issues), each triggered at mean < 3 on a 5-point rubric; descriptions rewritten by a **GPT-4.1-mini augmentor** grounded in execution traces.
**Result (measured):** cross-model **median ΔSR = +5.85 pp; improved in 54.17%, regressed in 16.67%, unchanged 29.16%; McNemar p = 0.02** (Table 7). Median ΔAE **15.12** (Table 8). **Input tokens +21.85–41.43%, output +11.10–69.00%** for Qwen3-Next (Table 9).
**Cost:** authoring cost not quantified; full benchmark rounds ~200–300M tokens / $75–600.
**Limitation:** LLM-written augmentation, single taxonomy; ~1-in-6 affected tasks got *worse*.
**Applicability:** relevant only if some of the caller's deliverable gaps are caused by tool surfaces (e.g. a shell/exec tool whose description never mentions that the task expects an R script). The 16.67% regression rate is the reason to A/B any description edit rather than ship it.

#### 3.13 AT-AT: Automatic Tool Optimization from Agent Traces — 2610.34679
**Source:** arXiv:2610.34679v1, preprint, 2026-10-26.
**Method:** a coding agent (Pi) reads evaluated traces offline and edits tool descriptions (and implementations when permitted); **one pass, no new rollouts**; agents held fixed.
**Result (measured, n=30 trials/task):** Airline **52.17 → 59.17 (p = 0.2305)**; Telecom **29.50 → 36.75 (p = 0.0033)**; Retail **77.00 → 74.67 (p = 0.2313)**. Optimizer cost **$2.78 / $10.69 / $1.93**. TerminalBench-2 held-out: **pass@1 49.32 → 51.36**, pass@3 59.43 → 61.34 (Table 3). Production MCP servers: Eng. Context pass **54.5% → 72.7%**; tool errors 73 → 37 (Table 4).
**Limitation:** **1 of 3 domains significant**; Retail regressed.
**Applicability:** the honest price of an LLM optimiser for tool/interface surfaces: ~$2–10 per domain for +2 pp held-out on a coding-agent benchmark, with a real chance of no significant gain.

### Angle 5 — Harness vs budget interaction

#### 3.14 How Much of a Harness Does a Strong Agent Need? — 2609.40303 (dissent)
**Source:** arXiv:2609.40303v1, preprint, 2026-09-30.
**Method:** harness ladder on OpenCode v1.15.6 — Chat, Oneshot, **Malena** (single session across the whole budget, minimal tools), search variants (Chain/Greedy/UCB1/Best-of-N), orchestration (delegation D, 3-way parallel P3, broadcast B), ablated extras (nudges N, retrieval memory, skills), and four external harnesses.
**Result (measured, 24 h budget, MLE-bench fixed30):** Malena **69.74 percentile / 62.5% medal** vs Arbor 59.26/45.8, AiScientist 61.39/47.1, MLEvolve 54.32/39.2, ScienceFlow 47.59/24.4 (Table 15). Adding machinery *hurts*: +D 63.62/45.2; +B+P3 58.91/33.3; +N 63.85/48.9; −J (removing a tool) 70.47/62.1 (Tables 1, 9). Cost: Malena **$12.12** vs AiScientist **$1.95**.
**Limitation:** **ML-engineering tasks, equal wall-clock budget, no deliverable-form arm**; single system; authors' own harness wins.
**Applicability:** a hard prior that generic scaffolding (planners, delegation, memory) does not add capability. It does **not** test requirement gates, so it is not evidence against ECLoop-style conditions.

#### 3.15 Agents Are Systems, Not Models — 2610.01618
**Source:** arXiv:2610.01618v1, preprint, 2026-10-01.
**Method:** 432 cells × 5 runs factorial over information given, model, reasoning effort, budget, verification on four scientific tasks.
**Result (measured):** "**information provided to the agent has the largest effect, exceeding both time budget and model size**" — mean ranks: **Information 1.00, Model 2.67, Reasoning 3.00, Budget 3.33, Verification 5.00** (Table 3; info effects 1.925/1.452/1.496). Verification by prompting rises only **19% → 22%** across prompting levels; ~**three quarters check only format**; with an oracle it "roughly triples" (Fig. 4). **~54% of score variance among genuine attempts is run-to-run noise**; ICC 39.4% / 46.1%. Oracle ablation pooled G 0.822 → 0.923 (Table 12).
**Limitation:** **four non-coding scientific tasks** — the strongest transfer limit in this brief.
**Applicability:** ranks information above budget with measured effect sizes, and shows that *asking* an agent to verify itself barely moves verification rates (19→22%) while an external oracle roughly triples them.

#### 3.16 Measure Before You Manage: agent working memory — 2608.31057
**Source:** arXiv:2608.31057v1, preprint, 2026-08-31.
**Result (measured):** "**none of ten constrained-arm pairs matches on all eight tasks**" for delivered context under equal token budgets (Table E.2); **18 of 24 caps exceed the nominal 15% anchor**; calibration-vs-FIFO Δ = **−1.633 [−2.667, −0.700], Holm p = 0.0146**, but held-out Δ = **−0.500 [−1.250, 0.000], p = 0.5000** — "**calibration gains do not transfer**". Management work: 285 importance calls / 169 summary calls; input tokens 2,140,636 (Full) vs 1,147,832 (FIFO). 55 archived SWE trajectories.
**Applicability:** equal token budgets are not equal delivered context, and context-management wins found on a calibration set failed to replicate. This is the methodological reason the caller should pre-register a held-out slice before believing any context-side result.

#### 3.17 TraceDev: traceability-driven development — 2607.18886 (confounded)
**Source:** arXiv:2607.18886v1, preprint, 2026-07-21.
**Method:** a Validator Agent maintains a requirement→design→code traceability graph; missing links become prompt injections; bounded to 3 traceability repair rounds and 5 test-remediation attempts.
**Result (measured):** ETOUR success **23.40% (ChatDev) → 53.63%**; SMOS 24.74% → 56.82% (Gemini-2.5-Flash, Table 2); tokens 109,541 vs 98,766 (Table 3). 125 use cases, 962 + 1,179 tests.
**Limitation:** compared against ChatDev/MetaGPT — a whole multi-agent framework, not an ablation of the traceability loop. **Cannot attribute the delta to the requirement mechanism.**
**Applicability:** useful only as an existence proof that traceability loops are runnable inside a budget; do not cite the +30 pp as a mechanism effect.

---

## 4. Implications for ROF

**Closest prior art.** ECLoop (2607.28815) is the same architecture ROF would build: compile per-task conditions before the run, gate the *commitment* action, hold at most N times, keep going with an audit trail. OctoBench's feedback-correction arm (2601.10343 §4.3.2) is the same idea without the gate. Agentic Rubrics (2601.04171) is the same idea without the repair.

**What is genuinely different about the caller's setting.**
1. ECLoop's predicates are **trajectory-event patterns** — "inspecting relevant callers before editing a function". The caller's failing test is a **workspace-state predicate** — file exists at a path, contains `library(rstan)`, the script actually runs, a CLI exits 0. No 2026 paper found compiles conditions over the *final artefact* and gates on them. This is a real, defensible increment, and it is also the untested part.
2. The caller has the predicate **already written** by the verifier. OctoBench, ECLoop and Intent all had to *derive* conditions; ROF can lift `test_r_script_created_and_used_rstan` into a pre-submission check. Extraction is the expensive step in the literature and it is free here **[inferred]**.
3. Multi-arm budget ablation with a stable failure is a cleaner identification strategy than anything in these papers.

**What is not novel at all.**
- "Give the agent a checklist of requirements" — OctoBench has 7,098 such items; MTAC-IFBench 91/task. Do not claim this as a contribution.
- "Verify before submitting" — ECLoop, TraceDev (3 repair rounds), OctoBench feedback. The 3-hold-then-fallback pattern is prior art.
- "Retrieve more repo context at edit time" — RepoNav, RepoNorm, Context-Augmented. And it is the weakest lever measured (BRA: retrieval −0.002 to 0.009).

**What the literature says is likely to fail here.**
- **Raising budget.** BRA: computation value 0.007 at failure; 4096 tokens ≈ 0 over 2048. This is a mechanism, not a correlation, and it matches the caller's three-arm null exactly.
- **Longer/richer context.** BRA retrieval ≈ 0 at repo level; RepoNav buys localisation at 2× tokens; RepoNorm's functional gain is not significant after Holm; Measure-Before-You-Manage shows context-calibration gains do not transfer.
- **Asking the model to self-check.** Systems-Not-Models: verification rises 19→22% under prompting. Self-Execution Simulation: inference-only self-simulation costs −10.6 to −20.1 pass@1.
- **Post-hoc critics.** Intent: extractor/critic −1.0, reviewer/ensemble −2.5/0.0, security audit-and-refine −12.8.
- **Generic scaffolding.** How-Much-Harness: delegation −2.9, broadcast+parallel −7.6 percentile.

---

## 5. Evidence strength

**Claims the sources actually support.**
- A large fraction of agent failures are constraint/form failures with functional success intact: MTAC-IFBench CSR 80.4 vs C-ISR 12.7, BSR 98.0 vs F-ISR 37.0 (Table 2/3, 2026-09-14); OctoBench ISR 28.11 vs CSR 85.64 (Table 2, 2026-01-15).
- Structuring requirements into **machine-checkable** conditions and gating commitment on them beats a natural-language spec by **10 pp** (ECLoop Table 4, 2026-07-30).
- That gate costs **fewer** tokens (−12.1%) than baseline (ECLoop Table 3).
- The gate breaks previously-correct runs at a **low single-digit** rate: 9/281 and 9/379 (**2.4–3.2%**, inferred from ECLoop's reported counts).
- Telling an agent which deliverable checks failed and letting it retry raises all-or-nothing compliance by **+7.2 to +16.8 pp** on already-failing instances (OctoBench Table 5).
- Post-generation critics/repair add **≈0** and can subtract (Intent, 2026-10-27).
- Computation/sampling budget saturates and **substitutes for**, rather than complements, information: 21/36 interactions negative, none positive (BRA Table 5, 2026-10-20).
- Passing tests is insufficient for a correct deliverable: **46%** of test-passing patches score < 0.7 on a spec-alignment rubric (Agentic Rubrics §4.2.2).
- Tool-description rewriting gives a **median +5.85 pp** solve-rate gain with **16.7%** of affected tasks regressing (MCP descriptions Table 7).

**Claims the caller might want to make that the sources do NOT support.**
- ❌ "Requirement extraction from the task statement has a measured solve-rate delta on coding tasks." **No 2026 paper isolates pre-run extraction from the gate that consumes it.** ECLoop bundles both; OctoBench's feedback is post-hoc and subset-conditioned.
- ❌ "A verifier-in-the-loop with bounded repair has a measured false-rejection rate." **No paper reports one.** Verification Horizon quantifies false *positives* (28.57% hacked-resolved) and explicitly has no headline false-negative number. ECLoop's 2.4–3.2% regression is a *net behavioural* regression count, not a predicate error rate — it conflates gate errors, extra turns and sampling variance.
- ❌ "Repository evidence bundles at edit time raise coding solve rate." RepoNorm's functional delta is **not significant after Holm**; RepoNav reports localisation only; BRA measures retrieval at **−0.002 to 0.009**; Context-Augmented's +49 pp is 8 tasks, three un-isolated components, vendor preprint.
- ❌ "Tool-description optimisation is a reliable capability lever." AT-AT: significant in **1 of 3** domains, Retail regressed; MCP: 16.7% regressions.
- ❌ "Semantic scaffolding helps more when budget is larger." The only controlled measurement of the interaction finds **substitution, not complementarity** (BRA Table 5), and rank order Information > Budget (Systems-Not-Models Table 3).
- ❌ "Harness scaffolding in general raises capability." How-Much-Harness finds the opposite for ML engineering under equal time.

---

## 6. Gaps

1. **No paper evaluates a completion predicate over the final artefact** (file at path, required library, exact CLI). ECLoop's condition language is trajectory-event based and the paper confirms artefact properties are outside it. This is the exact experiment ROF would be running first.
2. **No false-rejection / false-accept rate for artefact-form predicates** on tasks that already pass. Would need a controlled run: apply the predicate to gold/oracle solutions and to already-passing agent solutions, count rejections.
3. **No measurement of one bounded repair vs N repairs** for deliverable-form failures. OctoBench retries once with full failed-check feedback; ECLoop allows 3 holds; TraceDev allows 3 traceability + 5 test rounds. Nobody ablates the count.
4. **No 2026 coding-agent paper ablates requirement extraction alone**, holding the gate fixed. ECLoop's "NL spec" arm is the nearest proxy (58 vs 68) but the NL spec is still *given to the agent*.
5. **No standard benchmark in which the sole failing check is a deliverable-form property.** OctoBench and MTAC-IFBench aggregate 32–91 constraints per instance, so their ISR/C-ISR deltas cannot be attributed to form specifically.
6. **Cost reporting is systematically absent.** Of 17 papers, only ECLoop (tokens/$), Agentic Rubrics ($/instance), Context-Augmented ($), AT-AT ($/domain) and RepoNorm (acquisition tokens) report intervention cost. OctoBench, MTAC-IFBench, Intent and BRA report none.
7. **Transfer limits to flag in any citation.** How-Much-Harness = ML engineering; Systems-Not-Models = 4 scientific tasks, not coding; Intent = smart contracts with a live client; Self-Execution Simulation = training-time; TraceDev = framework-vs-framework.

---

## 7. Answer to the question asked

**The single intervention with the strongest measured evidence for this failure class is ECLoop's pattern: compile the task's requirements into machine-checkable conditions before the run, and gate the commit/submit action on them, holding at most a bounded number of times and then proceeding with an audit record.**

Evidence: **+11.8 pp** (56.2→68.0) on 500 SWE-bench Verified instances with GPT-5-mini/mini-swe-agent, **+4.8 pp** on MiniMax-M2.5, **+10.4 / +5.0 pp** replicated on Codex CLI (Tables 1–2); ablation-isolated **+10 pp over a natural-language spec** and **+10 pp from the commitment check itself** (Table 4, 100 instances); **−12.1% tokens and −10.2% dollars** (Table 3); **2.4–3.2%** of previously-solved runs broken (9 of 281 / 9 of 379). This is a controlled ablation across two harnesses and two models — the only such result in the set. Source: arXiv:2607.28815v1, preprint, 2026-07-30.

**What is missing before it could be adopted in ROF:**
1. **A condition language over workspace state.** ECLoop's predicates fire on trajectory events. ROF needs `∃ file at path P ∧ file contains pattern Q ∧ command C exits 0`, evaluated against the final workspace. No paper provides a validated design for this, and its failure modes (over-strict predicates, environment-dependent checks) are unmeasured.
2. **A measured false-rejection rate on already-passing tasks.** ECLoop gives a 2.4–3.2% *net regression* figure, not a predicate false-rejection rate. Before enabling by default, ROF should apply the lifted predicate to (a) gold solutions and (b) the already-passing rows of existing runs, and count spurious rejects.
3. **A repair-count ablation (1 vs 3 holds).** The literature uses 1 (OctoBench), 3 (ECLoop, TraceDev) and 5 (TraceDev test remediation) without comparing them. The caller's budget arm already shows that extra *turns* do not help; a bounded-1 design is the cheapest test and the most likely to survive the 1.3× cost guardrail.
4. **A token-matched control, not a budget-raised one.** BRA (2026-10-20) shows the comparison must be information-matched, because extra computation converts to approximately nothing (0.131 → 0.007) while information holds at 0.06–0.14.
5. **A stable-failure instrument.** The failing test must be confirmed stable across arms *before* treatment (the caller already has this), because Systems-Not-Models measures ~54% of score variance as run-to-run noise — a single failing row would otherwise be uninterpretable.

---

## 8. Sources

**Kept (17).**
| id | why kept |
|---|---|
| 2607.28815 | Only controlled ablation of executable requirement conditions gating commit; includes cost and regression counts |
| 2601.10343 | Checklist-defined ISR/CSR + measured feedback-repair delta on failed checks |
| 2609.14992 | Quantifies the exact failure class (CSR vs C-ISR; BSR vs F-ISR) at scale |
| 2610.28834 | Only controlled C/I/F factorial; supplies the budget-substitution mechanism |
| 2610.37830 | Front-load vs post-hoc; measures that post-hoc critics add ≈0 |
| 2610.01618 | Ranks information above budget/model with effect sizes; verification-by-prompting 19→22% |
| 2601.04171 | Rubric verifier with $ cost, ROC-AUC, and the tests-pass-but-spec-fails 46% |
| 2606.26300 | Dissent: no fixed verifier holds; false-positive rates quantified |
| 2604.03253 | Dissent: inference-only self-verification hurts |
| 2609.40303 | Dissent: harness machinery does not beat a minimal agent under equal time |
| 2602.14878 | Tool-description lever with median ΔSR, regression share and token overhead |
| 2610.34679 | LLM-optimiser cost and mixed significance for the same lever |
| 2610.07757 | Structured repo norms: compliance up, functional gain not significant |
| 2609.08355 | Repo-evidence packaging: localisation gain at ~2× tokens |
| 2605.08112 | Largest measured compliance delta (46→95%) but 8 tasks, un-isolated |
| 2608.31057 | Equal budgets ≠ equal context; calibration gains fail to transfer |
| 2607.18886 | Traceability loop inside budget — kept as confounded existence proof |

**Rejected.**
| id | why rejected |
|---|---|
| 2602.07513 (SPECA) | Ethereum-client auditing case study, not a coding-agent deliverable harness |
| 2608.17177 (Grounding AI Agents in Contracts) | Spec-driven *test generation* (+9.8 pp bug detection); does not gate an agent's deliverable |
| 2609.33429 (Graph-Guided Repository Environment Construction) | Builds execution environments, not deliverable conditions |
| 2610.28679 (VerifierBench) | Generates verifiers as a capability benchmark; no harness intervention delta |
| 2604.00280 (Spec-Harness) | Formal-specification verifiability, outside coding-agent deliverables |
| 2610.29822 (UserSWE) | Mentions false positives/negatives only as a positioning claim; no measured rate |
| 2605.23916 / 2610.33182 / 2602.03580 | Tool-description *attacks*/bias, not capability levers |
| 2607.17937 (Context rot) | Qualitative white-box study; no solve-rate delta for this class |

**Non-paper sources used:** none. All numbers come from opened papers; no search snippet was quoted.

---

## 9. Search log

Tools: `papers_search_arxiv` (relevance), arXiv Atom API via `fetch_content` with `submittedDate:[202601010000 TO 202611120000]` restrictions, full-text extraction via `fetch_content` on `arxiv.org/pdf/<id>` and `arxiv.org/abs/<id>`. Every number above was pulled from the paper body with an explicit request for the table/section containing it; four numbers were re-queried a second time in a different form to check consistency (ECLoop regression denominators and condition semantics twice; OctoBench reflection-arm semantics twice; Verification Horizon metadata twice).

Queries issued:
1. `checklist requirements extraction coding agent` (arXiv relevance)
2. `verifier-guided repair coding agent false rejection`
3. `abs:"checklist" AND abs:"agent" AND abs:"code"`, 2026-01-01→2026-11-12, 25 results
4. `abs:"verifier" AND abs:"agent" AND abs:"test"`, 2026, 30 results
5. `abs:"repository" AND abs:"retrieval" AND abs:"agent"`, 2026, 30 results
6. `abs:"tool description" AND abs:"agent"`, 2026, 30 results
7. `abs:"budget" AND abs:"coding agent"`, 2026, 30 results
8. `abs:"requirements" AND abs:"agent" AND abs:"repository"`, 2026, 40 results
9. `abs:"self-verification" AND abs:"coding"`, 2026, 40 results
10. `abs:"artifact" AND abs:"coding agent" AND abs:"instruction"`, 2026, 40 results (nothing on artefact form)
11. `abs:"false negative" AND abs:"test" AND abs:"patch"`, 2025-01→2026-11, 30 results
12. `abs:"specification" AND abs:"test generation" AND abs:"agent"`, 2026, 30 results
13. `arxiv 2026 coding agent harness "requirement checklist" OR "spec extraction" solve rate` (web search, arxiv.org / ar5iv.org)

Dead ends worth recording: query 10 returned nothing on artefact-form benchmarks; query 11 produced no paper with a measured false-rejection rate for patch validation; query 12 produced no paper that gates a deliverable on spec-derived tests. Those three absences are the substance of §6.
