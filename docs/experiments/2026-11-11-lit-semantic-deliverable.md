# Literature: harness interventions for semantic deliverable-form failures (2026)

**Date:** 2026-11-11 · **Scope:** coding agents only; 2026 work only unless explicitly marked **[pre-2026]**.
**Provenance labels:** **measured** (source + number), **inferred** (derived from measured), **chosen** (no anchor).

---

## 0. Question

Which harness intervention should ROF add to convert "solves the problem, ships the wrong artefact form" failures — where one test such as `test_r_script_created_and_used_rstan` fails stably in every row across three budget arms — into passes, at what token cost, and is that intervention's benefit conditional on budget?

As a build question: given a frozen task statement and a verifier that already names the missing property, is the cheapest measured win (a) extract the deliverable requirements up front, (b) gate submission on an executable predicate plus one bounded repair, (c) feed more repository evidence at edit time, or (d) nothing — because the agent already knows the requirement and simply never checks it?

The literature answers that (d) is wrong and (c) is nearly worthless; (a)+(b) are the same mechanism seen at two points in time, and the measured cost of (b) is *negative* in tokens.

## 1. Findings

| Paper | arXiv, date | Key measured number | Relevance here |
|---|---|---|---|
| ECLoop (evidence-conditioned execution layer) | 2607.28815, 2026-07-30 | **+11.8 pp** (56.2→68.0), 500 SWE-bench Verified, GPT-5-mini/mini-swe-agent; MiniMax-M2.5 75.8→80.6 (+4.8), both p<0.001; Codex CLI 40.4→50.8, 74.8→79.8; ablation 58 (NL spec) vs **68** full, 58 without the commitment check (100 instances); tokens **−12.1%**, $ −10.2%, Codex tokens −5.4% (Table 3); **9/281** and **9/379** previously-solved runs broken (2.4% and 3.2%, inferred division) | Only controlled ablation of executable conditions gating commit. Conditions are *trajectory-event* patterns, not artefact properties |
| OctoBench | 2601.10343, 2026-01-15 | ISR 28.11 / CSR 85.64 (Opus-4.5, avg@3); failed-check feedback on retry: +16.79 ISR (21.37→38.17, ChatGLM-4.6), +7.20 (38.40→45.60, Opus-4.5) (Table 5); 217 instances, 7,098 items | Checklist-defined all-or-nothing rate + measured repair delta. No token cost reported; Table 5 is partial-failure instances only, so it overstates a full-benchmark delta [inferred] |
| MTAC-IFBench | 2609.14992, 2026-09-14 | **CSR 80.4 vs C-ISR 12.7** (GLM-5.2, Claude Code); BSR 98.0 vs F-ISR 37.0 (Opus-4.6, Table 3); harness swap C-ISR 12.7→6.7 (CSR 80.4→76.8); evaluator agreement 0.921/0.939/0.887 (Table 4) bounds how much of the 12.7% is judge noise | Sizes the failure class: the code works, the deliverable does not. No intervention delta |
| BRA — Where Should a Stuck Agent Spend Its Next Tokens | 2610.28834, 2026-10-20 | computation value **0.131→0.007**; information 0.06–0.14; **retrieval −0.002 to 0.009** at repo level vs audited spec 0.048–0.103; feedback 0.362–0.402 on the 40 hardest checkpoints; **21/36 interactions significantly negative, none positive**; models self-select feedback in 95–100% of cases yet realise 10–27% of the oracle gain, worse than random by 0.029–0.064 | Budget is not the lever, and retrieved code is worth ~zero at repo level; a spec of what the code must do is not |
| Measuring Where Intent Gets Lost | 2610.37830, 2026-10-27 | post-generation stages **−1.0 to +0.7**: extractor/critic −1.0, reviewer/ensemble −2.5/0.0, rulebook repair −0.2, client triage +0.7, security audit −12.8 [−16.3, −9.5]; information is lost before generation — 65.9% caught from a short description vs 87.5% from a complete document (gap 21.6 [15.1, 29.3]); elicitation-first 39.3→72.9 (Table 2) | Post-hoc repair cannot recover a requirement never captured |
| Agents Are Systems, Not Models | 2610.01618, 2026-10-01 | mean ranks Information 1.00, Model 2.67, Reasoning 3.00, Budget 3.33, Verification 5.00 (Table 3); verification by prompting **19%→22%**, ~three quarters check only format, oracle ~triples it; ~54% of variance among genuine attempts is run-to-run noise | Information outranks budget; asking the agent to self-check barely moves it |
| Agentic Rubrics | 2601.04171, 2026-01-07 | **46%** of test-passing patches with rubric <0.7 are low-utility; ROC-AUC 0.886, PR-AUC 0.722; **$0.293/instance, 22.9 calls**; reranks K=16 rollouts | Direct measurement that passing tests do not imply a correct deliverable; selection only, no repair loop |
| The Verification Horizon | 2606.26300, 2026-06-24 | with hack monitoring: hacked-resolved **28.57%→0.56%**, hack rate 37.76%→1.31%, clean resolved 40.22%→60.53% | Quantifies false *positives*; no false-negative rate — the number ROF would need |
| Self-Execution Simulation | 2604.03253, 2026-03-11 | inference-only scaffold **hurts**: Qwen3-32B DMC pass@1 −10.6, LCB-IO −20.1 (train-time gains 48.5→75.5 CruxEval-O) | A repair loop must trigger on an external, deterministic predicate, not self-assessment |
| RepoNorm — repository norms | 2610.07757, 2026-10-06 | norm compliance DeepSeek +10.77, Qwen +7.42, Gemini +10.03; functional Δ +9.09/+7.44/+5.79 pp but **"no comparison significant after Holm"**; acquisition 134.52M in / 7.67M out tokens, 2.14 h | Structured requirement-shaped repo context raises form compliance without (significantly) raising functional success |
| RepoNav | 2609.08355, 2026-09-08 | Function Acc@5 0.484→0.557, File Acc@5 0.753→0.774 (560 instances); trace tokens 62.4k vs 31.7k (**+97%**) | Localisation at ~2× tokens; no end-to-end solve rate |
| Context-Augmented Code Generation | 2605.08112, 2026-04-27 | decision compliance **19/41 (46%)→39/41 (95%)**, blocking violations 5→0; tokens 3,902K→3,867K (−1%), $4.13→$5.28 (+28%) | Largest compliance delta in the set, but 8 tasks, 41 decision points, three un-isolated factors, vendor preprint |
| MCP tool descriptions | 2602.14878, 2026-05-31 | median ΔSR **+5.85 pp**; improved 54.17%, regressed **16.67%**, McNemar p=0.02; input tokens +21.85–41.43% | A tool-surface lever with a real regression tail; A/B any description edit |
| AT-AT | 2610.34679, 2026-10-26 | significant in **1 of 3** domains (Telecom 29.50→36.75, p=0.0033; Retail 77.00→74.67); optimiser **$2.78–10.69**/domain; TerminalBench-2 pass@1 49.32→51.36 | The honest price of an LLM optimiser for tool surfaces: cheap, with a real chance of no significant gain |
| How Much of a Harness Does a Strong Agent Need | 2609.40303, 2026-09-30 | Malena **69.74 percentile / 62.5% medal** vs +delegation 63.62/45.2, +broadcast+P3 58.91/33.3 (MLE-bench fixed30, 24 h) | Generic scaffolding does not add capability — but single system, authors' own harness, no deliverable-form arm, so no evidence against requirement gates |
| Measure Before You Manage | 2608.31057, 2026-08-31 | none of ten constrained-arm pairs matches on all eight tasks at equal token budgets (Table E.2); calibration Δ **−1.633 [−2.667, −0.700]**, Holm p=0.0146; held-out Δ **−0.500 [−1.250, 0.000]**, p=0.5000 — "calibration gains do not transfer" | Equal budgets are not equal delivered context; pre-register a held-out slice |
| TraceDev | 2607.18886, 2026-07-21 | ETOUR 23.40%→53.63%, SMOS 24.74%→56.82%; bounded 3 traceability + 5 test-remediation rounds; tokens 109,541 vs 98,766 | Existence proof that a traceability loop runs inside a budget; framework-vs-framework, so the delta is not attributable |

