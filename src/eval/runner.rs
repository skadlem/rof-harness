use super::metrics::{ContextMetrics, EvalReport};
use super::suite::{EvalSuite, EvalTask};
use crate::config::AppConfig;
use crate::engine::{Orchestrator, Session};
use crate::llm::{ContextService, ExecutorService};
use crate::obs::TraceSink;
use crate::tools::ToolRegistry;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What makes two reports comparable (or visibly incomparable) without reading
/// the trace: which harness revision ran, with which config, on which suite,
/// against which model pair.
///
/// Hashes are FNV-1a (64-bit) over a canonical JSON dump — enough to separate
/// "same inputs" from "different inputs" inside one report, and no new
/// dependency (the plan's sha256 would add one for no extra decision power).
/// `config_hash` hashes the `rof config` dump verbatim, so it can be
/// recomputed by hand from that command's output.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunLabel {
    /// Short HEAD of the tree the suite ran against; "unknown" outside a repo.
    pub git_head: String,
    pub config_hash: String,
    pub suite_hash: String,
    pub ctx_model: String,
    pub exec_model: String,
    /// §4.5: the judge model. Defaults to exec (self-review); a report that
    /// predates the field loads as empty and renders as exec, which is what
    /// it was. The label must render it because `config_hash` alone is not
    /// human-readable and an arm that swaps only the judge is otherwise
    /// indistinguishable from a rerun.
    #[serde(default)]
    pub verify_model: String,
    pub harness_version: String,
}

impl RunLabel {
    pub fn build(cfg: &AppConfig, suite: &EvalSuite, workdir: &Path) -> Self {
        Self {
            git_head: git_head(workdir),
            config_hash: fnv1a_hex(cfg.to_json().as_bytes()),
            suite_hash: fnv1a_hex(serde_json::to_string(suite).unwrap_or_default().as_bytes()),
            ctx_model: cfg.routing.context_model.clone(),
            exec_model: cfg.routing.executor_model.clone(),
            verify_model: cfg
                .routing
                .verify_model
                .clone()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| cfg.routing.executor_model.clone()),
            harness_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// True for a report written before labels existed (serde default).
    pub fn is_unlabeled(&self) -> bool {
        self.config_hash.is_empty() && self.suite_hash.is_empty() && self.git_head.is_empty()
    }

    pub fn render(&self) -> String {
        if self.is_unlabeled() {
            return "unlabeled (report predates stage 0)".to_string();
        }
        // An old report has no verify field; it was self-review, which is
        // exactly what printing exec there says.
        let verify = if self.verify_model.is_empty() {
            &self.exec_model
        } else {
            &self.verify_model
        };
        format!(
            "git {} cfg {} suite {} ctx {} exec {} verify {} v{}",
            self.git_head,
            self.config_hash,
            self.suite_hash,
            self.ctx_model,
            self.exec_model,
            verify,
            self.harness_version
        )
    }
}

/// FNV-1a, 64-bit, hex. In-house on purpose: the report only needs to tell
/// identical inputs from different ones, and a hash is not a security boundary.
pub fn fnv1a_hex(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Short HEAD of `dir` through git; "unknown" when git is absent, the path is
/// not a repo, or HEAD is unborn. A label must never fail a run.
pub fn git_head(dir: &Path) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub name: String,
    pub passed: bool,
    pub expected: bool,
    pub matched: bool,
    pub rounds: u32,
    pub feedback: String,
    /// Context accounting for this task (stage 0). Zeroes on a report written
    /// before the field existed.
    #[serde(default)]
    pub context: ContextMetrics,
    /// The configured checks and their outcomes (§4.3). Zeroes on a report
    /// written before the field existed; the rendered log is not kept here —
    /// `compare` reads outcomes, not prose.
    #[serde(default)]
    pub checks: Vec<crate::engine::CheckResult>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SuiteReport {
    #[serde(default)]
    pub tasks: Vec<TaskResult>,
    pub aggregate: EvalReport,
    /// Which harness/config/suite/model pair produced this report.
    #[serde(default)]
    pub label: RunLabel,
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
    /// Load a `--report` dump back. Old reports (no label, no per-task context)
    /// load through the serde defaults, so a baseline stays diffable.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("report {}: {e}", path.display()))?;
        let rep: Self = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("report {}: {e}", path.display()))?;
        Ok(rep)
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
    /// §4.5: the judge the label must name. Held separately from `executor`
    /// because arm #4's whole point is that they differ.
    verify: ExecutorService,
    workdir: PathBuf,
}

impl EvaluationRunner {
    pub fn new(
        trace: Arc<TraceSink>,
        cfg: AppConfig,
        context: ContextService,
        executor: ExecutorService,
        verify: ExecutorService,
        workdir: PathBuf,
    ) -> Self {
        Self {
            trace,
            cfg,
            context,
            executor,
            verify,
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
        let reg = ToolRegistry::with_defaults(
            workdir.clone(),
            cfg.permissions.clone(),
            cfg.skills.clone(),
        );
        let orch = Orchestrator::new(
            cfg,
            sink.clone(),
            self.context.clone(),
            self.executor.clone(),
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
        // feedback and checks. Surface the first failure's reason and its
        // checks (else the last entry's), so a MISMATCH line in the report is
        // never blank and the flipped-check row names the task that failed.
        let (feedback, checks) = {
            let entries = out["tasks"].as_array().cloned().unwrap_or_default();
            let failed = entries.iter().find(|t| {
                !t["passed"].as_bool().unwrap_or(false)
                    && !t["feedback"].as_str().unwrap_or("").is_empty()
            });
            let pick = failed.or_else(|| {
                entries
                    .iter()
                    .rev()
                    .find(|t| !t["feedback"].as_str().unwrap_or("").is_empty())
            });
            (
                pick.and_then(|t| t["feedback"].as_str())
                    .unwrap_or("")
                    .to_string(),
                pick.and_then(|t| t["check_results"].as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| serde_json::from_value(c.clone()).ok())
                            .collect()
                    })
                    .unwrap_or_default(),
            )
        };
        TaskResult {
            name: task.name.clone(),
            passed,
            expected: task.expect_pass,
            matched: passed == task.expect_pass,
            rounds: out["rounds"].as_u64().unwrap_or(0) as u32,
            feedback,
            context: ContextMetrics::from_run(&out),
            checks,
        }
    }

