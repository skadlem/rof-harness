use super::session::{checks_pass, render_checks, RoundServices};
use super::Session;
use crate::agents::{Agent, AgentCtx, ImplementerAgent, PlannerAgent, ReviewerAgent, Verdict};
use crate::config::AppConfig;
use crate::context::{render, ContextBuilder, CtxState, LayerReport, Retriever};
use crate::llm::{ContextService, ExecutorService};
use crate::obs::{TraceEvent, TraceSink};
use crate::tools::ToolRegistry;
use std::sync::Arc;

/// Supervisor: Planner -> (Implementer -> Reviewer)* bounded loop.
/// Each round's verdict feedback feeds the next implementer attempt via
/// the short-term context layer. All model/tool/verdict steps land in
/// the trace sink for the eval layer.
pub struct Orchestrator {
    cfg: AppConfig,
    trace: Arc<TraceSink>,
    context: ContextService,
    executor: ExecutorService,
    /// Model serving the reviewer. Defaults to a clone of the executor
    /// (self-review) until `verify_model` is configured.
    verify: ExecutorService,
}

impl Orchestrator {
    pub fn new(
        cfg: AppConfig,
        trace: Arc<TraceSink>,
        context: ContextService,
        executor: ExecutorService,
        verify: ExecutorService,
    ) -> Self {
        Self {
            cfg,
            trace,
            context,
            executor,
            verify,
        }
    }

