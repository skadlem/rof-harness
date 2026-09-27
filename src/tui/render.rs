use crate::obs::TraceEvent;

/// Select event indexes whose rendered text (or event name) contains `query`.
///
/// Replay search is deliberately pure: the caller owns the event list and
/// App owns the cursor. Matching the serialized event as well as the human
/// line makes event names such as `ReviewVerdict` searchable.
pub fn replay_filter(events: &[TraceEvent], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return (0..events.len()).collect();
    }
    events
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            let rendered = render_line(event).to_lowercase();
            let serialized = serde_json::to_string(event)
                .unwrap_or_default()
                .to_lowercase();
            rendered.contains(&query) || serialized.contains(&query)
        })
        .map(|(index, _)| index)
        .collect()
}

/// One event in, transcript lines out. Pure: no I/O, no terminal.
pub fn render_line(ev: &TraceEvent) -> String {
    match ev {
        TraceEvent::SessionStart { goal, .. } => format!("▶ {goal}"),
        TraceEvent::ModelCall {
            agent,
            input_tokens,
            output_tokens,
            ..
        } => {
            format!("● {agent} (in {input_tokens}, out {output_tokens})")
        }
        TraceEvent::ModelError { agent, error } => format!("! {agent}: {error}"),
        // §6: the context-per-turn measurement. A line like every other: the
        // number only matters if a transcript can be read for it, and a new
        // event kind that renders as nothing would be a measurement no run
        // could report.
        TraceEvent::ContextMeasured {
            agent, turn, chars, ..
        } => format!("≡ {agent} {turn}: {chars} chars of context"),
        TraceEvent::ToolCall {
            agent, tool, ok, ..
        } => {
            format!("  {} {agent}: {tool}", if *ok { "✓" } else { "✗" })
        }
        TraceEvent::StateTransition { from, to } => format!("· {from} → {to}"),
        TraceEvent::ReviewVerdict { pass, feedback } => {
            let first = feedback.lines().next().unwrap_or("");
            format!(
                "{} reviewer: {first}",
                if *pass { "✓ pass" } else { "✗ fail —" }
            )
        }
        TraceEvent::BudgetExceeded {
            task,
            tokens,
            limit,
        } => {
            format!("! budget: {task} hit {tokens}/{limit}")
        }
        TraceEvent::SkillOp {
            agent,
            op,
            name,
            ok,
            ..
        } => {
            format!(
                "▸ skill {op}:{name} ({agent}, {})",
                if *ok { "ok" } else { "failed" }
            )
        }
        TraceEvent::GoalQuality { note, .. } => format!("· goal note: {note}"),
        // The DEGRADED teaching step only. A concept already explained is
        // excluded silently, so this line never fires for the anti-nag
        // case — it fires when the profile store could not be read or
        // written, which is a fault the user should be able to see.
        TraceEvent::LessonSkipped { concept, reason } => {
            format!("· lesson skipped ({concept}): {reason}")
        }
        // §4's meta layer. The plan line names the artifact, because a
        // recorded run's plan is read from the FILE, not from scrollback.
        // A declined gate says so: silence would read as a run that never
        // considered decomposing.
        TraceEvent::Plan {
            tasks,
            path,
            reason,
        } => {
            if tasks.is_empty() {
                format!("· plan: single task — {reason}")
            } else {
                format!("· plan: {} task(s) → {path}", tasks.len())
            }
        }
        TraceEvent::AutoPoke { task, reason } => format!("· auto-poke {task}: {reason}"),
        // The same formatter the live reducer uses, so a replayed
        // acknowledgement reads identically to the live one.
        TraceEvent::Control(ack) => super::app::control_ack_line(ack),
        // Diff evidence is pane state, not scrollback: the reducer stores it
        // and returns, so this arm is only reached by the lenient raw-line
        // reader, where naming the event beats showing a patch as a line of
        // transcript.
        TraceEvent::DiffSnapshot { names, .. } => {
            format!(
                "· (diff evidence: {} file(s) — see the diff pane)",
                names.len()
            )
        }
    }
}

/// A JSONL line that no longer parses (a newer harness wrote it): dim marker, never an error.
pub fn render_unknown(_raw: &str) -> String {
    "· (unrecognized event)".to_string()
}

