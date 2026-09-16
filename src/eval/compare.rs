use super::metrics::ContextMetrics;
use super::runner::{RunLabel, SuiteReport, TaskResult};

/// How one task's matched result moved between two reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskChange {
    Gained,
    Lost,
    Same,
    /// Present in one report only (a different `--limit`, or a different suite).
    OnlyInA,
    OnlyInB,
}

impl TaskChange {
    fn marker(self) -> char {
        match self {
            TaskChange::Gained => '+',
            TaskChange::Lost => '-',
            TaskChange::Same => '=',
            TaskChange::OnlyInA => '<',
            TaskChange::OnlyInB => '>',
        }
    }
}

/// One label field, side by side. `a` is the older report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelDelta {
    pub field: String,
    pub a: String,
    pub b: String,
}

impl LabelDelta {
    pub fn changed(&self) -> bool {
        self.a != self.b
    }
}

/// One aggregate number, side by side.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricDelta {
    pub name: String,
    pub a: f64,
    pub b: f64,
}

impl MetricDelta {
    pub fn delta(&self) -> f64 {
        self.b - self.a
    }
    pub fn changed(&self) -> bool {
        (self.a - self.b).abs() > f64::EPSILON
    }
}

/// One task, both sides. `None` = the task is not in that report.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskDelta {
    pub name: String,
    pub a_matched: Option<bool>,
    pub b_matched: Option<bool>,
    pub a_rounds: Option<u32>,
    pub b_rounds: Option<u32>,
    pub a_context: ContextMetrics,
    pub b_context: ContextMetrics,
    /// Why it moved: the losing side's feedback, first line, trimmed.
    pub note: String,
    /// Checks that flipped between the runs (§4.3): same name, different
    /// outcome. Empty when either side predates structured checks or when no
    /// check changed — a task that moved without one is a *model* effect, not
    /// a check effect, and that distinction is the point of the field.
    pub flipped_checks: Vec<CheckFlip>,
}

/// One configured check, two outcomes.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckFlip {
    pub name: String,
    pub a_passed: bool,
    pub b_passed: bool,
}

impl CheckFlip {
    /// The side that passed it is the one that earned the result.
    pub fn render(&self) -> String {
        format!(
            "check '{}' flipped: {} -> {}",
            self.name,
            if self.a_passed { "pass" } else { "fail" },
            if self.b_passed { "pass" } else { "fail" }
        )
    }
}

impl TaskDelta {
    pub fn change(&self) -> TaskChange {
        match (self.a_matched, self.b_matched) {
            (Some(x), Some(y)) => match (x, y) {
                (true, false) => TaskChange::Lost,
                (false, true) => TaskChange::Gained,
                _ => TaskChange::Same,
            },
            (Some(_), None) => TaskChange::OnlyInA,
            (None, Some(_)) => TaskChange::OnlyInB,
            (None, None) => TaskChange::Same,
        }
    }
}

/// The delta between two `--report` dumps: labels, matched totals, per-task
/// movement and the aggregate metrics stage 0 added. Printed by `rof compare`
/// and asserted on by the tests, so both read the same structure.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub a: RunLabel,
    pub b: RunLabel,
    pub matched_a: usize,
    pub matched_b: usize,
    pub total_a: usize,
    pub total_b: usize,
    pub labels: Vec<LabelDelta>,
    pub notes: Vec<String>,
    pub metrics: Vec<MetricDelta>,
    pub tasks: Vec<TaskDelta>,
}

impl Comparison {
    /// Tasks whose matched result moved, plus tasks only one report has.
    pub fn changed(&self) -> Vec<&TaskDelta> {
        self.tasks
            .iter()
            .filter(|t| t.change() != TaskChange::Same)
            .collect()
    }