    pub async fn run_loop(
        &self,
        session: &Session,
        tools: &ToolRegistry,
        workdir: &std::path::Path,
    ) -> serde_json::Value {
        self.trace.emit(TraceEvent::SessionStart {
            session_id: session.id.clone(),
            goal: session.goal.clone(),
        });
        // §4.2: git is the task copy's tree-state substrate — the baseline an
        // attempt starts from, the rollback a failed attempt restores, and the
        // change set the write gate counts. Present always, harness-side only:
        // `git` is never on `ROF_ALLOW_CMDS` and `.git` is unreachable by any
        // path an agent names.
        let tree = crate::engine::tree::TreeService::new(workdir.to_path_buf());
        if let Err(e) = tree.ensure() {
            self.trace.emit(TraceEvent::ModelError {
                agent: "tree".to_string(),
                error: e.to_string(),
            });
            return serde_json::json!({ "error": e.to_string() });
        }
        // §4.3: the shared services. Both execution modes hold one of these
        // and call the same methods for skills, checks, budgets and reviewer
        // evidence, so a prompt part one mode forgets is a compile error
        // against this struct, not a silent drift between two loops.
        let svc = RoundServices {
            cfg: &self.cfg,
            trace: &self.trace,
            context: &self.context,
            executor: &self.executor,
            verify: &self.verify,
            tools,
        };
        if self.cfg.execution == "direct" {
            return self.run_direct_loop(session, &svc, workdir, &tree).await;
        }
        // Stage 2: the per-layer policy owns budgets, strategy and the
        // summarize threshold. `plan_summarized` below is the first-class path;
        // the planner's view stays on the pure `plan` because building a prompt
        // must not cost a model call before the planner's own call.
        let builder = ContextBuilder::with_policy(self.cfg.context_policy());

        // Keyword retrieval over the workdir feeds the mid-term layer,
        // so planner + implementer see relevant files beyond top-level.
        let retriever = Retriever::new(workdir.to_path_buf(), self.cfg.retrieval.clone());
        let snips = retriever.retrieve(&session.goal, self.cfg.retrieval.max_total_chars);
        let retrieved = render(&snips);
        let retrieved_files = snips.len();
        // Context accounting (stage 0), folded per task by the eval layer. The
        // retriever's snippets are echoed with their sizes so a report can say
        // what retrieval handed over (§2.6 `relevance_proxy` denominator).
        let retrieved_json: Vec<serde_json::Value> = snips
            .iter()
            .map(|s| serde_json::json!({ "path": s.path, "chars": s.content.chars().count() }))
            .collect();
        // Per-layer accounting (stage 2): every view the round loop builds
        // folds its LayerReports here. The planner's one-shot view is outside
        // the loop and is not counted.
        let mut acc = LayerAcc::default();

        // Skills, progressive disclosure: the index (names + one-line
        // descriptions) goes into the stable head of every prompt; a body is
        // injected only when the task names that skill. Both are reads through
        // the same gate as everything else — the harness calls
        // `skills.list`/`skills.view` *as* that agent, so the grant matrix
        // decides who sees what. These are prompt-construction reads, not
        // agent tool calls: they emit `SkillOp` and never `ToolCall`, because
        // folding them into tool_accuracy would quietly inflate it.
        let planner_skills = svc.skill_index("planner").await;
        let impl_skills = svc.skill_index("implementer").await;
        let reviewer_skills = svc.skill_index("reviewer").await;
        let planner_head =
            RoundServices::head_with_index(&session.ctx.long_term, &planner_skills.text);
        let impl_head = RoundServices::head_with_index(&session.ctx.long_term, &impl_skills.text);
        let reviewer_head =
            RoundServices::head_with_index(&session.ctx.long_term, &reviewer_skills.text);
        // The planner only gets bodies when it actually runs: with the planner
        // skipped there is no planner prompt to put them in, and a `reused`
        // count that includes a body nobody read would be a lie.
        let planner_reuse = if self.cfg.planner == "skip" {
            String::new()
        } else {
            svc.skill_bodies("planner", &session.goal, &planner_skills)
                .await
        };

        // A pre-check cheaper than the model that will consume the goal — the
        // one shared path, so direct mode cannot drift out of it again.
        let goal_note = svc.goal_note(&session.goal);

        // Planner (Context LLM, no tools)
        let plan_state = CtxState {
            long_term: planner_head.clone(),
            mid_term: format!(
                "goal: {}\n{}{retrieved}{planner_reuse}",
                session.goal,
                goal_note
                    .as_deref()
                    .map(|n| format!("GOAL QUALITY NOTE: {n}\n"))
                    .unwrap_or_default()
            ),
            short_term: session.ctx.short_term.clone(),
        };
        let (plan_view, _) = builder.plan(&plan_state);
        // Planner (Context LLM, no tools). Skippable: for goals that are
        // already task-shaped the call is pure overhead (see ROF_PLANNER).
        let plan_out = if self.cfg.planner == "skip" {
            crate::agents::AgentOutput {
                summary: "planner skipped".to_string(),
                data: serde_json::json!({ "tasks": [], "acceptance": [], "skipped": true }),
            }
        } else {
            let planner = PlannerAgent::new(&self.context);
            match planner
                .run(AgentCtx {
                    view: &plan_view,
                    context: Some(&self.context),
                    executor: None,
                    tools: None,
                    workdir: None,
                    trace: &self.trace,
                })
                .await
            {
                Ok(o) => o,
                Err(e) => {
                    self.trace.emit(TraceEvent::ModelError {
                        agent: "planner".to_string(),
                        error: e.to_string(),
                    });
                    return serde_json::json!({ "error": e.to_string() });
                }
            }
        };
        self.trace.emit(TraceEvent::StateTransition {
            from: "planned".to_string(),
            to: "implementing".to_string(),
        });

        // Plan tasks drive the loop: each task gets its own bounded
        // Implementer -> Reviewer rounds, and a failing task stops the run.
        let rounds = self.cfg.max_review_rounds.max(1);
        let tasks: Vec<String> = {
            let t: Vec<String> = plan_out
                .data
                .get("tasks")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            // No usable plan: fall back to the goal as a single task.
            if t.is_empty() {
                vec![session.goal.clone()]
            } else {
                t
            }
        };
        let plan_json = serde_json::to_string(&plan_out.data).unwrap_or_default();
        let mut task_results = Vec::new();
        let mut total_rounds = 0u32;
        let mut checks_log = String::new();
        let mut passed = true;

        for (ti, task) in tasks.iter().enumerate() {
            let mut feedback = String::new();
            // Fresh-read evidence from a refused patch, for the next round.
            let mut refused = String::new();
            let mut artifact = serde_json::Value::Null;
            // Paths git saw change across this task's rounds (§4.4 recall).
            let mut changed_files: Vec<String> = Vec::new();
            // The last round's `git diff --stat`, evidence for the report.
            let mut diff_stat = String::new();
            // §4.3: structured outcomes — the report carries them so `compare`
            // can name the check that flipped for a task that moved, instead of
            // only quoting the losing run's feedback.
            let mut check_results: Vec<crate::engine::session::CheckResult> = Vec::new();
            let mut verdict = Verdict {
                pass: false,
                feedback: "no rounds ran".to_string(),
            };
            let mut ran = 0;
            let mut writes_made = 0usize;
            let limit = session.max_tokens.unwrap_or(self.cfg.max_tokens_per_task);
            let budget = svc.budget(limit);
            let mut budget_hit: Option<u64> = None;
            // Skills this task names, once per task (not per round: a body
            // delivered five times is one reuse, not five).
            let impl_reuse = svc
                .skill_bodies(
                    "implementer",
                    &format!("{} {task}", session.goal),
                    &impl_skills,
                )
                .await;
            let reviewer_reuse = svc
                .skill_bodies(
                    "reviewer",
                    &format!("{} {task}", session.goal),
                    &reviewer_skills,
                )
                .await;
            // Round cap; the auto-poke below may extend it by exactly one
            // which is why this is a `while` and not a range.
            let mut rounds = rounds;
            let mut round = 1u32;
            let mut poked = false;
            while round <= rounds {
                // Token guard: stop before spending another expensive round.
                // O(1) — the sink totals at the emit choke point (§4.3).
                if let Some(spent) = budget.exceeded() {
                    self.trace.emit(TraceEvent::BudgetExceeded {
                        task: task.clone(),
                        tokens: spent,
                        limit,
                    });
                    budget_hit = Some(spent);
                    break;
                }
                ran = round;
                // The baseline this attempt starts from and rolls back to
                // (§4.2), committed before the implementer touches anything.
                if let Err(e) = tree.baseline() {
                    self.trace.emit(TraceEvent::ModelError {
                        agent: "tree".to_string(),
                        error: e.to_string(),
                    });
                    verdict = Verdict {
                        pass: false,
                        feedback: format!("harness: git baseline failed: {e}"),
                    };
                    break;
                }
                let istate = CtxState {
                    long_term: impl_head.clone(),
                    // Stable-first ordering: goal and repo retrieval are
                    // byte-stable across runs, the plan is not, the task text
                    // is per-task. Volatile markers live in short_term so the
                    // cacheable prefix (this block) stays byte-identical, and
                    // an injected skill body goes last for the same reason.
                    mid_term: format!(
                        "goal: {}\n{retrieved}\nplan: {}\nCURRENT TASK ({}/{}) : {task}{impl_reuse}",
                        session.goal,
                        plan_json,
                        ti + 1,
                        tasks.len()
                    ),
                    short_term: if feedback.is_empty() {
                        format!(
                            "WRITES REQUIRED: {}\nround {round}/{rounds}: first attempt",
                            if session.expect_writes { "yes" } else { "no" }
                        )
                    } else {
                        format!(
                            "WRITES REQUIRED: {}\nround {round}/{rounds}: reviewer feedback: {feedback}\nPREVIOUS CHECKS:\n{}{refused}",
                            if session.expect_writes { "yes" } else { "no" },
                            if checks_log.is_empty() {
                                "(none configured)"
                            } else {
                                checks_log.as_str()
                            }
                        )
                    },
                };
                // Per-layer: summarize-before-truncate, one cached cheap-model
                // call per layer that crossed its threshold. No `mut` state to
                // carry between rounds — the builder's cache owns that.
                let (iview, reports) = builder.plan_summarized(&istate, &self.context).await;
                self.fold_layers(&reports, &mut acc);
                let implementer = ImplementerAgent::new(&self.executor);
                match implementer
                    .run(AgentCtx {
                        view: &iview,
                        context: None,
                        executor: Some(&self.executor),
                        tools: Some(tools),
                        workdir: Some(workdir),
                        trace: &self.trace,
                    })
                    .await
                {
                    Ok(o) => artifact = o.data,
                    Err(e) => {
                        self.trace.emit(TraceEvent::ModelError {
                            agent: "implementer".to_string(),
                            error: e.to_string(),
                        });
                        artifact = serde_json::json!({"error": e.to_string()});
                    }
                };
                self.trace.emit(TraceEvent::StateTransition {
                    from: "implemented".to_string(),
                    to: "reviewing".to_string(),
                });

                // Acceptance evidence: run the session's allowlisted checks BEFORE
                // the verdict, so the reviewer judges execution, not prose.
                let task_checks = svc.run_checks(session, workdir).await;
                checks_log = render_checks(&task_checks);
                // The last round's outcomes are the task's: `compare` matches by
                // name, so accumulating earlier rounds would let a round-1
                // failure outrank the round that fixed it.
                check_results = task_checks;
                let verified_files = svc.reviewer_file_evidence(workdir, &artifact).await;
                // Write gate (§4.2): count what git saw change, not what the
                // artifact claims. A new file the model wrote counts; prose-only
                // work reads back as zero; a self-report that disagrees with the
                // tree loses to the tree. The same names feed recall (§4.4).
                let diff = match tree.diff() {
                    Ok(diff) => diff,
                    Err(e) => {
                        self.trace.emit(TraceEvent::ModelError {
                            agent: "tree".to_string(),
                            error: e.to_string(),
                        });
                        verdict = Verdict {
                            pass: false,
                            feedback: format!("harness: git diff failed: {e}"),
                        };
                        break;
                    }
                };
                writes_made = diff.changed();
                diff_stat = diff.stat.clone();
                for name in &diff.names {
                    if !changed_files.iter().any(|seen: &String| seen == name) {
                        changed_files.push(name.clone());
                    }
                }

                let rstate = CtxState {
                    long_term: reviewer_head.clone(),
                    mid_term: format!("PLAN: {plan_json}\nCURRENT TASK: {task}{reviewer_reuse}"),
                    short_term: format!(
                        "ARTIFACT: {}\nSKILL CHANGES: {}\nEXPECT WRITES: {}\nWRITES MADE: {}\nCHANGED (git): {}\nCHECKS:\n{}\nVERIFIED FILES:\n{}",
                        serde_json::to_string(&artifact).unwrap_or_default(),
                        crate::engine::session::skill_changes_line(&artifact),
                        if session.expect_writes { "yes" } else { "no" },
                        writes_made,
                        if diff.names.is_empty() {
                            "(none)".to_string()
                        } else {
                            diff.names.join(", ")
                        },
                        if checks_log.is_empty() {
                            "(none configured)".to_string()
                        } else {
                            checks_log.clone()
                        },
                        if verified_files.is_empty() {
                            "(none touched)".to_string()
                        } else {
                            verified_files.clone()
                        }
                    ),
                };
                // The reviewer is budgeted like everyone else, per layer. Its
                // short layer holds the artifact and the check output — fresh
                // evidence, unarmed for summarization by default (see
                // `policy.rs`), so what it usually needs here is the cut.
                let (rview, rreports) = builder.plan_summarized(&rstate, &self.context).await;
                self.fold_layers(&rreports, &mut acc);
                let reviewer = ReviewerAgent::new(&self.verify);
                match reviewer
                    .run(AgentCtx {
                        view: &rview,
                        context: None,
                        executor: Some(&self.executor),
                        tools: Some(tools),
                        workdir: Some(workdir),
                        trace: &self.trace,
                    })
                    .await
                {
                    Ok(o) => {
                        if let Ok(v) = serde_json::from_value::<Verdict>(o.data.clone()) {
                            verdict = v;
                        } else {
                            verdict = Verdict {
                                pass: false,
                                feedback: "bad verdict shape".to_string(),
                            };
                        }
                    }
                    Err(e) => {
                        self.trace.emit(TraceEvent::ModelError {
                            agent: "reviewer".to_string(),
                            error: e.to_string(),
                        });
                        verdict = Verdict {
                            pass: false,
                            feedback: e.to_string(),
                        };
                    }
                }
                // Harness-side write gate: defense in depth against a
                // pass-happy reviewer. The prompt asks for this, but the
                // harness refuses a pass on empty work regardless.
                if verdict.pass && session.expect_writes && writes_made == 0 {
                    self.trace.emit(TraceEvent::StateTransition {
                        from: "verdict_pass".to_string(),
                        to: "rejected_no_writes".to_string(),
                    });
                    verdict = Verdict {
                        pass: false,
                        feedback: "harness: pass rejected — the task expected a file change but \
                                   no writes were applied"
                            .to_string(),
                    };
                }
                if verdict.pass {
                    break;
                }
                feedback = verdict.feedback.clone();
                self.trace.emit(TraceEvent::StateTransition {
                    from: "reviewing".to_string(),
                    to: "implementing".to_string(),
                });
                // Buy one more round instead of stopping when
                // the failure shape says "instruction problem" (no writes when
                // they were required) or a cap-exhausted retry. Bounded: a
                // task is poked at most once, whatever the cap.
                if !poked
                    && self.cfg.auto_poke
                    && round == rounds
                    && crate::eval::goal_quality::should_auto_poke(
                        verdict.pass,
                        ran,
                        writes_made,
                        session.expect_writes,
                        budget_hit.is_some(),
                    )
                {
                    poked = true;
                    rounds += 1;
                    let reason = crate::eval::goal_quality::poke_reason(
                        ran,
                        writes_made,
                        session.expect_writes,
                    );
                    self.trace.emit(TraceEvent::AutoPoke {
                        task: task.clone(),
                        reason: reason.clone(),
                    });
                    feedback = format!("{feedback}\n{reason}");
                }
                // §4.2: a failed attempt is discarded when a retry follows, so
                // the retry starts from the baseline its snapshot shows. The
                // last round keeps its state in the copy for reading.
                if round < rounds {
                    self.trace.emit(TraceEvent::StateTransition {
                        from: "reviewing".to_string(),
                        to: "rolled_back".to_string(),
                    });
                    if let Err(e) = tree.rollback() {
                        // Best effort: a failed rollback leaves the tree as it
                        // is, the next baseline re-commits it, and the run
                        // continues — the substrate is a means, not a result.
                        self.trace.emit(TraceEvent::ModelError {
                            agent: "tree".to_string(),
                            error: format!("rollback failed (continuing): {e}"),
                        });
                    }
                }
                // The retry's evidence, after the rollback: the tool verdicts,
                // minus any content the rollback reverted (see the function).
                refused = crate::engine::session::file_state_evidence(&artifact);
                round += 1;
            }
            total_rounds += ran;
            let aborted = budget_hit.is_some();
            if aborted {
                verdict = Verdict {
                    pass: false,
                    feedback: format!(
                        "aborted: token budget exceeded ({} > {})",
                        budget_hit.unwrap_or(0),
                        limit
                    ),
                };
            }
            task_results.push(serde_json::json!({
                "task": task,
                "passed": verdict.pass,
                "rounds": ran,
                "writes_made": writes_made,
                "changed_files": changed_files,
                "diff_stat": diff_stat,
                "aborted": aborted,
                "tokens_used": budget.spent(),
                "check_results": check_results,
                "artifact": artifact,
                "feedback": verdict.feedback,
            }));
            if !verdict.pass {
                passed = false;
                break;
            }
        }

        self.trace.emit(TraceEvent::StateTransition {
            from: "reviewing".to_string(),
            to: "done".to_string(),
        });
        serde_json::json!({
            "plan": plan_out.data,
            "tasks": task_results,
            "rounds": total_rounds,
            "passed": passed,
            "retrieved_files": retrieved_files,
            "retrieved": retrieved_json,
            "summarize_calls": acc.summarize_calls,
            "summarize_tokens": acc.summarize_tokens,
            "truncated_views": acc.truncated_views,
            "layer_summaries": acc.layer_summaries,
            "layer_truncations": acc.layer_truncations,
            "checks": checks_log,
            "ctx_tokens": plan_view.used_tokens,
        })
    }

