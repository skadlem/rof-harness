use super::Session;
use crate::agents::{Agent, AgentCtx, ImplementerAgent, PlannerAgent, ReviewerAgent, Verdict};
use crate::config::AppConfig;
use crate::context::{render, ContextBuilder, CtxState, Retriever};
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
        let builder = ContextBuilder::new(self.cfg.budgets.clone());

        // Keyword retrieval over the workdir feeds the mid-term layer,
        // so planner + implementer see relevant files beyond top-level.
        let retriever = Retriever::new(workdir.to_path_buf(), self.cfg.retrieval.clone());
        let snips = retriever.retrieve(&session.goal, self.cfg.retrieval.max_total_chars);
        let retrieved = render(&snips);
        let retrieved_files = snips.len();

        // Planner (Context LLM, no tools)
        let mut state = session.ctx.clone();
        state.mid_term = format!("goal: {}\n{retrieved}", session.goal);
        let plan_view = builder.build(&state);
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
            let mut artifact = serde_json::Value::Null;
            let mut verdict = Verdict {
                pass: false,
                feedback: "no rounds ran".to_string(),
            };
            let mut ran = 0;
            let mut writes_made = 0usize;
            let limit = session.max_tokens.unwrap_or(self.cfg.max_tokens_per_task);
            let task_start = self.trace.len();
            let mut budget_hit: Option<u64> = None;
            let mut condensed: Option<String> = None;
            for round in 1..=rounds {
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
                let mut istate = CtxState {
                    long_term: state.long_term.clone(),
                    // Stable-first ordering: goal and repo retrieval are
                    // byte-stable across runs, the plan is not, the task text
                    // is per-task. Volatile markers live in short_term so the
                    // cacheable prefix (this block) stays byte-identical.
                    mid_term: format!(
                        "goal: {}\n{retrieved}\nplan: {}\nCURRENT TASK ({}/{}) : {task}",
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
                            "WRITES REQUIRED: {}\nround {round}/{rounds}: reviewer feedback: {feedback}\nPREVIOUS CHECKS:\n{}",
                            if session.expect_writes { "yes" } else { "no" },
                            if checks_log.is_empty() {
                                "(none configured)"
                            } else {
                                checks_log.as_str()
                            }
                        )
                    },
                };
                let mut iview = builder.build(&istate);
                // Budget overflow: compress once per task with the cheap model
                // instead of letting the builder chop context blindly.
                if iview.truncated {
                    if let Some(c) = &condensed {
                        istate.short_term = c.clone();
                        iview = builder.build(&istate);
                    } else if let Ok(r) = self
                        .context
                        .summarize(&iview.prompt, self.cfg.budgets.short_term)
                        .await
                    {
                        self.trace.emit(TraceEvent::ModelCall {
                            agent: "summarizer".to_string(),
                            model: self.context.model.clone(),
                            input_tokens: r.input_tokens,
                            output_tokens: r.output_tokens,
                            latency_ms: r.latency_ms,
                            cost_usd: r.cost_usd,
                            cached_input_tokens: r.cached_input_tokens,
                            attempts: r.attempts,
                        });
                        condensed = Some(r.text.clone());
                        istate.short_term = r.text;
                        iview = builder.build(&istate);
                    }
                }
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
                // Count real writes: prose-only artifacts must not pass a
                // task that was supposed to change the tree.
                writes_made = artifact
                    .get("writes")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter(|w| {
                                !w.as_str().map(|s| s.starts_with("FAILED")).unwrap_or(false)
                            })
                            .count()
                    })
                    .unwrap_or(0);

                let rstate = CtxState {
                    long_term: state.long_term.clone(),
                    mid_term: format!("PLAN: {plan_json}\nCURRENT TASK: {task}"),
                    short_term: format!(
                        "ARTIFACT: {}\nEXPECT WRITES: {}\nWRITES MADE: {}\nCHECKS:\n{}",
                        serde_json::to_string(&artifact).unwrap_or_default(),
                        if session.expect_writes { "yes" } else { "no" },
                        writes_made,
                        if checks_log.is_empty() {
                            "(none configured)".to_string()
                        } else {
                            checks_log.clone()
                        }
                    ),
                };
                let rview = builder.build(&rstate);
                let reviewer = ReviewerAgent::new(&self.executor);
                match reviewer
                    .run(AgentCtx {
                        view: &rview,
                        context: None,
                        executor: Some(&self.executor),
                        tools: None,
                        workdir: None,
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
            "checks": checks_log,
            "ctx_tokens": plan_view.used_tokens,
        })
    }

    /// Test hook: the configured default ceiling.
    pub fn token_limit_for_test(&self) -> u64 {
        self.cfg.max_tokens_per_task
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