## 2. Conclusions

**Decision: build (b), requirement-derived machine-checkable completion conditions evaluated against the *final artefact*, with a hold-and-repair path.** ECLoop is the same architecture — compile per-task conditions before the run, gate the *commitment* action, hold at most N times, then proceed with an audit record — and it is the only controlled, ablation-isolated result in the set: **+11.8 pp** on 500 SWE-bench Verified instances, replicated across two harnesses and two models (Tables 1–2), **+10 pp over a natural-language spec** and **+10 pp from the commitment check itself** (Table 4), at **−12.1% tokens and −10.2% dollars** (Table 3), breaking **2.4–3.2%** of previously-solved runs. OctoBench's feedback-correction arm (2601.10343 §4.3.2) is the same idea without the gate; Agentic Rubrics (2601.04171) the same idea without the repair.

**What is genuinely different in ROF's setting.**
1. ECLoop's predicates are trajectory-event patterns ("inspected relevant callers before editing"). ROF's failing test is a **workspace-state predicate** — file at a path, contains `library(rstan)`, the script runs, a CLI exits 0. No 2026 paper compiles conditions over the final artefact and gates on them: a real, defensible increment, and the untested part.
2. The predicate is **already written by the verifier**. ECLoop, OctoBench and Intent all had to derive conditions; ROF can lift `test_r_script_created_and_used_rstan` into a pre-submission check. Extraction is the expensive step in the literature and it is free here **[inferred]**.
3. A multi-arm budget ablation with a stable failure is a cleaner identification strategy than anything in these papers.