    async fn run_direct_loop(
        &self,
        session: &Session,
        svc: &RoundServices<'_>,
        workdir: &std::path::Path,
        tree: &crate::engine::tree::TreeService,
    ) -> serde_json::Value {
        let builder = ContextBuilder::with_policy(self.cfg.context_policy());
        let retriever = Retriever::new(workdir.to_path_buf(), self.cfg.retrieval.clone());
        let snips = retriever.retrieve(&session.goal, self.cfg.retrieval.max_total_chars);
        let retrieved = render(&snips);
        let retrieved_json: Vec<serde_json::Value> = snips
            .iter()
            .map(|s| serde_json::json!({ "path": s.path, "chars": s.content.chars().count() }))
            .collect();
        let mut acc = LayerAcc::default();
        let mut feedback = String::new();
        let mut checks_log = String::new();
        // §4.3: structured outcomes, same shape as the pipeline loop's — a
        // direct verdict is a field read, not a substring of the log it renders.
        let mut check_results: Vec<crate::engine::session::CheckResult> = Vec::new();
        let mut artifact = serde_json::Value::Null;
        let mut writes_made = 0usize;
        // Paths git saw change across the rounds (§4.4 recall).
        let mut changed_files: Vec<String> = Vec::new();
        // The last round's `git diff --stat`, evidence for the report.
        let mut diff_stat = String::new();
        let mut passed = false;
        // Rounds actually executed (the cap may grow by one on an auto-poke).
        let mut rounds = 0u32;
        let limit = session.max_tokens.unwrap_or(self.cfg.max_tokens_per_task);
        let budget = svc.budget(limit);
        let max_rounds = self.cfg.max_review_rounds.max(1);
        // §4.3: direct mode now takes the same skill index, the same
        // goal-quality note and the same auto-poke as the pipeline loop —
        // all three used to be written into `run_loop` only, so a direct run
        // silently saw no skills, no note and no poke.
        let impl_skills = svc.skill_index("implementer").await;
        let impl_head = RoundServices::head_with_index(&session.ctx.long_term, &impl_skills.text);
        let goal_note = svc.goal_note(&session.goal);
        let mut poked = false;
        let mut round = 1u32;
        let mut cap = max_rounds;
        while round <= cap {
            rounds = round;
            // Token guard: O(1) — the sink totals at the emit choke point.
            if let Some(spent) = budget.exceeded() {
                self.trace.emit(TraceEvent::BudgetExceeded {
                    task: session.goal.clone(),
                    tokens: spent,
                    limit,
                });
                feedback = format!("aborted: token budget exceeded ({spent} > {limit})");
                break;
            }
            // The baseline this attempt starts from and rolls back to (§4.2).
            if let Err(e) = tree.baseline() {
                self.trace.emit(TraceEvent::ModelError {
                    agent: "tree".to_string(),
                    error: e.to_string(),
                });
                feedback = format!("harness: git baseline failed: {e}");
                break;
            }
            let state = CtxState {
                long_term: impl_head.clone(),
                mid_term: format!(
                    "GOAL: {}\n{retrieved}\n{}{}",
                    session.goal,
                    goal_note
                        .as_deref()
                        .map(|n| format!("GOAL QUALITY NOTE: {n}\n"))
                        .unwrap_or_default(),
                    if feedback.is_empty() {
                        String::new()
                    } else {
                        format!("PREVIOUS ATTEMPT:\n{feedback}\n")
                    }
                ),
                short_term: format!(
                    "WRITES REQUIRED: {}\nDIRECT ROUND {round}/{cap}",
                    if session.expect_writes { "yes" } else { "no" }
                ),
            };
            let (view, reports) = builder.plan_summarized(&state, &self.context).await;
            self.fold_layers(&reports, &mut acc);
            let agent = ImplementerAgent::new(&self.executor);
            match agent
                .run_direct(AgentCtx {
                    view: &view,
                    context: None,
                    executor: Some(&self.executor),
                    tools: Some(svc.tools),
                    workdir: Some(workdir),
                    trace: &self.trace,
                })
                .await
            {
                Ok(output) => artifact = output.data,
                Err(e) => {
                    self.trace.emit(TraceEvent::ModelError {
                        agent: "executor".to_string(),
                        error: e.to_string(),
                    });
                    feedback = e.to_string();
                    break;
                }
            }
            let task_checks = svc.run_checks(session, workdir).await;
            checks_log = render_checks(&task_checks);
            check_results = task_checks;
            // Write gate (§4.2): git's change set is the count, not the
            // artifact's self-report; the same names feed recall (§4.4).
            let diff = match tree.diff() {
                Ok(diff) => diff,
                Err(e) => {
                    self.trace.emit(TraceEvent::ModelError {
                        agent: "tree".to_string(),
                        error: e.to_string(),
                    });
                    feedback = format!("harness: git diff failed: {e}");
                    break;
                }
            };
            writes_made = diff.changed();
            diff_stat = diff.stat.clone();
            for name in &diff.names {
                if !changed_files.iter().any(|seen: &String| seen == name) {
                    changed_files.push(name.clone());
                }
            }
            let checks_ok = checks_pass(&check_results);
            passed = (!session.expect_writes || writes_made > 0) && checks_ok;
            if passed {
                break;
            }
            let file_state = crate::engine::session::file_state_evidence(&artifact);
            feedback = if writes_made == 0 && session.expect_writes {
                format!("no file change landed; emit the actual patch or write now{file_state}")
            } else {
                format!(
                    "the configured check failed; fix the change and retry.\nCHECK OUTPUT:\n{}{}",
                    checks_log, file_state
                )
            };
            // §4.3: the same bounded auto-poke the pipeline loop has — a
            // direct run used to stop at the cap even when the failure shape
            // said "instruction problem".
            if !poked
                && self.cfg.auto_poke
                && round == cap
                && crate::eval::goal_quality::should_auto_poke(
                    passed,
                    round,
                    writes_made,
                    session.expect_writes,
                    budget.exceeded().is_some(),
                )
            {
                poked = true;
                cap += 1;
                let reason = crate::eval::goal_quality::poke_reason(
                    round,
                    writes_made,
                    session.expect_writes,
                );
                self.trace.emit(TraceEvent::AutoPoke {
                    task: session.goal.clone(),
                    reason: reason.clone(),
                });
                feedback = format!("{feedback}\n{reason}");
            }
            // §4.2: a failed attempt is discarded when a retry follows; the
            // last round keeps its state in the copy for reading.
            if round < cap {
                self.trace.emit(TraceEvent::StateTransition {
                    from: "direct_executing".to_string(),
                    to: "rolled_back".to_string(),
                });
                if let Err(e) = tree.rollback() {
                    self.trace.emit(TraceEvent::ModelError {
                        agent: "tree".to_string(),
                        error: format!("rollback failed (continuing): {e}"),
                    });
                }
            }
            round += 1;
        }

        self.trace.emit(TraceEvent::StateTransition {
            from: "direct_executing".to_string(),
            to: "done".to_string(),
        });
        serde_json::json!({
            "tasks": [{
                "task": session.goal,
                "passed": passed,
                "rounds": rounds,
                "writes_made": writes_made,
                "changed_files": changed_files,
                "diff_stat": diff_stat,
                "artifact": artifact,
                "check_results": check_results,
                "feedback": feedback,
            }],
            "rounds": rounds,
            "passed": passed,
            "retrieved": retrieved_json,
            "summarize_calls": acc.summarize_calls,
            "summarize_tokens": acc.summarize_tokens,
            "truncated_views": acc.truncated_views,
            "layer_summaries": acc.layer_summaries,
            "layer_truncations": acc.layer_truncations,
            "checks": checks_log,
        })
    }