    pub fn gained(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.change() == TaskChange::Gained)
            .count()
    }

    pub fn lost(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.change() == TaskChange::Lost)
            .count()
    }

    pub fn metric(&self, name: &str) -> Option<&MetricDelta> {
        self.metrics.iter().find(|m| m.name == name)
    }

    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("a: {}\n", self.a.render()));
        s.push_str(&format!("b: {}\n", self.b.render()));
        s.push_str(&format!(
            "matched {} -> {} of {} -> {} (gained {}, lost {})\n",
            self.matched_a,
            self.matched_b,
            self.total_a,
            self.total_b,
            self.gained(),
            self.lost()
        ));

        s.push_str("\nlabel (a -> b):\n");
        for l in &self.labels {
            s.push_str(&format!(
                "  {}{:<18}{:<22}-> {}\n",
                if l.changed() { '*' } else { ' ' },
                l.field,
                empty_as(&l.a, "(none)"),
                empty_as(&l.b, "(none)")
            ));
        }
        if self.notes.is_empty() {
            s.push_str("  (no comparability warnings)\n");
        }
        for n in &self.notes {
            s.push_str(&format!("  ! {n}\n"));
        }

        s.push_str("\ntasks (matched a -> b):\n");
        for t in &self.tasks {
            s.push_str(&format!(
                "  {} {:<30}{:<7}-> {:<7}rounds {} -> {}\n",
                t.change().marker(),
                t.name,
                matched_word(t.a_matched),
                matched_word(t.b_matched),
                rounds_word(t.a_rounds),
                rounds_word(t.b_rounds)
            ));
            if !t.note.is_empty() && t.change() != TaskChange::Same {
                let side = match t.change() {
                    TaskChange::Lost => "b",
                    _ => "a",
                };
                s.push_str(&format!("      ({side}) {}\n", t.note));
            }
            // §4.3: name the check that flipped. An empty list on a moved task
            // is itself the signal — the gate held, so the verdict moved.
            if t.change() != TaskChange::Same && !t.flipped_checks.is_empty() {
                for f in &t.flipped_checks {
                    s.push_str(&format!("      {}\n", f.render()));
                }
            }
            if t.change() != TaskChange::Same && t.change() != TaskChange::OnlyInA {
                // The context columns are the point of stage 0's instrument:
                // a task that moved while retrieval did not is a model effect.
                s.push_str(&format!(
                    "      context: retrieved {} -> {} chars, referenced {} -> {}, proxy {:.3} -> {:.3}, recall {:.2} -> {:.2} ({} -> {} files), summarize {} -> {} tok\n",
                    t.a_context.retrieved_chars,
                    t.b_context.retrieved_chars,
                    t.a_context.referenced_chars,
                    t.b_context.referenced_chars,
                    t.a_context.relevance_proxy,
                    t.b_context.relevance_proxy,
                    t.a_context.recall(),
                    t.b_context.recall(),
                    t.a_context.recalled_files,
                    t.b_context.recalled_files,
                    t.a_context.summarize_tokens,
                    t.b_context.summarize_tokens
                ));
            }
        }

        s.push_str("\nmetrics (a -> b):\n");
        s.push_str(&format!(
            "  {:<24}{:>14}{:>14}{:>14}\n",
            "name", "a", "b", "delta"
        ));
        for m in &self.metrics {
            s.push_str(&format!(
                "  {:<24}{:>14}{:>14}{:>14}\n",
                m.name,
                num(m.a),
                num(m.b),
                num_delta(m.delta())
            ));
        }
        s
    }
}

