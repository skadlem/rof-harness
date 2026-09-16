use super::metrics::EvalReport;
use super::suite::{EvalSuite, EvalTask};
use crate::config::AppConfig;
use crate::engine::{Orchestrator, Session};
use crate::llm::{ContextService, ExecutorService};
use crate::obs::TraceSink;
use crate::tools::ToolRegistry;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub name: String,
    pub passed: bool,
    pub expected: bool,
    pub matched: bool,
    pub rounds: u32,
    pub feedback: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SuiteReport {
    #[serde(default)]
    pub tasks: Vec<TaskResult>,
    pub aggregate: EvalReport,
}

impl SuiteReport {
    pub fn matched(&self) -> usize {
        self.tasks.iter().filter(|t| t.matched).count()
    }
    pub fn success_rate(&self) -> f64 {
        if self.tasks.is_empty() {
            0.0
        } else {
            self.matched() as f64 / self.tasks.len() as f64
        }
    }
}

/// Meta-harness: runs each suite task through the full
/// Planner -> Implementer -> Reviewer loop with an isolated trace sink,
/// then folds everything into one aggregate report. Per-task pass/fail
/// is recorded explicitly, so multi-round retries don't inflate stats.
///
/// Isolation: every task runs against its own scratch copy of the
/// workdir (build artifacts excluded), so parallel writers never share
/// a tree and sequential tasks cannot see each other's edits.
#[derive(Clone)]
pub struct EvaluationRunner {
    trace: Arc<TraceSink>,
    cfg: AppConfig,
    context: ContextService,
    executor: ExecutorService,
    workdir: PathBuf,
}

impl EvaluationRunner {
    pub fn new(
        trace: Arc<TraceSink>,
        cfg: AppConfig,
        context: ContextService,
        executor: ExecutorService,
        workdir: PathBuf,
    ) -> Self {
        Self {
            trace,
            cfg,
            context,
            executor,
            workdir,
        }
    }

    pub fn report(&self) -> EvalReport {
        let mut r = EvalReport {
            pricing: self.cfg.pricing.clone(),
            cost_lambda: self.cfg.cost_lambda,
            ..Default::default()
        };
        for ev in self.trace.events() {
            r.fold(&ev);
        }
        r
    }

    /// Copy the workdir for one task, skipping build output and VCS state.
    /// The copy keeps fixtures and manifests, so checks behave identically.
    fn prepare_task_dir(&self, task_name: &str) -> anyhow::Result<PathBuf> {
        let base = match &self.cfg.task_root {
            // A suite whose checks run a build system writes a target/ per
            // task; the system temp dir is often a small tmpfs, so the root
            // is configurable and points at disk for real runs.
            Some(r) => {
                std::fs::create_dir_all(r)?;
                r.clone()
            }
            None => std::env::temp_dir(),
        };
        let dest = base.join(format!(
            "rof-task-{}-{}-{}",
            std::process::id(),
            sanitize(task_name),
            uuid::Uuid::new_v4().simple()
        ));
        copy_tree(&self.workdir, &dest)?;
        Ok(dest)
    }

    /// One task, in the caller's tree. Public only for tests that hand over a
    /// tree they built themselves; the suite always goes through
    /// `run_suite`, which copies first.
    pub async fn run_task_in(&self, task: &EvalTask, workdir: PathBuf) -> TaskResult {
        let sink = Arc::new(self.trace.fork());
        let mut cfg = self.cfg.clone();
        cfg.permissions.allowed_dirs = vec![workdir.clone()];
        let reg = ToolRegistry::with_defaults(workdir.clone(), cfg.permissions.clone());
        let orch = Orchestrator::new(
            cfg,
            sink.clone(),
            self.context.clone(),
            self.executor.clone(),
        );
        let out = orch
            .run_loop(
                &Session::new(task.goal.clone())
                    .with_checks(task.checks.clone())
                    .expecting_writes(task.expect_writes)
                    .with_token_limit(task.max_tokens),
                &reg,
                &workdir,
            )
            .await;
        self.trace.extend(sink.events());
        let passed = out["passed"].as_bool().unwrap_or(false);
        // The orchestrator records one entry per plan task, each with its own
        // feedback. Surface the first failure reason, else the last note, so a
        // MISMATCH line in the report is never a blank.
        let feedback = {
            let entries = out["tasks"].as_array().cloned().unwrap_or_default();
            let failed = entries.iter().find(|t| {
                !t["passed"].as_bool().unwrap_or(false)
                    && !t["feedback"].as_str().unwrap_or("").is_empty()
            });
            failed
                .or_else(|| {
                    entries
                        .iter()
                        .rev()
                        .find(|t| !t["feedback"].as_str().unwrap_or("").is_empty())
                })
                .and_then(|t| t["feedback"].as_str())
                .unwrap_or("")
                .to_string()
        };
        TaskResult {
            name: task.name.clone(),
            passed,
            expected: task.expect_pass,
            matched: passed == task.expect_pass,
            rounds: out["rounds"].as_u64().unwrap_or(0) as u32,
            feedback,
        }
    }

