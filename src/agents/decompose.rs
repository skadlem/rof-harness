//! The meta layer, v1: SEQUENTIAL decomposition with a durable plan
//! artifact (design report §4, §9 item 4).
//!
//! The consumer already existed and is unchanged: the orchestrator reads a
//! `Vec<String>` and runs each task through its own bounded Implementer ->
//! Reviewer rounds, stopping at the first failure. What was missing was
//! producing a list for a request that is not one task — the planner that
//! used to do it was deleted in 2e78912 because it cost tokens on goals that
//! never needed it. So this is not a planner that always runs: it is ONE
//! bounded call behind a cheap local gate, and the plan it produces lives in
//! a FILE rather than in context.
//!
//! **Sequential, not fan-out.** One task at a time, no concurrent workers,
//! no subagent spawning. That is the design report's own reading of the
//! evidence: "most coding tasks involve fewer truly parallelizable tasks than
//! research, and LLM agents are not yet great at coordinating and delegating
//! to other agents in real time", and token usage alone explains ~80% of
//! performance variance, so fan-out buys quality with spend. Nothing in this
//! module spawns anything; it returns strings.
//!
//! Two properties the rest of the harness leans on:
//!
//! * **Bounded.** [`DECOMPOSER_MAX_TASKS`] caps the list and
//!   [`DECOMPOSER_MAX_TASK_CHARS`] caps each entry, because a sequential
//!   loop over a model-authored list is the one place an unbounded answer
//!   turns into unbounded work. The cap is a harness decision, not a request
//!   made in the prompt.
//! * **Degrading, never failing.** A failed call, an empty list, prose, or a
//!   list of non-strings all return `Err`, and the caller falls back to the
//!   goal as a single task — exactly what the empty-plan path has always
//!   done. A run must never get worse than it was before this existed.

use crate::llm::LlmReq;
use crate::obs::{TraceEvent, TraceSink};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Ceiling on how many tasks one decomposition may produce. The consumer
/// runs every task through its own bounded rounds, so this is a bound on
/// total work, not on prompt size.
pub const DECOMPOSER_MAX_TASKS: usize = 8;

/// Ceiling on one task's text. A "task" longer than this is a paragraph the
/// model mistook for a step.
pub const DECOMPOSER_MAX_TASK_CHARS: usize = 400;

/// Directory the plan artifact is written under, relative to the work root.
///
/// Under the work root because that is the only root a run is guaranteed to
/// own: it is what `ROF_WORKDIR` points at, what the task copy is made from,
/// and what the trace's other evidence already lives beside. The name is
/// fixed and the file inside it is named by the run id, so the path is
/// built from the root plus two harness-chosen components and NEVER from
/// goal text — model-authored text cannot traverse out of the work root
/// because it never reaches the path.
const PLAN_DIR: &str = ".rof/plan";

/// Goal text -> the task list that bought. Process-lifetime, keyed by the
/// goal text itself.
///
/// Why the goal and nothing else: the answer depends on the request, not on
/// which run asked, so two runs of one goal must not be billed twice. Why
/// never dropped: a decomposition is a pure function of the goal, so a
/// cached entry cannot go stale within a process, and the alternative — a
/// cache per run — is exactly the per-run billing this exists to remove.
/// It dies with the process; nothing is persisted, so a fresh process pays
/// again and there is no on-disk cache to invalidate or to leak a goal into.
/// One cached decomposition: the goal text, and the task list it bought.
type CachedPlan = (String, Arc<Vec<String>>);

/// The process-wide cache, so a goal is decomposed at most once per process.
type PlanCache = Mutex<Vec<CachedPlan>>;

fn plan_cache() -> &'static PlanCache {
    static CACHE: OnceLock<PlanCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// The number of goals this process has decomposed, for the test that
/// proves a second run of one goal buys nothing.
pub fn cached_plan_count() -> usize {
    plan_cache().lock().map(|c| c.len()).unwrap_or(0)
}

/// One bounded model call that turns a goal into an ordered task list.
pub struct DecomposerAgent<'a> {
    llm: &'a crate::llm::ExecutorService,
}

/// What one decomposition produced, and how the run should record it.
pub struct Plan {
    /// The task list, in order. Empty means "degrade to the goal".
    pub tasks: Vec<String>,
    /// Where the list was written, for this run.
    pub path: String,
    /// True when the list came from the cache rather than a call.
    pub cached: bool,
    /// The record's reason line.
    pub reason: String,
}

impl<'a> DecomposerAgent<'a> {
    pub fn new(llm: &'a crate::llm::ExecutorService) -> Self {
        Self { llm }
    }

    /// One task's text, cut at [`DECOMPOSER_MAX_TASK_CHARS`]. Cut on a char
    /// boundary, so a task in any language is never cut mid-codepoint.
    fn bound_task_text(t: &str) -> String {
        if t.chars().count() > DECOMPOSER_MAX_TASK_CHARS {
            t.chars().take(DECOMPOSER_MAX_TASK_CHARS).collect()
        } else {
            t.to_string()
        }
    }

