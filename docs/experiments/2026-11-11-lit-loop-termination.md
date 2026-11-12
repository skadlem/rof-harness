# Loop termination and budget stopping for coding/terminal agents — measured evidence

**Date:** 2026-11-11 · **Scope:** coding/terminal agents; LLM-agent budget and stopping literature, 2024–2026, weighted to Oct–Nov 2026.

**Decision this informs (one sentence):** whether ROF should build a *trajectory-level "this run will not finish" detector* that terminates early, or instead spend the effort on *budget allocation across tasks* and *hard structural gates on the first write* — and on what measured evidence either choice rests.

**Reading note.** Every source below is an **arXiv preprint**; I checked for an accepted-venue "Comments" field on the three most load-bearing ones (2610.35971, 2610.22732, 2610.05572) and found none, so **no citation here is peer-reviewed as far as I can verify**. Depth is marked per paper: `[full]` = read the HTML full text including results tables; `[abs]` = abstract + metadata only. Numbers marked `[T]` came from a table; numbers marked `[A]` came from an abstract and were not cross-checked against a table.

---

## 1. Summary

The 2026 literature has converged on a **negative result that is directly about our failure case**: within-run, prefix-only prediction of "this attempt will fail" is at chance — within-task AUC **0.48–0.52** in the two largest coding-agent studies, **0.496** in an independent math-reasoning replication, and a **near-zero increment** over a difficulty prior in a third — while a *task-difficulty* model that never looks at the run reaches **0.92–0.945 AUROC**. Almost all apparent "stuck detection" performance is task recognition, not run recognition. The same body of work finds that **abort-and-restart policies lose to uninterrupted continuation** on both solve rate and mean tokens in 216 of 224 budget configurations, and that abort-only intervention loses 17.2–49.0 tasks versus allocating the same budget across tasks.

What *does* survive measurement is (a) **between-task budget allocation** from a difficulty model, and (b) **hard structural gates that change what the agent is permitted to do next** — the single largest ablation effect in the one coding-agent gate paper is the "commitment check" that postpones an action until evidence conditions are met (**−10 pp Pass@1 when removed**). The one published stuck-loop *terminator* for coding agents fires only on edit churn plus error output and **would never fire on a read-only, no-write loop**; it also reports no false-positive count and admits "a difficult but productive task may be mistaken for a failure loop."

**The one thing to do differently:** do not build a learned within-run "stuck" classifier. Build the cheapest deterministic predicate we already have — *does the required artefact exist and is it non-empty at checkpoint k* — and use it as a **gate that forces a first write and blocks further read-only exploration**, while keeping the run alive. This is the only mechanism in the literature whose measured effect comes from **enforcement rather than prediction**; every measured prediction-based variant either fails to beat a difficulty prior or costs more than it saves.

---

## 2. Landscape

| Approach family | Representative work (id, date) | Mechanism | How it is evaluated | Main limitation |
|---|---|---|---|---|
| Within-run failure prediction (monitor/classifier) | Continue or Restart? 2610.35971 (2026-10-26); Disentangling Difficulty 2610.05572 (2026-10-04); Hard Issue/Weak Model 2611.08285 (2026-11-05) | Prefix features → P(fail); count models, frozen embeddings, fine-tuned 0.6B monitors | Within-task AUC on mixed-outcome tasks; replay under shared token caps | AUC ≈ chance within task (0.48–0.52); pooled AUC is >99.93% cross-task |
| Monitor + calibrated threshold + abort | StopLoss 2610.25525 (2026-10-18); Fail-Fast/Restart-Smart 2608.03222 (2026-08-04) | Small LM scores last 1–3 steps; threshold calibrated on successful runs | Cost per retained solve; fraction of successful runs preserved | −4.5% cost on Verified (small); needs 25% FPR to gain solve rate |
| Recall-controlled probes (white-box) | Doomed from the Start 2607.06503 (2026-07-07) | Linear probes on hidden activations + cascaded calibrated gates | Token saving at fixed global recall | Needs hidden states; web agents, not coding; behavioural features add nothing |
| Between-task budget allocation | SPLIT 2610.23255 (2026-10-16); Disentangling 2610.05572 | Empirical-Bayes / Thompson sampling over tasks from a small pilot | Throughput at fixed total token budget | Needs repeated attempts; regret depends on ranking quality |
| Decision-value framing of restart monitors | Monitor Accuracy Isn't the Answer 2610.36544 (2026-10-27) | Values a monitor by decision, not AUC | Live paired comparisons with certification bar | 0 of 103 live comparisons certified |
| Rule-based stuck terminator + redirect | TokenEconomizer 2610.22732 (2026-10-15) | Counters on edit churn, error density, repeat signatures; judge; forced stop at κB_i | Cost per task and Pass@1 on 277 SWE-bench tasks | Loop terminator not ablated alone; zero-edit loops out of scope |
| Escalation / hand-off gate | Knowing When to Stop 2610.31895 (2026-10-22) | `escalate_to_human` tool + external monitor | Budget-exhaustion rate; success after env restoration | Hand-off ≠ termination; no FPR reported |
| Evidence-conditioned action gating | ECLoop 2607.28815 (2026-07-30) | Postpone actions whose evidence conditions are unmet | Pass@1 on 500 SWE-bench Verified, 2 models × 2 scaffolds | Depends on issue-text quality |
| Macro/abstraction cost reduction | FlowPlan 2611.13241 (2026-11-09); TokenEconomizer | Cut re-reads/verification structurally | Token ratio at equal resolution | Not a stopping method; no solve gain |
| Progress-rate metric (foundational) | AgentBoard 2401.13178 (**2024-01**, older) | Judge-scored incremental progress | Correlation with success | Numbers not retrievable from abstract; 2024 setting |

---

## 3. Key papers

### 3.1 Angle 1 — trajectory-level stuck / no-progress detection

#### P1. Continue or Restart? The Limits of Early Failure Prediction in Coding Agents `[full]`
arXiv:2610.35971v1, preprint, submitted **2026-10-26**. <https://arxiv.org/abs/2610.35971>