/// Parse one JSONL trace line leniently: known events render, anything else
/// becomes a dim marker. Plan B's file tailer uses this; unknown kinds must
/// never break the console.
pub fn parse_lenient_line(line: &str) -> String {
    match serde_json::from_str::<TraceEvent>(line) {
        Ok(ev) => render_line(&ev),
        Err(_) => render_unknown(line),
    }
}

/// Said where a cost figure would go when the trace recorded none: a call
/// whose `cost_usd` is absent says nothing about money, which is not the
/// same claim as a call the provider reported as free.
pub const COST_UNRECORDED: &str = "unrecorded";

/// Said where the model identity goes before any model call has been
/// recorded. Named plainly: an unknown model must never read as a named one.
pub const NO_MODEL_YET: &str = "none yet";

/// The recorded spend, for the status metrics row.
///
/// Cents are only shown above a cent: rounding a sub-cent amount to `$0.00`
/// would read as a free run, so below that the amount keeps the digits it
/// actually carries. `recorded` is what separates a provider that reported
/// `$0.00` from a trace that reported no cost at all — without it both sum
/// to `0.0` and the row would claim a run was free on no evidence.
///
/// No price is ever inferred here: this formats the sum the fold recorded
/// and nothing else.
pub fn format_cost(usd: f64, recorded: bool) -> String {
    if !recorded {
        return COST_UNRECORDED.to_string();
    }
    if usd == 0.0 {
        return "$0.00".to_string();
    }
    if usd.abs() >= 0.01 {
        return format!("${usd:.2}");
    }
    format!("${usd}")
}

/// The model identity the trace recorded, as one compact status segment.
///
/// The trace records a model per call, not a routing role, so each path's
/// LAST recorded model is shown and named for the path it came from: a run
/// that executed and reviewed on different models cannot read as one model.
/// A recorded but blank id is no identity at all, so it is left to the
/// caller's `None` and never printed as though it named a model.
pub fn format_models(executor: Option<&str>, reviewer: Option<&str>) -> String {
    match (executor, reviewer) {
        (None, None) => NO_MODEL_YET.to_string(),
        (Some(model), None) => format!("exec={model}"),
        (None, Some(model)) => format!("rev={model}"),
        (Some(exec), Some(rev)) => format!("exec={exec} rev={rev}"),
    }
}

/// Whether a recorded call came from the review path. The trace carries the
/// agent's own name and nothing finer, so this is the one distinction the
/// events support: every agent that is not a reviewer (`executor`,
/// `implementer`, `context`) counts as the execution path.
fn is_review_path(agent: &str) -> bool {
    agent.to_ascii_lowercase().contains("review")
}

#[derive(Debug, Default, PartialEq)]
pub struct Counters {
    pub model_calls: u64,
    pub in_tokens: u64,
    pub out_tokens: u64,
    pub cost_usd: f64,
    /// True once any `ModelCall` carried a `cost_usd` value. `cost_usd`
    /// alone cannot tell a run a provider reported as free from a trace
    /// that recorded no cost at all: both fold to `0.0`.
    pub cost_recorded: bool,
    /// The last model the execution path called, as recorded.
    pub executor_model: Option<String>,
    /// The last model the review path called, as recorded.
    pub reviewer_model: Option<String>,
    pub pass: u64,
    pub fail: u64,
}

impl Counters {
    /// Fold one event into the counters. The single writer of every field
    /// here: [`Self::fold`] and the live `App` reducer both go through it,
    /// so a replayed trace and a live run cannot report different totals for
    /// the same events.
    pub fn apply(&mut self, ev: &TraceEvent) {
        match ev {
            TraceEvent::ModelCall {
                agent,
                model,
                input_tokens,
                output_tokens,
                cost_usd,
                ..
            } => {
                self.model_calls += 1;
                self.in_tokens += input_tokens;
                self.out_tokens += output_tokens;
                if let Some(cost) = cost_usd {
                    self.cost_usd += cost;
                    self.cost_recorded = true;
                }
                let model = model.trim();
                if !model.is_empty() {
                    if is_review_path(agent) {
                        self.reviewer_model = Some(model.to_string());
                    } else {
                        self.executor_model = Some(model.to_string());
                    }
                }
            }
            TraceEvent::ReviewVerdict { pass, .. } => {
                if *pass {
                    self.pass += 1;
                } else {
                    self.fail += 1;
                }
            }
            _ => {}
        }
    }

    pub fn fold(events: &[TraceEvent]) -> Self {
        let mut c = Self::default();
        for ev in events {
            c.apply(ev);
        }
        c
    }
}