    /// Test hook: the configured default ceiling.
    pub fn token_limit_for_test(&self) -> u64 {
        self.cfg.max_tokens_per_task
    }

    /// Fold one view's layer reports into the run's context counters, and
    /// trace the cheap-model calls they report. A *cached* summary emits no
    /// `ModelCall` (the round that paid for it already emitted one) but does
    /// count as a delivered summary for its layer — the two reuse paths are
    /// counted apart, the same way `reused`/`viewed` are for skills.
    fn fold_layers(&self, reports: &[LayerReport], acc: &mut LayerAcc) {
        for r in reports {
            let i = r.layer.index();
            if r.truncated {
                acc.truncated_views += 1;
                acc.layer_truncations[i] += 1;
            }
            if r.summarized {
                acc.layer_summaries[i] += 1;
            }
            if r.summarize.call {
                self.trace.emit(TraceEvent::ModelCall {
                    agent: "summarizer".to_string(),
                    model: self.context.model.clone(),
                    input_tokens: r.summarize.input_tokens,
                    output_tokens: r.summarize.output_tokens,
                    latency_ms: r.summarize.latency_ms,
                    cost_usd: r.summarize.cost_usd,
                    cached_input_tokens: r.summarize.cached_input_tokens,
                    attempts: r.summarize.attempts,
                });
                acc.summarize_calls += 1;
                acc.summarize_tokens += r.summarize.input_tokens + r.summarize.output_tokens;
            }
        }
    }
}
/// Context accounting for one run, folded from the `LayerReport`s of every
/// view the round loop built (stage 2). `layer_*` are per `LayerKind::index()`;
/// `truncated_views` is their sum, kept as its own number because it is the one
/// the pre-stage-2 reports already carried.
#[derive(Debug, Default, Clone)]
struct LayerAcc {
    truncated_views: u32,
    layer_truncations: [u32; 3],
    layer_summaries: [u32; 3],
    summarize_calls: u64,
    summarize_tokens: u64,
}