- **Problem.** Should a coding agent's run be killed early when a cheap monitor says it is failing, and is restarting better than continuing?
- **Method.** 147,002 execution traces / 32,406 attempts over four open-weight model–framework settings (OpenHands/Qwen3.5, OpenHands/MiniMax-M2.5, SWE-agent/Qwen3.5, +1). Three detector families: issue+prefix token-count model, frozen text embeddings, and a three-seed LoRA fine-tuned Qwen3-0.6B monitor. 224 budget–threshold configurations; all comparisons replay recorded attempts under shared token caps.
- **Result (the number that matters).** Within-task AUC is **0.50 [0.49,0.51]** at 5k tokens, **0.50 [0.48,0.51]** at 20k, **0.48 [0.45,0.52]** for SWE-agent/Qwen3.5, **0.49 [0.48,0.50]** for frozen embeddings, and **0.50 [0.49,0.51]** for a task-conditional model over **38,447 success–failure pairs from 1,737 mixed-outcome tasks** [T II]. Stratified by solve rate (low/medium/high), within-task AUC stays **0.49–0.51** and *every* CI contains 0.50 [T III]. Learned restart beats matched random restart by **up to 1.98 pp**, but the algebraic decomposition shows **−0.11 to +0.24 pp** comes from within-task selection; the rest is between-task allocation. Uninterrupted continuation beats both restart policies on solve rate **and** mean token cost in **111 of 112** configurations at the 75k/100k frontiers and **216 of 224** overall (68.10k vs 69.93k mean tokens, SWE-agent/Qwen3.5 at 75k cap).
- **Detector cost.** A 0.6B monitor plus count/embedding baselines — the paper's point is that even a *free* detector has no within-run signal to exploit.
- **Ablation vs single system.** Controlled: same traces, same caps, three detector families, four systems, 224 configurations, plus an exact algebraic decomposition of the restart gain.
- **Admitted failure mode.** "Within-task discrimination requires mixed-outcome tasks by definition"; scoped to four *open-weight* settings and Python-centric repos; replay assigns "zero credit to partial edits that exceed the residual budget"; and at early landmarks "observed prefix signals do not separate competing runs on the same issue."
- **Applicability.** This is the direct test of our proposed detector and it fails. It does **not** say trouble detection is impossible — it says prefix-only prediction at 5k–20k tokens, the regime where stopping still saves budget, carries no within-run information in coding agents.

#### P2. Disentangling Task Difficulty from Run-Level Failure in Agent Failure Prediction `[full]`
arXiv:2610.05572v1, preprint, submitted **2026-10-04**. <https://arxiv.org/abs/2610.05572>

- **Problem.** Is the reported strength of failure predictors real run-level signal, or just task-difficulty recognition?
- **Method.** Four corpora including SWE-rebench (**6,306 tasks / 1,823 repos**), LiveClawBench (**17 models, 6,834 trajectories**) and a 10-attempt × 500-task SWE-bench set; compares pooled AUROC against within-unit AUROC, and abort-only against task-level budget allocation.
- **Result.** **>99.93%** of the positive–negative pairs underlying pooled AUROC are *cross-unit*. A difficulty oracle that never observes the run reaches **AUROC 0.945** while being at chance within task. Released early monitors measure **0.507** (Automata, after one turn), **0.49–0.52** (EarlyEval, after one step), **0.509/0.513** (hidden-state probes). Deployment math: early stopping only improves throughput once within-task AUROC reaches **≈0.84 at a 10M-token budget** and **≈0.93 at 5M** (rising to 0.90–1.00 when monitor errors concentrate on hard tasks). Abort-only intervention **loses 17.2–49.0 tasks** relative to allocation and "does not outperform round-robin"; adding a monitor to budgeted Thompson sampling changes throughput by **−4.2 to −0.6 tasks** with all CIs spanning zero. Allocation alone solves **162.0 tasks at 2M tokens vs 53.0** for round-robin and 172.9 for the oracle (93.7% of oracle throughput).
- **Detector cost.** Not the binding constraint — the paper shows even free prediction adds nothing at observed AUC.
- **Ablation vs single system.** Multi-corpus, 17-model corpus, explicit oracle vs predictor comparison.
- **Admitted failure mode.** "Failure to reject H0: A_within = 0.5 does not imply that no run-level signal exists. We therefore describe the early signal as **weak rather than absent**"; deployment results are replay, not live intervention; allocation results apply "most directly to settings with repeated attempts."
- **Applicability.** Gives us a **quantitative adoption bar**: any learned stuck detector must exceed **~0.84 within-task AUROC** before it can beat simply allocating budget across tasks. That is the single most useful number in this review for a go/no-go decision.

#### P3. StopLoss: Teaching Agents to Predict Failure and Stop Early `[full]`
arXiv:2610.25525v1, preprint, submitted **2026-10-18**. <https://arxiv.org/abs/2610.25525>

- **Problem.** Cheap, calibrated mid-run abort for SWE agents.
- **Method.** A small external monitor reads step count plus the **three most recent action–observation pairs** at checkpoints 5, 8, 11, …; aborts when score ≥ τ, with τ calibrated so that **at most 5% of successful calibration runs** would be stopped.
- **Result.** Cost per *retained* solve falls **4.5%** on SWE-bench Verified (Luna) and **24.2% / 30.6% / 48.1%** on SWE-bench Pro (Luna / Sonnet / Sonnet+Qwen3-8B). Successful runs preserved: **99.0% / 72.2% / 91.6% / 83.2%** [T 1]. Tier AUC is only **0.622 / 0.614**. Measured false-kill at the frozen threshold is **1.0%** on Verified and **27.8%** on Pro/Luna — i.e. **the 5% calibration did not hold out of distribution, it became 27.8%**. Monitoring costs **$0.000237 per check**; GPT judges cost **9–74×** more per check. Break-even requires `g > m + r` (execution savings > monitoring + false kills); doubling the monitoring charge moves Verified from −4.5% to −2.9%, and at **5× it becomes +1.8% (a net loss)**.
- **Detector cost.** $0.000237/check; ~1 check per 3 tool steps.
- **Ablation vs single system.** Four system/benchmark settings, compared against six general-purpose judges reading identical windows.
- **Admitted failure mode.** Threshold is "an empirical calibration constraint, not a guarantee under distribution shift"; trained exclusively on GPT-generated trajectories; Python-only; Pro covers three repos.
- **Applicability.** Shows the *shape* of a defensible detector budget: cheap per check, net saving only on expensive tasks. On Verified-class tasks the entire win is 4.5% — smaller than the noise in our own 1.3× guardrail.

#### P4. Hard Issue, Weak Model, or Bad Run? `[abs]`
arXiv:2611.08285v1, preprint, submitted **2026-11-05**. <https://arxiv.org/abs/2611.08285>

- **Result.** A reference score that never looks at the run reaches **0.92 AUROC**, while trajectory predictors reach "about **0.62**"; the increment over the difficulty prior is "**statistically equivalent to zero**". Within an issue, *knowing which model ran predicts failure better than their predictors*. "Per-issue switching gains nothing on held-out runs."
- **Admitted failure mode.** "Current predictors cannot tell the causes apart, as they mostly predict the issue and the model, not the run."
- **Applicability.** Second independent replication of P2's confound, on SWE-bench Verified. Consistent enough that a within-run detector is now the *default-suspect* design.