**What is not novel.** "Give the agent a checklist" (OctoBench 7,098 items; MTAC-IFBench 91/instance); "verify before submitting" and the 3-hold-then-fallback pattern (ECLoop, TraceDev, OctoBench); "retrieve more repo context at edit time" (RepoNav, RepoNorm, Context-Augmented — and the weakest lever measured, BRA retrieval −0.002 to 0.009).

**What the literature says is likely to fail here.**
- **Raising budget.** BRA: computation value 0.131→0.007 at failure, 4096 tokens ≈ 0 over 2048 (0.610 vs 0.600). A mechanism, not a correlation, and it matches the three-arm null exactly.
- **Longer or richer context.** BRA retrieval ≈ 0; RepoNav at 2× tokens for localisation; RepoNorm not significant after Holm; Measure-Before-You-Manage: calibration gains do not transfer.
- **Asking the model to self-check.** Verification by prompting 19→22% (Systems-Not-Models); BRA: models pick feedback 95–100% of the time yet realise 10–27% of the oracle gain; inference-only self-simulation −10.6 to −20.1 pass@1.
- **Post-hoc critics.** Intent: extractor/critic −1.0, reviewer/ensemble −2.5/0.0, security audit-and-refine −12.8.
- **Generic scaffolding.** How-Much-Harness: delegation −2.9, broadcast+parallel −7.6 percentile (the paper's own deltas against its base arm; the two arms read 63.62 and 58.91 vs Malena 69.74 — the 4.7 gap between the deltas is exactly the 4.7 gap between the arms).

**Gates before enabling by default.**
1. **A condition language over workspace state** — `∃ file at path P ∧ file contains pattern Q ∧ command C exits 0`, evaluated against the final workspace. No validated design exists; over-strict and environment-dependent predicates are unmeasured failure modes.
2. **A measured false-rejection rate on already-passing tasks.** ECLoop's 2.4–3.2% is a net behavioural regression count, not a predicate error rate. Apply the lifted predicate to (a) gold solutions and (b) the already-passing rows of existing runs; count spurious rejects.
3. **A repair-count ablation (1 vs 3 holds).** The literature uses 1 (OctoBench), 3 (ECLoop, TraceDev) and 5 (TraceDev test remediation) without comparing. Extra turns are the lever the budget arms already show failing, so bounded-1 is the cheapest test and the likeliest to survive the 1.3× cost guardrail.
4. **A token-matched control, not a budget-raised one** — BRA's information values hold at 0.06–0.14 where computation saturates.
5. **A stable-failure instrument.** Confirm the failing test is stable across arms before treatment; ~54% of score variance among genuine attempts is run-to-run noise, so a single failing row is uninterpretable.

> **Now (2026-11-11, after this survey).** Both the checklist form and an artefact predicate were tried on this corpus the same day. A goal-checklist arm converted `mcmc-sampling-stan` 5/6 → **6/6 solved** in 1 of 3 reps at fewer gross tokens per test passed, path-robustness code 11–12× in checklist deliverables vs 0–1× in controls (local research note `goal-checklist-ab1`, summarized in [decisions](../decisions.md)); the byte-identical checklist on 16/16 further rows flipped no named test (flat tokens, registered KILL), so the lever is rerun-context specific, not a general one. An offline predicate (zero-cost triage) over 47 banked passing workspaces found no reachable trigger on this corpus (of 12 post-fix rows every `done` row already passes; the targetable cells are accuracy-bound) and false rejections of **0 of 14** git-snapshotted passing workspaces against **32 of 33** files-only captures, a capture artifact (`docs/experiments/2026-11-11-triage.md`, `docs/decisions.md`).

## 3. Caveats

**Claims the sources do NOT support.**
- ❌ *Requirement extraction from the task statement has a measured solve-rate delta on coding tasks.* No 2026 paper isolates pre-run extraction from the gate consuming it; ECLoop bundles both, OctoBench's feedback is post-hoc and subset-conditioned.
- ❌ *A verifier-in-the-loop with bounded repair has a measured false-rejection rate.* None reported. Verification Horizon quantifies false positives (28.57% hacked-resolved) and has no headline false-negative figure; its conclusion — "no fixed reward function can remain effective as policy capability continues to grow" — is why a test-derived predicate must not be treated as ground truth.
- ❌ *Repository evidence bundles raise coding solve rate.* RepoNorm not significant after Holm; RepoNav localisation only; BRA retrieval −0.002 to 0.009; Context-Augmented is 8 tasks with un-isolated components.
- ❌ *Tool-description optimisation is a reliable capability lever.* AT-AT significant in 1 of 3 domains, Retail regressed; MCP 16.67% regressions.
- ❌ *Semantic scaffolding helps more when budget is larger.* The only controlled interaction measurement finds substitution, not complementarity (BRA Table 5), and Information outranks Budget.
- ❌ *Harness scaffolding in general raises capability.* How-Much-Harness finds the opposite for ML engineering under equal time.

**Gaps.**
1. No paper evaluates a completion predicate over the final artefact (file at path, required library, exact CLI); ECLoop confirms artefact properties fall outside its condition language.
2. No false-rejection / false-accept rate for artefact-form predicates on tasks that already pass.
3. No ablation of one bounded repair against N repairs for deliverable-form failures.
4. No 2026 coding-agent paper ablates requirement extraction alone with the gate held fixed; ECLoop's NL-spec arm (58 vs 68) is the nearest proxy, and the NL spec is still given to the agent.
5. No standard benchmark whose sole failing check is a deliverable-form property; OctoBench and MTAC-IFBench aggregate 32–91 constraints per instance, so their ISR/C-ISR deltas cannot be attributed to form specifically.
6. **Cost reporting is systematically absent**: of the 17 papers only ECLoop, Agentic Rubrics, Context-Augmented, AT-AT and RepoNorm report intervention cost; OctoBench, MTAC-IFBench, Intent and BRA report none.
7. **Transfer limits to flag in any citation**: How-Much-Harness is ML engineering; Systems-Not-Models is four scientific tasks, not coding; Intent is smart contracts with a live client; Self-Execution Simulation is training-time; TraceDev is framework-vs-framework.

**Rejected.** 2602.07513 (SPECA) Ethereum-client auditing case study; 2608.17177 spec-driven *test generation* (+9.8 pp bug detection), does not gate a deliverable; 2609.33429 builds execution environments; 2610.28679 (VerifierBench) generates verifiers as a capability benchmark, no intervention delta; 2604.00280 (Spec-Harness) formal-spec verifiability, outside coding-agent deliverables; 2610.29822 (UserSWE) false positives/negatives only as a positioning claim; 2605.23916 / 2610.33182 / 2602.03580 tool-description attacks and bias, not capability levers; 2607.17937 (Context rot) qualitative, no solve-rate delta for this class.

**Search provenance.** No non-paper sources: every number came from an opened paper, no search snippet quoted. Tools `papers_search_arxiv`, the arXiv Atom API restricted to `submittedDate:[202601010000 TO 202611120000]`, and full-text extraction from `arxiv.org/pdf/<id>`; four numbers were re-queried in a different form (ECLoop regression denominators and condition semantics, OctoBench reflection-arm semantics, Verification Horizon metadata). Dead ends that are the substance of the gaps above: nothing on artefact-form benchmarks, no measured false-rejection rate for patch validation, no paper gating a deliverable on spec-derived tests.