/// Compare two report dumps. Everything here is folded from the reports
/// themselves — no re-run, no model call, no re-reading of the trace.
pub fn compare(a: &SuiteReport, b: &SuiteReport) -> Comparison {
    let labels = label_delta(&a.label, &b.label);
    let notes = comparability_notes(a, b);
    let ctx_a = ContextMetrics::sum(a.tasks.iter().map(|t| t.context.clone()));
    let ctx_b = ContextMetrics::sum(b.tasks.iter().map(|t| t.context.clone()));

    let mut metrics = Vec::new();
    let mut push = |name: &str, x: f64, y: f64| {
        metrics.push(MetricDelta {
            name: name.to_string(),
            a: x,
            b: y,
        });
    };
    push("matched", a.matched() as f64, b.matched() as f64);
    push("tasks", a.tasks.len() as f64, b.tasks.len() as f64);
    push("matched_rate", a.success_rate(), b.success_rate());
    push(
        "est_cost_usd",
        a.aggregate.est_cost_usd,
        b.aggregate.est_cost_usd,
    );
    if let (Some(x), Some(y)) = (a.aggregate.cost_usd, b.aggregate.cost_usd) {
        push("provider_cost_usd", x, y);
    }
    push(
        "est_input_tokens",
        a.aggregate.est_input_tokens as f64,
        b.aggregate.est_input_tokens as f64,
    );
    push(
        "est_output_tokens",
        a.aggregate.est_output_tokens as f64,
        b.aggregate.est_output_tokens as f64,
    );
    push(
        "cached_input_tokens",
        a.aggregate.cached_input_tokens as f64,
        b.aggregate.cached_input_tokens as f64,
    );
    push(
        "cache_hit_rate",
        a.aggregate.cache_hit_rate(),
        b.aggregate.cache_hit_rate(),
    );
    push(
        "tool_calls",
        a.aggregate.tool_calls as f64,
        b.aggregate.tool_calls as f64,
    );
    push(
        "tool_accuracy",
        a.aggregate.tool_accuracy(),
        b.aggregate.tool_accuracy(),
    );
    push(
        "retried_calls",
        a.aggregate.retried_calls as f64,
        b.aggregate.retried_calls as f64,
    );
    push(
        "budget_aborts",
        a.aggregate.budget_aborts as f64,
        b.aggregate.budget_aborts as f64,
    );
    push(
        "model_errors",
        a.aggregate.model_errors as f64,
        b.aggregate.model_errors as f64,
    );
    push(
        "total_latency_ms",
        a.aggregate.total_latency_ms as f64,
        b.aggregate.total_latency_ms as f64,
    );
    push(
        "retrieved_chars",
        ctx_a.retrieved_chars as f64,
        ctx_b.retrieved_chars as f64,
    );
    push(
        "referenced_chars",
        ctx_a.referenced_chars as f64,
        ctx_b.referenced_chars as f64,
    );
    push(
        "relevance_proxy",
        ctx_a.relevance_proxy as f64,
        ctx_b.relevance_proxy as f64,
    );
    push("recall", ctx_a.recall() as f64, ctx_b.recall() as f64);
    push(
        "summarize_calls",
        ctx_a.summarize_calls as f64,
        ctx_b.summarize_calls as f64,
    );
    push(
        "summarize_tokens",
        ctx_a.summarize_tokens as f64,
        ctx_b.summarize_tokens as f64,
    );
    push(
        "truncated_views",
        ctx_a.truncated_views as f64,
        ctx_b.truncated_views as f64,
    );
    // Stage 2: per layer [long, mid, short] — where context was compressed and
    // where it was cut. Sums keep the table readable; the per-task rows above
    // and the raw report carry the layer split.
    push(
        "layer_summaries",
        ctx_a.layer_summaries.iter().map(|n| *n as f64).sum(),
        ctx_b.layer_summaries.iter().map(|n| *n as f64).sum(),
    );
    push(
        "layer_truncations",
        ctx_a.layer_truncations.iter().map(|n| *n as f64).sum(),
        ctx_b.layer_truncations.iter().map(|n| *n as f64).sum(),
    );
    // Skill-store traffic: the arm-level answer to "did an agent write a skill,
    // and did anything later reuse it?".
    push(
        "skill_listed",
        a.aggregate.skills.listed as f64,
        b.aggregate.skills.listed as f64,
    );
    push(
        "skill_viewed",
        a.aggregate.skills.viewed as f64,
        b.aggregate.skills.viewed as f64,
    );
    push(
        "skill_proposed",
        a.aggregate.skills.proposed as f64,
        b.aggregate.skills.proposed as f64,
    );
    push(
        "skill_applied",
        a.aggregate.skills.applied as f64,
        b.aggregate.skills.applied as f64,
    );
    push(
        "skill_reused",
        a.aggregate.skills.reused as f64,
        b.aggregate.skills.reused as f64,
    );

    Comparison {
        a: a.label.clone(),
        b: b.label.clone(),
        matched_a: a.matched(),
        matched_b: b.matched(),
        total_a: a.tasks.len(),
        total_b: b.tasks.len(),
        labels,
        notes,
        metrics,
        tasks: task_deltas(a, b),
    }
}

fn label_delta(a: &RunLabel, b: &RunLabel) -> Vec<LabelDelta> {
    let fields = [
        ("git_head", &a.git_head, &b.git_head),
        ("config_hash", &a.config_hash, &b.config_hash),
        ("suite_hash", &a.suite_hash, &b.suite_hash),
        ("ctx_model", &a.ctx_model, &b.ctx_model),
        ("exec_model", &a.exec_model, &b.exec_model),
        ("harness_version", &a.harness_version, &b.harness_version),
    ];
    fields
        .into_iter()
        .map(|(field, x, y)| LabelDelta {
            field: field.to_string(),
            a: x.clone(),
            b: y.clone(),
        })
        .collect()
}

/// Warnings a reader needs before believing a delta. A different suite hash
/// means the per-task table is a set difference, not an effect.
fn comparability_notes(a: &SuiteReport, b: &SuiteReport) -> Vec<String> {
    let mut notes = Vec::new();
    if a.label.is_unlabeled() {
        notes.push(
            "a: unlabeled report (written before stage 0 — no git/config/suite hash)".to_string(),
        );
    }
    if b.label.is_unlabeled() {
        notes.push(
            "b: unlabeled report (written before stage 0 — no git/config/suite hash)".to_string(),
        );
    }
    if !a.label.is_unlabeled() && !b.label.is_unlabeled() {
        if a.label.suite_hash != b.label.suite_hash {
            notes.push(
                "suite_hash differs: the reports do not cover the same task set — per-task rows are a set difference, not an effect".to_string(),
            );
        }
        if a.label.config_hash != b.label.config_hash {
            notes.push(
                "config_hash differs: the config (or an env override) changed between the runs"
                    .to_string(),
            );
        }
        if a.label.git_head != b.label.git_head {
            notes.push(format!(
                "git_head differs: harness revision moved {} -> {}",
                a.label.git_head, b.label.git_head
            ));
        }
    }
    let ctx_a = ContextMetrics::sum(a.tasks.iter().map(|t| t.context.clone()));
    let ctx_b = ContextMetrics::sum(b.tasks.iter().map(|t| t.context.clone()));
    if !a.tasks.is_empty() && ctx_a.retrieved_chars == 0 {
        notes.push(
            "a: no retrieved context recorded for any task (pre-stage-0 report, or retrieval matched nothing)"
                .to_string(),
        );
    }
    if !b.tasks.is_empty() && ctx_b.retrieved_chars == 0 {
        notes.push(
            "b: no retrieved context recorded for any task (pre-stage-0 report, or retrieval matched nothing)"
                .to_string(),
        );
    }
    notes
}