#### P5. Divergent Paths: outcome-predictive representation of agent trajectories `[full]` — **the dissenting positive**
arXiv:2610.19614v1, preprint, submitted **2026-10-14**. <https://arxiv.org/abs/2610.19614>

- **Method.** Learns a shared embedding space over actions + observations; fits **per-task discriminators with stratified 3-fold CV over each task's rollouts (~33 rollouts per task)**.
- **Result.** Within-task AUROC **up to 0.950±0.009** on Tau2-Bench/DS-V3.2 [T 1]; best per-task discriminator 0.77–0.95. Deployed early stopping saves **47.3±1.8%** training FLOPs but pass@1 drops **35.0±3.6 → 31.9±3.7 (−3.1 pp)** [T 3].
- **Admitted failure mode.** The learned space is **domain-bound**: on unseen benchmarks AUROC falls "to or below the AUROC of the untrained encoder regardless of pool size"; AgentClinic is the consistent low performer because outcomes there "are often not determined by the trajectory."
- **Applicability.** This is the strongest counterexample, and reading it carefully *supports* rather than refutes the negative consensus: the 0.95 exists only (a) on customer-service/tool benchmarks, (b) with **per-task fitting on ~33 rollouts of that same task**, which a one-shot eval run does not have, and (c) it still cost 3.1 pp of pass@1 at deployment. It does not transfer across domains.

#### P6. Fail-Fast, Restart-Smart `[full]`
arXiv:2608.03222v1, preprint, submitted **2026-08-04**. <https://arxiv.org/abs/2608.03222>

- **Result.** 0.6B monitor trained with terminal + dense fail-to-pass supervision on observable prefixes (no logits, no hidden states) saves **14.6%–20.4%** of execution tokens **at a target 5% FPR** (20.4% vs 12.5% for a per-step AgentStop adaptation). Restart-with-diff-overlay at a **25% target FPR** raises resolution **66.6% → 71.8%** while cold restart reaches only **66.8%**. Monitor overhead **0.1%–0.6%** of compute; 15–52× cheaper per token than the policy.
- **Admitted failure mode.** "Early termination… risks interrupting trajectories that would otherwise succeed"; evaluation limited to SWE-bench Verified with mini-swe-agent; the paper reports *target* FPRs, **not achieved ones**.
- **Applicability.** Quantifies the trade we would be making: **+5.2 pp solve requires accepting a 25% false-kill budget.** That is the price of the solve-rate gain, not a side note.

#### P7. TokenEconomizer: experience-driven black-box cost control `[full]` — **closest prior art**
arXiv:2610.22732v1, preprint, submitted **2026-10-15**. <https://arxiv.org/abs/2610.22732>

- **Mechanism (this is the one published stuck-loop terminator for coding agents).** Rule-based trigger on "repeated failure signatures, dense error output, or a trajectory that has exceeded **B_i = β·Q_p**", where **Q_p is the empirical p-th percentile of turn counts of prior *successful* tasks in the same repository** and β>1. A judge is consulted only while the trigger is open, at most once every 5 turns (interval grows by 5 up to 20 after a productive verdict). A **forced stop** fires at **κB_i, κ=2.5**, and only when a *strict stall rule* holds — **all three** of: (i) edit churn: ≥4 edits to one file **or** edits to ≥3 files; (ii) ≥6 error outputs; (iii) ≥20% of failing outputs repeating an earlier failure signature (Appendix F, verbatim).
- **Result.** 277 SWE-bench tasks across 6 repos, five agent–model settings, three seeds. Mean cache-aware cost falls **18.4%–30.9%** including all controller overhead; Pass@1 improves in four settings and falls **1.0 pp** on Opus 4.8 (vs 1.8–10.3 pp for simply lowering reasoning effort). Judge consulted **9.9–12.2 times per stream**, redirects **3.9–5.3**, aborts **2.0–2.2**.
- **Admitted failure mode.** "A **difficult but productive task may be mistaken for a failure loop**"; results are "related but independent fixes rather than actual user sessions"; and **no false-positive count is reported** anywhere in the paper.
- **Applicability.** Two direct consequences for ROF. (1) Its forced-stop condition is **conjunctive and edit-centric**: a run with 99 reads, zero writes and few errors has **zero edits and almost certainly <6 error outputs**, so *this detector structurally cannot fire on our loop.* Our failure mode is outside the definitional scope of the field's best published terminator. (2) Its `B_i = β·Q_p(successful turn counts)` is a **data-driven budget** we can adopt cheaply and without any classifier.

#### P8. Push Your Agent: quantitative goal persistence `[abs]`
arXiv:2605.23574v1, preprint, submitted **2026-05-22**. <https://arxiv.org/abs/2605.23574>

- **Result.** PushBench measures "repeated work, duplicate submissions, false completion, and progress drift" directly instead of behind a final success flag. A state-tracking retrieval controller reaches **69–78% success while eliminating duplicate submissions**; a backlog-tracking controller reaches **25–50%** where standard and completion-gated controllers complete **no** task instances. Claude Code (Sonnet 4.6) and Codex CLI (gpt-5.4) "solve many 50-artifact tasks but drop to **3 out of 9** successes per condition at 100 artifacts."
- **Applicability.** Evidence that *state-tracking controllers* (not predictors) can enforce progress at scale, and that frontier CLI agents degrade sharply as artifact count rises — our 99-call/0-write run is consistent with that degradation.

### 3.2 Angle 2 — termination policies that beat a fixed cap

#### P9. Knowing When to Stop: The Evidence-Action Gap in Coding Agents `[full]`
arXiv:2610.31895v1, preprint, submitted **2026-10-22**. <https://arxiv.org/abs/2610.31895>

- **Method.** 8 frontier model configurations, **19,371 trajectories**, 9,684 complete pairs, 4,344 persistent-barrier pairs. Adds an `escalate_to_human` tool alongside `bash`/`edit_file`/`task_complete`, plus a Qwen3-Coder-30B external monitor (rule-based reminder + pre-filled ticket) for models that ignore the tool.
- **Result.** The tool cuts **budget exhaustion from 34.0% to 6.7%** (fivefold, §3.2.1). The monitor raises end-to-end repair success after environment restoration from **7.6% → 21.8% (p<0.001)** on **102 held-out SWE-bench Verified tasks** (§4).
- **Detector cost.** One extra 30B model call at trigger points (not quantified in tokens).
- **Ablation vs single system.** Tool vs monitor vs baseline across 8 configurations; single-system for the monitor result (102 tasks).
- **Admitted failure mode.** **No false-positive rate is reported.** A fired escalation "is not yet a useful handoff: tickets reliably name the blocker but often omit the agent's own progress and verification status." Evaluation limited to "inference-time scaffolds under controlled sandbox faults and rule-based evaluators."
- **Applicability.** The closest thing to a "declare blocked or justify continuing" gate with numbers. Note what it changes: it converts *silent budget exhaustion* into an *explicit, scorable end state* — it does not make the run solve more. That is a measurement-quality intervention as much as a capability one, and it is cheap to test in ROF.