    /// One task in isolation: copy the tree, run, then delete the copy
    /// unless `clean_task_dirs` is off. A suite whose checks run a build
    /// leaves a full `target/` per task (~1 GB on a Rust repo), and that
    /// blowup is debug-only state that has cost real disk (and, when the
    /// disk is a small tmpfs, real task failures).
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
                    context: ContextMetrics::default(),
                    checks: Vec::new(),
                };
            }
        };
        let res = self.run_task_in(&task, dir.clone()).await;
        if self.cfg.clean_task_dirs {
            // Best effort: a copy that will not delete must never fail the
            // task that just succeeded.
            std::fs::remove_dir_all(&dir).ok();
        }
        res
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
            for task in &rep.tasks {
                rep.aggregate.record_task(task.passed);
            }
            rep.label = self.label(suite);
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
                    context: ContextMetrics::default(),
                    checks: Vec::new(),
                }),
            }
        }
        rep.aggregate = self.report();
        for task in &rep.tasks {
            rep.aggregate.record_task(task.passed);
        }
        rep.label = self.label(suite);
        rep
    }

    /// Which harness revision / config / suite / model pair this report is
    /// about. Hashed from the effective config (post-env) and the suite as it
    /// ran, so a `--limit` run labels the tasks it actually executed.
    ///
    /// The model ids come from the services, not from `cfg.routing`: those are
    /// the ids actually sent to the provider (`rof eval` wires both from the
    /// same config, a test or embedder may not).
    fn label(&self, suite: &EvalSuite) -> RunLabel {
        let mut l = RunLabel::build(&self.cfg, suite, &self.workdir);
        l.ctx_model = self.context.model.clone();
        l.exec_model = self.executor.model.clone();
        // The label's ground truth is the services actually wired, not the
        // config's intent: `ROF_VERIFY_MODEL` and the judge's own client are
        // both resolved in `main`, so the report must say what ran.
        l.verify_model = self.verify.model.clone();
        l
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

/// Recursive copy of a task tree, skipping `target/` (rebuildable artifacts,
/// by far the largest subtree). `.git` comes along as the tree-state substrate
/// (§4.2) — shallow when the source history is large, or `git init` plus one
/// commit when the source was no repo — so the copy is always a repo and
/// rollback never silently degrades. The agents never see `.git`: it is
/// excluded from retrieval and from every path the file tools resolve
/// (`tools::resolve_under`).
fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == "target" {
            continue;
        }
        if name_str == ".git" {
            // Copied as repo state below (shallow when large), not as a plain
            // subtree.
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
    if src.join(".git").is_dir() {
        crate::engine::tree::copy_git_state(src, dst)?;
    } else {
        // A source without git still gets a substrate: rollback and the write
        // gate work on every task copy, not just repo tasks.
        crate::engine::tree::TreeService::new(dst.to_path_buf()).ensure()?;
    }
    Ok(())
}