/// Checks whose outcome differs between two task results, matched by name.
/// A task that moved while no check flipped is the interesting case: the
/// change was in the model's verdict, not in the acceptance gate.
fn flipped_checks(a: &TaskResult, b: &TaskResult) -> Vec<CheckFlip> {
    let mut out = Vec::new();
    for ca in &a.checks {
        if let Some(cb) = b.checks.iter().find(|c| c.name == ca.name) {
            if ca.passed != cb.passed {
                out.push(CheckFlip {
                    name: ca.name.clone(),
                    a_passed: ca.passed,
                    b_passed: cb.passed,
                });
            }
        }
    }
    out
}

/// Union of both task lists, in `a`'s order first: reports keep suite order,
/// so the table reads like the suite unless the two differ.
fn task_deltas(a: &SuiteReport, b: &SuiteReport) -> Vec<TaskDelta> {
    let mut out: Vec<TaskDelta> = Vec::new();
    for ta in &a.tasks {
        let tb = b.tasks.iter().find(|t| t.name == ta.name);
        out.push(TaskDelta {
            name: ta.name.clone(),
            a_matched: Some(ta.matched),
            b_matched: tb.map(|t| t.matched),
            a_rounds: Some(ta.rounds),
            b_rounds: tb.map(|t| t.rounds),
            a_context: ta.context.clone(),
            b_context: tb.map(|t| t.context.clone()).unwrap_or_default(),
            note: String::new(),
            flipped_checks: match tb {
                Some(tb) => flipped_checks(ta, tb),
                None => Vec::new(),
            },
        });
    }
    for tb in &b.tasks {
        if !a.tasks.iter().any(|t| t.name == tb.name) {
            out.push(TaskDelta {
                name: tb.name.clone(),
                a_matched: None,
                b_matched: Some(tb.matched),
                a_rounds: None,
                b_rounds: Some(tb.rounds),
                a_context: ContextMetrics::default(),
                b_context: tb.context.clone(),
                note: String::new(),
                flipped_checks: Vec::new(),
            });
        }
    }
    // The note explains the move: the losing side's feedback.
    for d in out.iter_mut() {
        let note = match d.change() {
            TaskChange::Lost => b
                .tasks
                .iter()
                .find(|t| t.name == d.name)
                .map(|t| t.feedback.clone()),
            TaskChange::Gained => a
                .tasks
                .iter()
                .find(|t| t.name == d.name)
                .map(|t| t.feedback.clone()),
            _ => None,
        };
        d.note = note.map(|n| first_line(&n, 200)).unwrap_or_default();
    }
    out
}

/// First line of a feedback string, capped — a report row, not an essay.
fn first_line(s: &str, cap: usize) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut out: String = line.trim().chars().take(cap).collect();
    if line.trim().chars().count() > cap {
        out.push('…');
    }
    out
}

fn empty_as<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    if s.is_empty() {
        fallback
    } else {
        s
    }
}

fn matched_word(m: Option<bool>) -> String {
    match m {
        Some(true) => "pass".to_string(),
        Some(false) => "fail".to_string(),
        None => "-".to_string(),
    }
}

fn rounds_word(r: Option<u32>) -> String {
    match r {
        Some(v) => v.to_string(),
        None => "-".to_string(),
    }
}

/// Whole numbers print as integers; small fractions (cost, rates) keep the
/// digits that matter.
fn num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v as i64)
    } else if v.abs() < 1.0 {
        format!("{v:.5}")
    } else {
        format!("{v:.3}")
    }
}

fn num_delta(v: f64) -> String {
    if v.abs() < 1e-12 {
        return "0".to_string();
    }
    if (v - v.round()).abs() < 1e-9 && v.abs() >= 1.0 {
        format!("{v:+.0}")
    } else if v.abs() < 1.0 {
        format!("{v:+.5}")
    } else {
        format!("{v:+.3}")
    }
}