#### P10. Judged Useless, Queried Anyway `[abs]`
arXiv:2610.06191v1, preprint, submitted **2026-10-05**. <https://arxiv.org/abs/2610.06191>

- **Result.** Across 7 agents, agents call a failing source's results useless **97–100%** of the time, "yet most of them rarely stop on that judgment." A stopping rule or call cost in the prompt "is followed **at most partly**." Stopping follows the evidence *only* when "the harness enforces an integration step that makes the agent answer after five consecutive results it judged useless." Pre-registered replication on **300 fresh questions**.
- **Admitted failure mode.** Early stops "regardless of evidence" appear when memory permission/reasoning mode is enabled.
- **Transfer limit.** Retrieval/QA environment with controlled source failures, not coding.
- **Applicability.** The single best measured argument that **prompt-level budget awareness is dead** and that only harness-enforced structure changes stopping behaviour. It is the mechanism-level explanation for our own dead wrap-up nudge (2.64× tokens per solved task).

#### P11. Your Wish Is My Commit: over-compliant refactoring `[abs]`
arXiv:2610.36346v1, preprint, submitted **2026-10-26**. <https://arxiv.org/abs/2610.36346>

- **Result.** Coding agents "keep editing after no edit improves the code": **rewriting up to 31× the original codebase before first stopping**; **resuming edits even after declaring the task complete**; and **none** stopped on recognizing an already-optimized codebase. 50 algorithmic + 10 OO suites, Codex and Claude Code, 5 models, >3,000 turns; six behavioural signals of over-compliance.
- **Admitted failure mode.** Consequences "remain largely understudied"; open-ended refactoring with no external stopping criterion (so it does not measure solve rate).
- **Applicability.** The mirror image of our failure: agents both refuse to stop *and* refuse to stop *productively*. Any "declare done" gate must survive the fact that self-declared completion is not a stopping event.

#### P12. Monitor Accuracy Isn't the Answer: a decision-value framework `[abs]`
arXiv:2610.36544v1, preprint, submitted **2026-10-27**. <https://arxiv.org/abs/2610.36544>

- **Result.** 194 externally evaluated model executions on 19 SymPy issues from SWE-bench Verified; a **2.29-point apparent restart gain** did not replicate, and a second model showed a **42.29-point loss**; **zero of 103 live comparisons met the specified certification bar**. A monitor's value depends on "the replacement attempt's residual-budget success probability."
- **Admitted failure mode.** Depends on stated sampling assumptions; no positive replication on prospective cohorts.
- **Applicability.** This is the strongest warning about restart-style interventions and about our own 8/9-but-never-solved observation: partial-progress gains do not convert, and restart effects do not replicate across models.

#### P13. Fail-fast coverage from Angle 1 also serves here
See P6: restarting *with the interrupted diff available as an overlay* is what produced the +5.2 pp, not restarting cold (66.8% vs 71.8%). **Cold restart ≈ no-op.**

### 3.3 Angle 3 — the "never writes the deliverable" trap

I found **no study that measures forcing an early artefact write** (skeleton-first / write-then-refine) in coding agents. What exists is adjacent and, read carefully, points the other way.

#### P14. ECLoop: preventing premature commitment with an evidence-conditioned execution layer `[full]`
arXiv:2607.28815v1, preprint, submitted **2026-07-30**. <https://arxiv.org/abs/2607.28815>

- **Method.** An execution layer between agent and repository compiles evidence conditions from the issue + repo structure and **postpones actions whose required conditions are unmet**. All **500** SWE-bench Verified instances; 2 models (MiniMax-M2.5, GPT-5-mini) × 2 scaffolds (mini-swe-agent v2, Codex CLI v0.144.4).
- **Result.** **+4.8 to +11.8 pp Pass@1**, "no additional inference cost", average tokens **−12.1%** (179K → 157K) and total cost −1.5–10.2%. Ablation (100 instances, mini-swe-agent v2 + GPT-5-mini) [T 4]: full **68** solved; **−commitment check 58 (−10)**; −state update 59 (−9); −guidance 63 (−5); **baseline 47 (−21)**; spec-as-natural-language 58.
- **Detector cost.** Zero extra inference; deterministic condition compilation (but it does parse the issue text with a model).
- **Ablation vs single system.** Controlled 5-way ablation plus a paraphrase control (structured conditions beat an NL summary by 10 points).
- **Admitted failure mode.** "Dependence on issue quality", "shared-model evidence operations", SWE-bench Verified only.
- **Applicability.** **The measured winner is a gate that blocks a premature action, not one that forces an early artefact.** Removing the commitment check costs 10 pp — the largest single ablation effect in this entire review. This is the design pattern to copy; "force a write at step k" is the untested inverse and is exactly the behaviour ECLoop was built to prevent.

#### P15. Gated spec-driven workflow: audit-trail study `[abs]`
arXiv:2611.05094v1, preprint, submitted **2026-11-03**. <https://arxiv.org/abs/2611.05094>

- **Result.** A reviewer subagent checks each artefact against a fixed defect list; an executor may only touch listed files; a **deterministic file-inventory check** runs after each task block. 9 features / 7 projects, author-run: **5 of 9** features complete, **288 of 581** planned tasks done. The executor left **1** undeclared change in the deliverable folder and the inventory check caught it. The orchestrator recorded undeclared changes in **11 of 247** block checks but **stopped work in only 4**.
- **Admitted failure mode.** "The weak point was the orchestrator, which runs and judges its own checks."
- **Applicability.** Direct evidence on gate firing rates: a real gate in a real harness fired an *intervention* **4 times in 247 checks (1.6%)** — the same order as our 3-identical-action guard that never fired. Gates that rarely fire are the norm, not a bug in our implementation, and a gate's value must be judged on the runs where it would have fired.

#### P16. On the risk of coding before testing `[abs]`
arXiv:2607.05139v1, preprint, submitted **2026-07-06**. <https://arxiv.org/abs/2607.05139>

- **Result.** Generating tests *after* faulty code reduces fault detection **14% vs 25%** for independently generated tests — i.e. artefact coupling (writing one artefact in the presence of another) measurably degrades the second artefact.
- **Applicability.** A caution, not a solve-rate result: the order in which artefacts are written changes their quality. It argues for *independence* between the write and the check, which matters if we ever have the agent write its own verification.

