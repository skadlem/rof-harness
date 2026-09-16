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
}

impl Orchestrator {
    pub fn new(
        cfg: AppConfig,
        trace: Arc<TraceSink>,
        context: ContextService,
        executor: ExecutorService,
    ) -> Self {
        Self {
            cfg,
            trace,
            context,
            executor,
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
        if self.cfg.execution == "direct" {
            return self.run_direct_loop(session, tools, workdir, &tree).await;
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
        let planner_skills = self.skill_index(tools, "planner").await;
        let impl_skills = self.skill_index(tools, "implementer").await;
        let reviewer_skills = self.skill_index(tools, "reviewer").await;
        let planner_head = head_with_index(&session.ctx.long_term, &planner_skills.text);
        let impl_head = head_with_index(&session.ctx.long_term, &impl_skills.text);
        let reviewer_head = head_with_index(&session.ctx.long_term, &reviewer_skills.text);
        // The planner only gets bodies when it actually runs: with the planner
        // skipped there is no planner prompt to put them in, and a `reused`
        // count that includes a body nobody read would be a lie.
        let planner_reuse = if self.cfg.planner == "skip" {
            String::new()
        } else {
            self.skill_bodies(tools, "planner", &session.goal, &planner_skills)
                .await
        };

        // A pre-check cheaper than the model that will consume
        // the goal. It never blocks — it emits a trace event and (when
        // enabled) a note in the planner prompt, because a pre-check that
        // refuses goals would be the harness claiming a judgement it cannot
        // make. Off by default (`AppConfig::goal_quality`).
        let goal_note = if self.cfg.goal_quality {
            match crate::eval::goal_quality::check_goal_quality(&session.goal) {
                Some(note) => {
                    self.trace.emit(TraceEvent::GoalQuality {
                        goal: session.goal.clone(),
                        note: note.clone(),
                    });
                    Some(note)
                }
                None => None,
            }
        } else {
            None
        };

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
            let mut verdict = Verdict {
                pass: false,
                feedback: "no rounds ran".to_string(),
            };
            let mut ran = 0;
            let mut writes_made = 0usize;
            let limit = session.max_tokens.unwrap_or(self.cfg.max_tokens_per_task);
            let task_start = self.trace.len();
            let mut budget_hit: Option<u64> = None;
            // Skills this task names, once per task (not per round: a body
            // delivered five times is one reuse, not five).
            let impl_reuse = self
                .skill_bodies(
                    tools,
                    "implementer",
                    &format!("{} {task}", session.goal),
                    &impl_skills,
                )
                .await;
            let reviewer_reuse = self
                .skill_bodies(
                    tools,
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
                let spent = self.tokens_since(task_start);
                if limit > 0 && spent > limit {
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
                checks_log = self.run_checks(session, tools, workdir).await;
                let verified_files = self.reviewer_file_evidence(tools, workdir, &artifact).await;
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
                        skill_changes_line(&artifact),
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
                let reviewer = ReviewerAgent::new(&self.executor);
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
                refused = file_state_evidence(&artifact);
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
                "tokens_used": self.tokens_since(task_start),
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
        tools: &ToolRegistry,
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
        let mut artifact = serde_json::Value::Null;
        let mut writes_made = 0usize;
        // Paths git saw change across the rounds (§4.4 recall).
        let mut changed_files: Vec<String> = Vec::new();
        // The last round's `git diff --stat`, evidence for the report.
        let mut diff_stat = String::new();
        let mut passed = false;
        let mut rounds = 0u32;
        let start = self.trace.len();
        let limit = session.max_tokens.unwrap_or(self.cfg.max_tokens_per_task);
        let max_rounds = self.cfg.max_review_rounds.max(1);

        for round in 1..=max_rounds {
            let spent = self.tokens_since(start);
            if limit > 0 && spent > limit {
                self.trace.emit(TraceEvent::BudgetExceeded {
                    task: session.goal.clone(),
                    tokens: spent,
                    limit,
                });
                feedback = format!("aborted: token budget exceeded ({spent} > {limit})");
                break;
            }
            rounds = round;
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
                long_term: session.ctx.long_term.clone(),
                mid_term: format!(
                    "GOAL: {}\n{retrieved}\n{}",
                    session.goal,
                    if feedback.is_empty() {
                        String::new()
                    } else {
                        format!("PREVIOUS ATTEMPT:\n{feedback}\n")
                    }
                ),
                short_term: format!(
                    "WRITES REQUIRED: {}\nDIRECT ROUND {round}/{max_rounds}",
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
                    tools: Some(tools),
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
            checks_log = self.run_checks(session, tools, workdir).await;
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
            let checks_ok = !checks_log.contains("STATUS: FAILED");
            passed = (!session.expect_writes || writes_made > 0) && checks_ok;
            if passed {
                break;
            }
            let file_state = file_state_evidence(&artifact);
            feedback = if writes_made == 0 && session.expect_writes {
                format!("no file change landed; emit the actual patch or write now{file_state}")
            } else {
                format!(
                    "the configured check failed; fix the change and retry.\nCHECK OUTPUT:\n{}{}",
                    checks_log, file_state
                )
            };
            // §4.2: a failed attempt is discarded when a retry follows; the
            // last round keeps its state in the copy for reading.
            if round < max_rounds {
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

    /// The skill index as `agent` may see it. Empty when the grant does not
    /// cover `skills.list`, when the store is empty, or when the tool fails —
    /// an agent that may not list skills simply gets no `[SKILLS]` block.
    /// Emits `SkillOp{op: "list"}` only when there was something to deliver.
    async fn skill_index(&self, tools: &ToolRegistry, agent: &str) -> SkillIndex {
        let (r, _) = tools
            .call(agent, "skills.list", None, serde_json::json!({}))
            .await;
        let Ok(o) = r else {
            return SkillIndex::default();
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&o.output) else {
            return SkillIndex::default();
        };
        let text = v
            .get("index")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string();
        let names: Vec<String> = v
            .get("skills")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| Some(s.get("name")?.as_str()?.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if !text.trim().is_empty() {
            self.trace.emit(TraceEvent::SkillOp {
                agent: agent.to_string(),
                op: "list".to_string(),
                name: String::new(),
                ok: true,
                bytes: text.len() as u64,
            });
        }
        SkillIndex { text, names }
    }

    /// Bodies of the skills `text` names, as `agent`, through the gate. This is
    /// the reuse path: the task says what it wants by name and the recorded
    /// procedure arrives, the same way the retriever hands over a named file.
    async fn skill_bodies(
        &self,
        tools: &ToolRegistry,
        agent: &str,
        text: &str,
        index: &SkillIndex,
    ) -> String {
        // Two is already a lot of procedure for one task; more is a prompt
        // stuffed with instructions nobody asked for.
        const MAX_BODIES: usize = 2;
        let mut out = String::new();
        let mut taken = 0usize;
        for name in &index.names {
            if taken >= MAX_BODIES {
                break;
            }
            if !crate::skills::mentions(text, name) {
                continue;
            }
            let (r, _) = tools
                .call(
                    agent,
                    "skills.view",
                    None,
                    serde_json::json!({ "name": name }),
                )
                .await;
            let body = match r {
                Ok(o) => serde_json::from_str::<serde_json::Value>(&o.output)
                    .ok()
                    .and_then(|v| v.get("body").and_then(|b| b.as_str()).map(str::to_string)),
                Err(_) => None,
            };
            let Some(body) = body else {
                continue;
            };
            taken += 1;
            self.trace.emit(TraceEvent::SkillOp {
                agent: agent.to_string(),
                op: "reuse".to_string(),
                name: name.clone(),
                ok: true,
                bytes: body.len() as u64,
            });
            out.push_str(&format!(
                "\nSKILL {name} (recorded procedure — follow it):\n{}\n",
                body.trim()
            ));
        }
        out
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

    /// Sum of input+output tokens reported by model calls emitted at or
    /// after `from` (per-task spend, read back from the trace stream).
    fn tokens_since(&self, from: usize) -> u64 {
        self.trace
            .events()
            .into_iter()
            .skip(from)
            .filter_map(|e| match e {
                TraceEvent::ModelCall {
                    input_tokens,
                    output_tokens,
                    ..
                } => Some(input_tokens + output_tokens),
                _ => None,
            })
            .sum()
    }

    /// Runs the session's allowlisted checks through the tool gate and
    /// returns the combined log (empty when none configured).
    async fn run_checks(
        &self,
        session: &Session,
        tools: &ToolRegistry,
        workdir: &std::path::Path,
    ) -> String {
        let mut log = String::new();
        for cmd in &session.checks {
            let (r, lat) = tools
                .call(
                    "reviewer",
                    "proc.run",
                    Some(workdir),
                    serde_json::json!({ "cmd": cmd }),
                )
                .await;

            self.trace.emit(TraceEvent::ToolCall {
                agent: "reviewer".to_string(),
                tool: "proc.run".to_string(),
                ok: r.is_ok(),
                latency_ms: lat,
            });
            log.push_str(&match r {
                Ok(o) => {
                    // State the outcome explicitly. A passing build prints only
                    // progress lines, which condensation drops — the reviewer
                    // must never have to infer "passed" from an empty body.
                    let code = o.error.clone().unwrap_or_else(|| "exit 0".to_string());
                    format!(
                        "$ {cmd}\nSTATUS: {} ({code})\n{}\n",
                        if o.ok { "PASSED" } else { "FAILED" },
                        condense_output(&o.output)
                    )
                }
                Err(e) => format!("$ {cmd}\nSTATUS: FAILED ({e})\n"),
            });
        }
        log
    }

    /// Read touched files through the reviewer's grant after the implementer
    /// has applied its artifact. This is independent evidence: the reviewer
    /// should judge the tree, not only the implementer's self-reported state.
    async fn reviewer_file_evidence(
        &self,
        tools: &ToolRegistry,
        workdir: &std::path::Path,
        artifact: &serde_json::Value,
    ) -> String {
        let mut paths = Vec::new();
        if let Some(entries) = artifact.get("file_state").and_then(|v| v.as_array()) {
            for entry in entries {
                let Some(path) = entry.get("path").and_then(|v| v.as_str()) else {
                    continue;
                };
                if !paths.iter().any(|seen| seen == path) {
                    paths.push(path.to_string());
                }
            }
        }

        let mut out = String::new();
        for path in paths.into_iter().take(5) {
            let target = workdir.join(&path);
            let (result, latency_ms) = tools
                .call(
                    "reviewer",
                    "fs.read",
                    Some(&target),
                    serde_json::json!({"path": path, "max_bytes": 262_144}),
                )
                .await;
            self.trace.emit(TraceEvent::ToolCall {
                agent: "reviewer".to_string(),
                tool: "fs.read".to_string(),
                ok: result.is_ok(),
                latency_ms,
            });
            match result {
                Ok(read) => out.push_str(&format!("--- {path}\n{}\n", read.output)),
                Err(error) => out.push_str(&format!("--- {path}\n(unreadable: {error})\n")),
            }
        }
        out
    }
}

/// What a model may see about skills: the rendered index and the names, so a
/// task's text can be tested against them without re-reading the store.
#[derive(Debug, Default, Clone)]
struct SkillIndex {
    text: String,
    names: Vec<String>,
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

/// The stable head a prompt gets: the session's conventions, plus the skill
/// index when there is one. Byte-stable per task, which is what keeps it in the
/// provider's cached prefix.
fn head_with_index(base: &str, index: &str) -> String {
    if index.trim().is_empty() {
        return base.to_string();
    }
    format!("{base}\n[SKILLS]\n{index}")
}

/// One line for the reviewer: what the implementer did to the skill store.
/// Empty work in the skills channel must be as legible as a zero WRITES MADE.
fn skill_changes_line(artifact: &serde_json::Value) -> String {
    let entries = artifact
        .get("skill_changes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if entries.is_empty() {
        return "none".to_string();
    }
    let parts: Vec<String> = entries
        .iter()
        .map(|e| {
            let outcome = e.get("outcome").and_then(|v| v.as_str()).unwrap_or("?");
            let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let detail = e.get("detail").and_then(|v| v.as_str()).unwrap_or("");
            format!("{outcome} {name} ({detail})")
        })
        .collect();
    format!("{} - {}", entries.len(), parts.join("; "))
}

/// What the next round needs after a failed attempt: the tool verdicts, plus
/// for a refused patch the file's text. A change that *landed* is reported
/// without its text — §4.2 rolled the tree back to the baseline, so the
/// post-attempt read describes a state the tree no longer has, and a retry that
/// trusted it would skip a change that is gone. Two failures this addresses,
/// both measured: "search string not found" with no file in front of the model
/// makes it guess again, and a retry told an applied edit is in place neither
/// re-applies it nor re-reads the file (duplicate definitions, E0592/E0428).
fn file_state_evidence(artifact: &serde_json::Value) -> String {
    // ponytail: one shared budget, first file takes it; split it per file when
    // a run shows two touched files that both need their text.
    const CAP: usize = 12_000;
    let mut out = String::new();
    let entries = artifact
        .get("file_state")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    // One entry per path, the LAST one: a file patched twice in an artifact has
    // two reads, and the earlier one describes a state the model already moved
    // past — keeping it would hand the retry a stale file and a fresh-looking
    // label (measured: the duplicate-definition class returning at 3 rounds).
    let mut last: Vec<serde_json::Value> = Vec::new();
    for e in entries {
        let path = e.get("path").and_then(|v| v.as_str()).unwrap_or("?");
        if let Some(slot) = last
            .iter_mut()
            .find(|k| k.get("path").and_then(|v| v.as_str()) == Some(path))
        {
            *slot = e;
        } else {
            last.push(e);
        }
    }
    for e in last {
        let left = CAP.saturating_sub(out.len());
        if left == 0 {
            break;
        }
        let path = e.get("path").and_then(|v| v.as_str()).unwrap_or("?");
        let why = e.get("why").and_then(|v| v.as_str()).unwrap_or("changed");
        if why == "applied" || why == "rewritten" {
            // The text is deliberately not included: the harness restored the
            // baseline, so this content is not on disk. `retrieved:` and a
            // fresh read show the file as it is.
            out.push_str(&format!(
                "\nFILE {path} ({why} by your previous round, then ROLLED BACK by the harness: \
                 the tree is back to its baseline and the change is NOT in it now — re-read \
                 the file and re-apply the change if the task still needs it).\n"
            ));
        } else {
            let text = e
                .get("current_content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // A refused patch changed no file, so its read survived the
            // rollback and is still the tree's content.
            let win: String = text.chars().take(left).collect();
            out.push_str(&format!(
                "\nPATCH REFUSED for {path}: {why}\n--- {path} (current content, read back) ---\n{win}\n"
            ));
        }
    }
    out
}

/// Keeps the lines a reviewer can act on and drops build noise. Raw
/// `cargo test` output is mostly "test x ... ok" lines that the reviewer
/// never uses but which every retry pays for (~2.4k tokens measured).
fn condense_output(raw: &str) -> String {
    const KEEP: [&str; 9] = [
        "FAILED",
        "error",
        "panicked",
        "assertion",
        "warning",
        "test result",
        "failures:",
        "left:",
        "right:",
    ];
    let mut kept: Vec<&str> = Vec::new();
    let mut dropped = 0usize;
    for line in raw.lines() {
        let t = line.trim();
        let noise = t.is_empty()
            || t.starts_with("Compiling")
            || t.starts_with("Finished")
            || t.starts_with("Running")
            || t.starts_with("Doc-tests")
            || t.starts_with("Blocking")
            || (t.starts_with("test ") && t.ends_with("... ok"));
        if noise {
            dropped += 1;
            continue;
        }
        if KEEP.iter().any(|k| t.contains(k)) {
            kept.push(line);
        } else {
            dropped += 1;
        }
    }
    if kept.is_empty() {
        // Silence must be legible: an empty body means the command printed
        // nothing actionable, not that the check failed to run.
        return format!(
            "(no actionable lines in {} lines of output)",
            raw.lines().count()
        );
    }
    let omitted = kept.len().saturating_sub(80);
    let body = kept[..kept.len().min(80)].join("\n");
    if omitted > 0 || dropped > 0 {
        format!("{body}\n[...{dropped} noise lines and {omitted} kept-lines over cap omitted]")
    } else {
        body
    }
}

#[cfg(test)]
mod condense_tests {
    use super::condense_output;

    #[test]
    fn keeps_failures_drops_noise() {
        let raw = "Compiling rof v0.1.0\nFinished test profile\nRunning tests/a.rs\ntest a ... ok\ntest b ... FAILED\nassertion `left == right` failed\nleft: 1\nright: 2\ntest result: FAILED. 1 passed; 1 failed\n";
        let out = condense_output(raw);
        assert!(out.contains("test b ... FAILED"));
        assert!(out.contains("left: 1"));
        assert!(out.contains("test result: FAILED"));
        assert!(!out.contains("test a ... ok"));
        assert!(!out.contains("Compiling"));
        assert!(out.len() < raw.len());
    }

    #[test]
    fn green_run_collapses_to_summary() {
        let raw = "test a ... ok\ntest b ... ok\ntest result: ok. 2 passed\n";
        let out = condense_output(raw);
        assert!(out.contains("test result: ok"));
        assert!(!out.contains("test a ... ok"));
    }

    #[test]
    fn empty_body_says_so() {
        // A green `cargo check` prints only progress lines: the condensed
        // body must read as "nothing to report", not as a blank.
        let out = condense_output("   Compiling rof v0.1.0\n    Finished dev profile\n");
        assert!(out.contains("no actionable lines"), "{out}");
    }
}

#[cfg(test)]
mod file_state_tests {
    use super::file_state_evidence;

    #[test]
    fn the_last_read_of_a_path_wins() {
        // Two patches to one file: the retry must get the second (current)
        // read, not the first. Here the second is a refusal, so its text is
        // the one the retry sees — the stale applied read is dropped, and
        // with §4.2 an applied read carries no text at all (its change was
        // rolled back).
        let artifact = serde_json::json!({
            "file_state": [
                {"path": "src/a.rs", "why": "applied", "current_content": "OLD"},
                {"path": "src/a.rs", "why": "search string not found", "current_content": "NEW"},
            ]
        });
        let out = file_state_evidence(&artifact);
        assert!(out.contains("NEW"), "{out}");
        assert!(!out.contains("OLD"), "{out}");
        assert!(out.contains("PATCH REFUSED for src/a.rs"), "{out}");
    }

    #[test]
    fn an_applied_change_is_reported_as_rolled_back_without_its_text() {
        // §4.2: the tree went back to the baseline, so the post-attempt text
        // is not on disk and must not be handed to the retry as if it were.
        let artifact = serde_json::json!({
            "file_state": [{"path": "src/a.rs", "why": "applied", "current_content": "LANDED"}]
        });
        let out = file_state_evidence(&artifact);
        assert!(out.contains("ROLLED BACK"), "{out}");
        assert!(!out.contains("LANDED"), "{out}");
    }

    #[test]
    fn separate_files_each_keep_their_evidence() {
        // Applied files get the rollback line; a refused patch still hands over
        // its text — the attempt changed nothing there, so the read survived.
        let artifact = serde_json::json!({
            "file_state": [
                {"path": "src/a.rs", "why": "applied", "current_content": "AAA"},
                {"path": "src/b.rs", "why": "search string not found", "current_content": "BBB"},
            ]
        });
        let out = file_state_evidence(&artifact);
        assert!(out.contains("ROLLED BACK"));
        assert!(!out.contains("AAA"), "applied text must not survive: {out}");
        assert!(out.contains("BBB"));
        assert!(out.contains("PATCH REFUSED for src/b.rs"));
    }
}