    /// One task in isolation: copy the tree, run, keep the copy for
    /// debugging (disposable; see README on temp-dir pressure).
    async fn run_task_isolated(&self, task: EvalTask) -> TaskResult {
        let dir = match self.prepare_task_dir(&task.name) {
            Ok(d) => d,
            Err(e) => {
                return TaskResult {
                    name: task.name.clone(),
                    passed: false,
                    expected: task.expect_pass,
                    matched: !task.expect_pass,
                    rounds: 0,
                    feedback: format!("harness: task-dir copy failed: {e:#}"),
                };
            }
        };
        let result = self.run_task_in(&task, dir.clone()).await;
        if self.cfg.clean_task_dirs {
            std::fs::remove_dir_all(&dir).ok();
        }
        result
    }

    pub async fn run_suite(&self, suite: &EvalSuite) -> SuiteReport {
        let jobs = self.cfg.max_parallel_tasks.max(1);
        self.run_suite_with_jobs(suite, jobs).await
    }

    /// Bounded fan-out over isolated task dirs. Results keep suite order
    /// regardless of finish order, so reports diff cleanly across runs.
    pub async fn run_suite_with_jobs(&self, suite: &EvalSuite, jobs: usize) -> SuiteReport {
        let jobs = jobs.max(1);
        if jobs == 1 {
            let mut rep = SuiteReport::default();
            for t in &suite.tasks {
                rep.tasks.push(self.run_task_isolated(t.clone()).await);
            }
            rep.aggregate = self.report();
            return rep;
        }
        let sem = Arc::new(tokio::sync::Semaphore::new(jobs));
        let mut handles = Vec::with_capacity(suite.tasks.len());
        for t in &suite.tasks {
            let runner = self.clone();
            let task = t.clone();
            let permit_slot = sem.clone();
            handles.push(tokio::spawn(async move {
                let _permit = permit_slot.acquire_owned().await;
                runner.run_task_isolated(task).await
            }));
        }
        let mut rep = SuiteReport::default();
        for h in handles {
            match h.await {
                Ok(r) => rep.tasks.push(r),
                Err(e) => rep.tasks.push(TaskResult {
                    name: "<join>".to_string(),
                    passed: false,
                    expected: true,
                    matched: false,
                    rounds: 0,
                    feedback: format!("harness: task join failed: {e}"),
                }),
            }
        }
        rep.aggregate = self.report();
        rep
    }
}

fn sanitize(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    s.truncate(32);
    if s.is_empty() {
        s.push_str("task");
    }
    s
}

/// Recursive copy of a task tree, skipping `target/` (rebuildable
/// artifacts, by far the largest subtree) and `.git/` (history the
/// agents must never see or mutate).
fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == "target" || name_str == ".git" {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        let ft = entry.file_type()?;
        if ft.is_dir() {
            // Best effort: unreadable subtrees fail the copy loudly; a
            // half-copied tree must never run as if it were whole.
            copy_tree(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)?;
        }
        // Symlinks and sockets are skipped: they escape the tree or
        // cannot be meaningfully copied per task.
    }
    Ok(())
}