#### P17. SWE-Marathon: ultra-long-horizon software work `[abs]`
arXiv:2606.07682v1, preprint, submitted **2026-06-05**. <https://arxiv.org/abs/2606.07682>

- **Result.** 20 long-horizon tasks; logged attempts average **27.2M total tokens**; frontier agents solve **<30%**. Failures "often arise from poor self-verification, **self-reported infeasibility, and premature termination**"; reward hacking in **13.8%** of rollouts.
- **Applicability.** The essential counterweight. At long horizons the *measured* dominant pathology is **giving up too early**, not persisting too long. Any termination policy we adopt must be justified by evidence that our tasks are on the other side of that line.

### 3.4 Angle 4 — budget calibration

#### P18. SPLIT: Sample, Mix, or Verify? `[abs]`
arXiv:2610.23255v1, preprint, submitted **2026-10-16**. <https://arxiv.org/abs/2610.23255>

- **Method.** Plans how many samples to draw, from which models, and how much to spend verifying, under a fixed budget, from a **small labelled pilot**; exact formula for P(correct submission) under accept/reject and top-score selection, fit by empirical Bayes.
- **Result.** "Lands within **0.02–0.8 percentage points** of the best allocation in hindsight, while **every fixed rule we test loses at least 2.5 points** in some setting." 7 benchmarks, 26 models, **175 agent systems**, 6 kinds of real verifier (including SWE agents).
- **Admitted failure mode.** Real verifiers "accept wrong answers, reject right ones, and make correlated mistakes"; regret depends only on how well SPLIT ranks configurations.
- **Applicability.** The strongest quantitative case that **budget should be a per-task prediction, not a constant** — and that per-task budgets are learnable from a small pilot. This is the *between-task* lever the negative results keep pointing to.

#### P19. It's the Problem, Not the Path: budget and difficulty confounds `[abs]`
arXiv:2609.03436v1, preprint, submitted **2026-09-03**. <https://arxiv.org/abs/2609.03436>

- **Result.** A **trace-blind difficulty proxy reaches AUROC 0.873** on 192K DeepSeek-R1 generations — *inside the published probe range* — while the closest published positive reconstruction (0.849 pooled) is **statistically indistinguishable from chance within-problem at all ten anchors (0.496 at t=4)**. Only **1 of 178** problem-model cells survives as prefix-limited. Where the matched budget lies inside the restart grid, **continuing the model's own prefix beats restarting in 9 of 9** cases, "predominantly compute compression rather than expanded reachability."
- **Transfer limit.** Math reasoning (MATH), two small open models — not coding.
- **Applicability.** Independent third replication of the difficulty confound, from a different field. Its recommendation is the evaluation rule I would hold ROF to: "High pooled probe AUROCs cannot by themselves establish within-attempt information; a question-only baseline or within-problem evaluation is required."

#### P20. Rethinking Inference-Time Scaling in Local Computer-Use Agents `[abs]`
arXiv:2607.28573v1, preprint, submitted **2026-07-30**. <https://arxiv.org/abs/2607.28573>

- **Result.** On OSWorld (Qwen3-VL-8B/30B-A3B, UI-TARS-1.5-7B, OpenCUA-7B): "additional computation often yields diminishing returns **while changing failure modes**"; contextual scaling gains "saturate as token cost increases"; temporal scaling "does not substantially improve task success" and "longer horizons often extend erroneous trajectories rather than correct them." Crucially, failures "shift from repetitive or stalled trajectories toward **premature false successes**."
- **Transfer limit.** Computer-use, small local models.
- **Applicability.** The best available description of what our 3.3× budget run did: **more budget moved the failure mode from stuck to prematurely-successful-looking, and did not produce a solve.** It also warns that at higher budget our *termination* problem may be replaced by a *false-completion* problem.

#### P21. FlowPlan `[abs]` and TokenEconomizer cost results — the structural alternative
arXiv:2611.13241v1 (2026-11-09) <https://arxiv.org/abs/2611.13241>; P7 above.

- **Result.** Replacing post-edit re-reading/testing with explicit verification at an abstract program level cuts tokens to **0.32–0.35×** baseline on SWE-bench Verified (Haiku 4.5) and 0.73–0.74× (Sonnet 5), with **no significant change in resolution**. TokenEconomizer achieves 18.4–30.9% with Pass@1 up or −1.0 pp.
- **Applicability.** Cheaper than any detector and already measured: the read-heavy loop can be *structurally prevented* rather than *detected*. If our 99 calls are mostly re-reads, this is a bigger lever than a stuck classifier.

### 3.5 Angle 5 — adversarial cases and false-positive cost

| Evidence | Number | What it establishes |
|---|---|---|
| StopLoss frozen threshold out of distribution (P3) | calibrated ≤5% → measured **27.8%** false-kill on SWE-bench Pro | Calibration does not transfer across difficulty; the cost of wrongly killing a good run is the binding risk |
| FailFast operating point (P6) | **+5.2 pp** solve only at a **25%** target FPR | The measured exchange rate between killing runs and gaining solves |
| Divergent Paths deployment (P5) | **−3.1 pp pass@1** for 47.3% FLOP saving | Even a *working* within-task detector (0.95 AUROC) costs real solves |
| Abort-only vs allocation (P2) | **−17.2 to −49.0 tasks**; monitor + Thompson sampling **−4.2 to −0.6** (CIs span 0) | Aborting runs is dominated by reallocating budget across tasks |
| 0/103 certification (P12) | zero of 103 live restart comparisons met the bar | Restart effects do not replicate |
| TokenEconomizer strict stall rule (P7) | requires ≥4 edits/1 file **or** ≥3 files **and** ≥6 errors **and** ≥20% repeat signatures | **A zero-write, low-error loop cannot trigger the field's best stuck terminator.** Our repetition guard fails for a *second, structural* reason beyond action variation |
| Gate firing rate (P15) | intervention in **4 of 247** checks | Rarely-firing gates are normal; our never-firing guard is not anomalous |
| Detector labels vs humans (P22 below) | verification detector agrees **67/81**; 14 disagreements in 4 patterns | Trace-level detectors mislabel in systematic, not random, ways |
| Budget changes measured outcome (P22) | one model **21/30 → 27/30** as step/token budgets change | Budget is a confound in *our own* A/B results, not just a knob |