    /// The task list a model response carries, or `Err` naming why there is
    /// none. Every rejection is a degradation the caller survives, never a
    /// run failure — the distinction the empty-plan path has always made.
    pub fn parse_tasks(value: &serde_json::Value) -> Result<Vec<String>, String> {
        let Some(raw) = value.get("tasks").and_then(|t| t.as_array()) else {
            return Err("decomposition response carried no `tasks` array".to_string());
        };
        if raw.is_empty() {
            return Err("decomposition returned an empty task list".to_string());
        }
        // Non-string entries are not tasks. Dropping them and using the rest
        // would be kinder than failing, but a list of numbers is a model
        // that did not follow the contract at all, and the fallback is the
        // honest reading of that.
        if !raw.iter().all(|v| v.is_string()) {
            return Err("decomposition returned non-string task entries".to_string());
        }
        let tasks: Vec<String> = raw
            .iter()
            .map(|v| v.as_str().unwrap_or_default().trim().to_string())
            // An empty string is not a task; a list of them is a model that
            // returned the envelope without the content.
            .filter(|t| !t.is_empty())
            .map(|t| Self::bound_task_text(&t))
            .take(DECOMPOSER_MAX_TASKS)
            .collect();
        if tasks.is_empty() {
            return Err("decomposition returned no usable task text".to_string());
        }
        Ok(tasks)
    }

    /// Write the plan where the run's trace can point at it.
    ///
    /// Per RUN, not per goal: two runs of one goal each get their own file,
    /// so the first run's trace never names a file the second run rewrote.
    /// The filename is the run id, which the harness generates; goal text
    /// contributes the file's CONTENT and nothing about its location.
    fn write_artifact(
        workdir: &Path,
        run_id: &str,
        goal: &str,
        tasks: &[String],
    ) -> Result<PathBuf, String> {
        let dir = workdir.join(PLAN_DIR);
        std::fs::create_dir_all(&dir).map_err(|e| format!("plan dir: {e}"))?;
        // A run id that could traverse is not one the harness generated;
        // refuse rather than write outside the root.
        let safe_id: String = run_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if safe_id.is_empty() {
            return Err("run id has no usable characters for an artifact name".to_string());
        }
        let path = dir.join(format!("{safe_id}.json"));
        let body = serde_json::json!({
            "goal": goal,
            "tasks": tasks,
            "task_shaped": crate::eval::goal_quality::goal_is_task_shaped(goal),
        });
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&body).unwrap_or_default(),
        )
        .map_err(|e| format!("plan write: {e}"))?;
        Ok(path)
    }

    /// Buy the plan for `goal`, or say why there isn't one.
    ///
    /// The caller has already consulted the gate; this is only reached when
    /// it fired. Cached answers return without a call, which is the whole
    /// point of the cache and is asserted by counting calls on a fake model.
    pub async fn plan(
        &self,
        goal: &str,
        workdir: &Path,
        run_id: &str,
        trace: &TraceSink,
    ) -> Result<Plan, String> {
        if let Ok(cache) = plan_cache().lock() {
            if let Some((_, tasks)) = cache.iter().find(|(g, _)| g == goal) {
                let tasks = tasks.as_ref().clone();
                drop(cache);
                // The artifact is still written: this run's trace points at
                // this run's file, whether the list was paid for or reused.
                let path = match Self::write_artifact(workdir, run_id, goal, &tasks) {
                    Ok(p) => p.to_string_lossy().into_owned(),
                    Err(e) => {
                        trace.emit(TraceEvent::ModelError {
                            agent: "decomposer".to_string(),
                            error: format!("plan artifact not written (cached plan): {e}"),
                        });
                        String::new()
                    }
                };
                return Ok(Plan {
                    tasks,
                    path,
                    cached: true,
                    reason: "reused the cached decomposition for this goal".to_string(),
                });
            }
        }

        // The one bounded call. The prompt is the goal and the instruction
        // and nothing else: no environment value, no config, no credentials
        // are interpolated into what the model is asked, so nothing can be
        // smuggled in through this call.
        let system = "You decompose a request into a short ordered list of tasks. \
             Reply with JSON only: {\"tasks\": [\"<task>\", ...]}. Rules: (1) each task must be a \
             single concrete action naming the file or symbol it concerns; (2) if the request is \
             already one task, return exactly one task that restates it; (3) no commentary, no \
             markdown fence, no prose outside the JSON.";
        let prompt = format!("REQUEST:\n{goal}\n\nTASKS (JSON only):");
        crate::context::measure_turn(trace, "decomposer", crate::context::TURN_CALL, &prompt);
        let req = LlmReq {
            system: system.to_string(),
            prompt,
            // Bounded: a task list is a few hundred tokens, and the cap is
            // what stops a runaway completion from being paid for in full.
            max_tokens: super::max_tokens_from_env("ROF_DECOMPOSER_MAX_TOKENS", 1024),
            reasoning_off: false,
            reasoning_low: false,
            roomier: false,
            shrunk: false,
            thinking_off: true,
        };
        let resp = self
            .llm
            .complete(req)
            .await
            .map_err(|e| format!("decomposition call failed: {e}"))?;
        trace.emit(TraceEvent::ModelCall {
            agent: "decomposer".to_string(),
            model: self.llm.model.clone(),
            input_tokens: resp.input_tokens,
            output_tokens: resp.output_tokens,
            latency_ms: resp.latency_ms,
            cost_usd: resp.cost_usd,
            cached_input_tokens: resp.cached_input_tokens,
            attempts: resp.attempts,
        });
        let value = crate::llm::parse_lenient(&resp.text)
            .ok_or_else(|| "decomposition response was not JSON".to_string())?;
        let tasks = Self::parse_tasks(&value)?;
        if let Ok(mut cache) = plan_cache().lock() {
            cache.push((goal.to_string(), Arc::new(tasks.clone())));
        }
        let path = Self::write_artifact(workdir, run_id, goal, &tasks)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Plan {
            tasks,
            path,
            cached: false,
            reason: "one bounded decomposition call".to_string(),
        })
    }
}