#### P22. What the Trace Shows and What the Label Says `[abs]`
arXiv:2610.30491v1, preprint, submitted **2026-10-21**. <https://arxiv.org/abs/2610.30491> — three trace-level detectors audited against human labels: final correctness 67/67, fault impact 19/19, fault relevance 18/18, but **verification only 67/81**, with all 14 disagreements in four recurring patterns; a key failure is "silent tool-output override"; and reported correctness moves with execution limits (**21/30 → 27/30** for one model).

#### P23. AgentHorizon: agentic judges for long-horizon tasks `[abs]`
arXiv:2610.11050v1, preprint, submitted **2026-10-08**. <https://arxiv.org/abs/2610.11050> — the best agentic judge (GPT-5.5) reaches **80.9% balanced accuracy** on long computer-use tasks (up to 300 screenshots/actions), with the authors noting the need for judges "capable of locating and verifying often hidden evidence that a task was properly completed inside long interaction histories." This is the accuracy ceiling to assume for any LLM-judge stuck classifier: **~1 in 5 verdicts wrong**, at judge cost.

#### P24. Doomed from the Start: recall-controlled probe cascade `[abs]`
arXiv:2607.06503v1, preprint, submitted **2026-07-07**. <https://arxiv.org/abs/2607.06503> — the only paper that treats false kills as a *constraint* rather than a report: a distribution-free calibrated detector at each early round with a **user-specified global recall** on eventually-successful episodes, "an exact post-selection recall guarantee." Saves **60.2%** (TextCraft) and **54.9%** (WebShop) tokens at **90% recall**, 45.0%/41.5% at 95%; achieved recall within 1 SD of target in **all 24** configurations; **1.5–8.8×** more compute saved than baselines. **Behaviour-only monitoring is "consistently weaker", and adding behavioural features to hidden-state probes "provides no further gain."**
- **Applicability.** If we ever build a detector, **recall-on-successful-runs must be the primary specification**, not AUC. But note the transfer limits: web agents, white-box hidden states, and behavioural features contributed nothing on top of hidden states.

#### P25. CodeRescue (WITHDRAWN — excluded)
arXiv:2607.19338, "Budget-Calibrated Recovery Routing for Coding Agents" — **withdrawn at the request of ByteDance** ("submitted before completing the company's required internal review"). Its conformal-risk-control routing claims (a frontier point exceeding always-escalate at 35% of mean recovery cost) are **not usable evidence**. Listed here so nobody re-discovers and cites it.

---

## 4. Implications for ROF

**Closest prior art.** TokenEconomizer (P7) is the nearest thing to what we contemplated: a per-run, rule-based stuck terminator with redirect-then-abort and a data-derived turn budget `β·Q_p`, measured on 277 SWE-bench tasks across five agent–model settings with overhead included. StopLoss (P3) is the nearest calibrated monitor. Continue-or-Restart (P1) and Disentangling Difficulty (P2) are the nearest *evaluations of the idea itself*, and both are negative for the within-run version.

**What is genuinely different about our situation — and possibly novel.**
1. **Our stuck mode is read-only.** Every published stall rule keys on edit churn and error density (P7's conjunctive rule; StopLoss's action–observation pairs; FailFast's prefixes trained on fail-to-pass signals). A run that makes 99 reads, zero writes and few hard errors is *outside the definitional scope* of all of them. That is a real gap, and it explains both why our 3-identical guard never fired and why the literature has nothing to say about our exact trace.
2. **We have a within-task pair.** Our 0/9 → 8/9 result at 3.3× budget is a *mixed-outcome task*, which is precisely the condition P1 says is required for within-task discrimination to be even definable. We are unusually well placed to measure this — but the measurement has to be within-task AUC, not pooled.
3. **Our budget is a step cap and a token cap simultaneously**, and P22 shows measured outcomes move with those limits. Any A/B we run on termination must hold both fixed across arms.

**What is not novel at all.** "Detect a stuck agent from its trace and kill it" is heavily worked and mostly negative. A per-run identical-action repetition guard is AutoGPT-era folklore with, as far as this search reaches, **no measured precision/recall anywhere** — the 2026 papers do not even cite it as a baseline, which is itself informative.

**What the literature says is likely to fail.**
- A learned within-run stuck classifier: chance-level within-task AUC in the only large studies (P1, P2, P4), and it must clear **~0.84** to beat allocation (P2).
- Prompt-level budget awareness / wrap-up nudges: "followed at most partly" (P10), consistent with our own 2.64× negative.
- Abort-and-restart: dominated by continuation in 216/224 configs (P1); cold restart is a near no-op (P6: 66.8% vs 66.6%).
- LLM-judge verdicts on long traces: ~80.9% balanced accuracy ceiling (P23), at judge cost, with systematic mislabelling (P22).
- Any gate with a narrow trigger: fires in ~1.6% of checks at best (P15) — which is fine only if we accept that its value is in the rare run it catches.

**What the literature supports building instead.** (a) `B_i = β·Q_p` over our own *successful* turn counts, per task class (P7) — a budget with an empirical anchor rather than 120; (b) between-task allocation (P18, P2); (c) a deterministic commitment gate in the ECLoop sense — block the *next read* until an artefact exists (P14, −10 pp if removed); (d) an explicit blocked/hand-off end state so budget exhaustion is scored, not silent (P9: 34.0% → 6.7%).

---

## 5. Evidence strength

**Claims the sources actually support.**
- Within-run, prefix-only failure prediction for coding agents is at chance on mixed-outcome tasks: within-task AUC **0.48–0.52** in the two largest coding corpora (P1, with 38,447 pairs from 1,737 tasks; P2), **0.496** in a math-reasoning replication (P19), and a **near-zero increment over a difficulty prior** in a third coding study (P4).
- Pooled AUROC numbers of 0.85–0.945 are reproducible from a model that never looks at the run (P2's difficulty oracle 0.945; P4's prior 0.92; P19's 0.873).
- Uninterrupted continuation beats abort-and-restart on both solve rate and mean tokens in 216 of 224 configurations (P1).
- Abort-only intervention loses 17.2–49.0 tasks versus the same budget allocated across tasks (P2).
- A calibrated 5% false-kill target became a measured 27.8% out of distribution (P3); a solve-rate gain required accepting 25% FPR (P6); a working 0.95-AUROC detector still cost 3.1 pp pass@1 (P5).
- Prompt-level stopping rules are followed "at most partly"; harness-enforced structure is what changes stopping (P10).
- Gates on action availability have a large measured effect: removing ECLoop's commitment check costs 10 pp Pass@1 (P14, Table 4).
- A rule-based stuck terminator with redirect-then-abort reduces cost 18.4–30.9% at −1.0 pp worst-case Pass@1, with overhead counted (P7).
- Long-horizon agents *also* fail by terminating early (P17), and higher budgets shift failures toward premature false success rather than solving (P20).

**Claims we might want to make that the sources do NOT support.**
- ❌ *"We can detect our stuck run from the trace."* Nothing measures read-only/no-write loops. The nearest rule-based terminator cannot fire on one (P7's conjunctive edit+error condition).
- ❌ *"Our repetition guard is a reasonable defence."* No paper reports precision/recall for identical-action guards; the 2026 work does not use them as a baseline.
- ❌ *"Early stopping will save tokens."* On Verified-class tasks the measured saving is 4.5% (P3) and turns negative at 5× monitoring cost; abort-only loses tasks outright (P2).
- ❌ *"A 3-identical-action guard failing is an implementation bug."* Gates firing rarely is normal: 4 interventions in 247 checks in a purpose-built gated harness (P15).
- ❌ *"More budget would have solved it."* More budget moved 0/9 → 8/9 without a solve in our run, which is what P20 describes as extending an erroneous trajectory; P12 shows restart/extension effects failing to replicate (42.29-point loss on a second model).
- ❌ *"An LLM judge can adjudicate stuck-ness."* 80.9% balanced accuracy is the best measured ceiling on long traces, in a different domain (P23), and trace-level detectors systematically disagree with humans on verification status, 67/81 (P22).
- ❌ *"Peer-reviewed consensus."* Every source here is an unreviewed arXiv preprint from 2026.

---

## 6. Gaps

1. **No measurement of read-only / no-write loops.** Zero papers define, count, or detect the failure mode we actually hit. Prevalence, conditional success probability, and the best predicate are all unmeasured. This is the main original contribution available to us, and it is an experiment, not a citation.
2. **No measurement of forced early write** (skeleton-first, write-then-refine, minimal-patch-first). The one intervention in this family that was measured (ECLoop) is its *inverse* — delay commitment until evidence exists.
3. **No per-task success-plateau estimate** for coding agents from live runs. P1 says continuation keeps paying to 75k; P20 says CUA scaling saturates. Nobody reports the budget at which P(solve | task) flattens per difficulty tier. The nearest principled substitutes are percentile-of-successful-runs budgets (P7) and pilot-based allocation (P18).
4. **Zero reported false-positive counts** for the two most relevant terminators (P7 explicitly, P9 by omission). We would be first to report "runs the gate would have killed that later succeeded."
5. **No action-class-entropy stuck signal** for coding agents. Entropy-based work exists only in VLA/robotics control (e.g. arXiv:2608.24488, 2606.20479 on uncertainty for navigation), which does not transfer. Treat action-entropy as an untested idea, not a method.
6. **AgentBoard's progress-rate numbers** (2024) were not retrievable at abstract level; if we want plan-progress estimation as a baseline we must read the paper.
7. **Replay vs live.** P1, P2 and P12 all use recorded-run replay. No one has published a live, prospective within-run termination trial on coding agents that beats continuation. That experiment does not exist yet — which is either an opportunity or a warning, depending on appetite.

---

## 7. Single most promising signal, and what must be measured first

**Signal: the deliverable-existence predicate, used as a gate rather than a detector.** At fixed checkpoints, evaluate a deterministic predicate — *does the required artefact exist, is it non-empty, does the task's public check run* — and when it is false, **block further read-only actions and force a first write**, keeping the run alive.

Why this and not a classifier: it is the only mechanism in this literature whose measured effect comes from **enforcement** rather than prediction (P10's harness-enforced integration step; P14's −10 pp commitment-check ablation; P15's deterministic inventory check catching the one undeclared change), it costs **zero inference** (P14), it is immune to action variation by construction, and it does not require clearing the 0.84 within-task AUROC bar (P2) that every learned monitor has failed.

**What must be measured before adopting it (in order):**

1. **Prevalence on our own traces.** Fraction of TB4.1.0 runs reaching step *k* with zero writes, and `P(success | zero writes by k)`. If most successful runs also write late, the gate is a false-positive machine and we stop here. Compute this only on **mixed-outcome tasks** — P1's precondition; a task we solve 0/9 has no within-task signal to estimate.
2. **Shadow-mode false-kill count.** Run the gate inert on all successful runs and count how many it *would* have blocked. Report this number before any arm runs; do **not** assume a 5% calibration holds (P3 measured 27.8% out of distribution).
3. **The rewrite tax.** If the forced write is wrong, measure the tokens spent rewriting it. P16 shows artefact coupling degrades the second artefact (14% vs 25% fault detection), so a premature skeleton may poison the later patch.
4. **Two-arm, token-matched, fresh development tasks**, scored on solve rate **and** tokens per solved task under the 1.3× guardrail, with the 8/9-no-solve outcome recorded as *unsolved* — never as partial credit (P1 replay's zero-credit-for-partial-edits rule is the honest convention).
5. **If we ever do build a learned monitor**, its **within-task AUROC must exceed ~0.84 at our 1M-token budget** before it is allowed to kill anything (P2), and its recall on eventually-successful runs must be the primary spec, with a stated target (P24's recall-controlled design).

**Falsification condition.** If step 1 shows `P(success | zero writes by k) ≥ P(success)`, the gate is not detecting anything and the correct action is to stop building termination machinery and spend the effort on between-task budget allocation (P18) and structural read-cost reduction (P21) instead.

---

## 8. Sources

**Kept.**
| Source | Why |
|---|---|
| 2610.35971 Continue or Restart? (2026-10-26) | The direct negative test of our proposed detector; 147k traces; exact decomposition |
| 2610.05572 Disentangling Difficulty (2026-10-04) | Supplies the 0.84–0.93 adoption threshold and the abort-vs-allocation comparison |
| 2610.25525 StopLoss (2026-10-18) | Calibrated monitor with measured cost-per-retained-solve and a measured calibration failure |
| 2610.22732 TokenEconomizer (2026-10-15) | Closest prior art: the only published coding-agent stuck terminator, with its trigger conditions |
| 2610.31895 Knowing When to Stop (2026-10-22) | Only paper with a measured escalation gate (34.0% → 6.7%) |
| 2610.06191 Judged Useless (2026-10-05) | Measured proof that prompt-level stopping rules are weak and enforcement works |
| 2608.03222 Fail-Fast/Restart-Smart (2026-08-04) | The FPR-for-solve-rate exchange rate, measured |
| 2610.19614 Divergent Paths (2026-10-14) | Necessary dissent: within-task AUC 0.95 exists — under per-task fitting, in another domain, at −3.1 pp |
| 2611.08285 Hard Issue/Weak Model (2026-11-05) | Second replication of the difficulty confound (0.92 vs 0.62) |
| 2610.36544 Monitor Accuracy Isn't the Answer (2026-10-27) | 0/103 live restart certifications; decision-value framing |
| 2607.28815 ECLoop (2026-07-30) | The gate design that actually works, with a clean 5-way ablation |
| 2610.23255 SPLIT (2026-10-16) | Budget allocation that beats every fixed rule by ≥2.5 points |
| 2609.03436 It's the Problem, Not the Path (2026-09-03) | Third confound replication; 9/9 continuation beats restart |
| 2607.28573 Inference-Time Scaling in CUAs (2026-07-30) | The failure-mode-shift account of our 3.3× budget result |
| 2606.07682 SWE-Marathon (2026-06-05) | The counterweight: premature termination is a dominant long-horizon failure |
| 2607.06503 Doomed from the Start (2026-07-07) | Only recall-controlled detector design; behavioural features add nothing |
| 2610.36346 Your Wish Is My Commit (2026-10-26) | Agents resume editing after declaring completion — kills naive "declare done" gates |
| 2610.30491 Auditing Behavioral Detectors (2026-10-21) | Detector-vs-human label disagreement; budget confounds measured outcomes |
| 2611.05094 Gated Spec-Driven Workflow (2026-11-03) | Gate firing rate: 4 interventions in 247 checks |
| 2605.23574 Push Your Agent (2026-05-22) | State-tracking controllers beat completion-gated ones; degradation at 100 artifacts |
| 2610.11050 AgentHorizon (2026-10-08) | LLM-judge accuracy ceiling (80.9%) on long traces |
| 2611.13241 FlowPlan (2026-11-09) | Structural read-cost reduction at equal resolution |
| 2610.28834 Where Should a Stuck Agent Spend (2026-10-20) | Diminishing returns of added compute; model-chosen allocation worse than random |
| 2605.12947 Always-Valid Release (2026-05-13) | Principled stopping with a false-release guarantee (weak numbers) |
| 2401.13178 AgentBoard (**2024-01, older**) | Origin of the progress-rate metric; cited as background only, numbers unverified |

**Rejected.**
| Source | Why |
|---|---|
| 2607.19338 CodeRescue (2026-07-21) | **Withdrawn** at the employer's request; claims unusable |
| 2610.15884 RetryBench (2026-10-11) | Retry *permission* phrasing sensitivity, not termination; no cost/success numbers |
| 2607.05139 Coding before testing (2026-07-06) | Cited as a caution only; measures fault detection, not solve rate or early writes |
| 2608.16742 TDD-Agent (2026-08-17) | Test-first helps, but no pass@1 numbers, no cost, abstract-level only |
| 2609.22222 Reproduce Official Statistics (2026-09-02) | Retry budget in a stats-reproduction niche; not transferable |
| 2610.08722 Compaction/history (2026-10-06) | Compaction, not termination |
| 2611.06863 Rethinking Self-Repair (2026-11-04) | Retry success drivers; adjacent to our problem but not about stopping |
| 2605.08715 AgentForesight (2026-05-09) | Multi-agent auditing; different unit of analysis |
| Robotics/VLA entropy work (2608.24488, 2606.20479) | Wrong domain; cited only to establish that coding-agent action-entropy is unmeasured |
| 2607.27415, 2611.06868 | Inference-time scaling via memory / best-of-N; not termination |

---

## 9. Search log

Tools: `papers_search_arxiv` (returned truncated payloads beyond ~3 KB in this session, so it was used only for single-result spot checks), `papers_search_semantic` (same truncation), and — for everything substantive — the **arXiv API** (`export.arxiv.org/api/query`) and **arXiv HTML full texts** (`arxiv.org/html/<id>v1`) fetched via `fetch_content`, with exact passages pulled by `get_search_content(findText=…)` on the stored responses. Full texts read this way: 2610.35971, 2610.25525, 2610.31895, 2610.19614, 2607.28815, 2610.05572, 2608.03222, 2610.22732.

Queries issued (arXiv API, sorted by `submittedDate` descending unless noted):
1. `ti:"early failure prediction"` → 2610.35971, 2608.03222, 2605.08715, 2607.26704
2. `abs:"early stopping" AND abs:"agent"` (20 results) → 2610.25525, 2610.05572, 2610.06191, 2610.19614, 2607.06503, 2608.18884, 2610.34943
3. `abs:"repetitive" AND abs:"agent" AND abs:"tool"` → noise; nothing on repetition guards
4. `ti:"budget" AND abs:"coding agent"` → 2607.19338 (withdrawn), 2604.10182, 2609.22222
5. `abs:"token budget" AND abs:"agent"` (25) → 2610.28834, 2610.08722, 2611.06863
6. `abs:"loop" AND abs:"agent" AND abs:"detect"` → truncated; retried via `ti:"loop"` (returned only control-loop noise)
7. `abs:"premature" AND abs:"coding agent"` → 2610.31895, 2607.28815, 2611.05094, 2606.07682, 2605.12947
8. `abs:"test-first" OR abs:"write the file first" OR abs:"skeleton-first"` → 2608.16742 only; **no artefact-first study exists**
9. `abs:"repetition" AND abs:"LLM agent"` → noise
10. `abs:"unproductive" AND abs:"agent"` → 2610.22732, 2610.19439, 2610.36346
11. `abs:"action entropy" OR abs:"action-space entropy"` → robotics only (confirms gap)
12. `abs:"read-only" AND abs:"coding agent"` → no relevant hit for read-only loops
13. `abs:"duplicate" AND abs:"tool call"` → 2605.23574, 2610.15884
14. `abs:"empty patch" OR abs:"no edit" AND abs:"agent"` → noise
15. `abs:"repetitive action" OR abs:"action loop"` → robotics/embodied noise
16. `abs:"progress rate" AND abs:"agent"` → AgentBoard 2401.13178 (2024) plus noise
17. `abs:"first edit" AND abs:"agent"` → noise
18. `abs:"successful trajectories" AND abs:"coding agent"` → 2610.14405, 2610.13152, 2610.11050
19. `abs:"token budget" AND abs:"solve rate"` → 2610.35971, 2609.03436
20. `abs:"deliverable" AND abs:"coding agent"` → 2611.05094, 2610.13166, 2609.01481
21. `abs:"inference-time scaling" AND abs:"agent"` → 2607.28573, 2610.28834, 2611.06868
22. `id_list=2611.08285,2610.36544,2610.23255,2610.06191,2610.05572,2610.35971` for exact titles/dates/authors

Not searched: OpenAlex and dblp were not exercised as separate sources (the arXiv API covered the space; all relevant 2026 work in this area appears on arXiv first). No `web_search` / non-paper sources were used for any claim in this brief — there are no standards, vendor docs or blog claims involved.
