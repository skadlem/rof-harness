use crate::obs::TraceEvent;

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
        TraceEvent::AutoPoke { task, reason } => format!("· auto-poke {task}: {reason}"),
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

#[derive(Debug, Default, PartialEq)]
pub struct Counters {
    pub model_calls: u64,
    pub in_tokens: u64,
    pub out_tokens: u64,
    pub cost_usd: f64,
    pub pass: u64,
    pub fail: u64,
}

impl Counters {
    pub fn fold(events: &[TraceEvent]) -> Self {
        let mut c = Self::default();
        for ev in events {
            match ev {
                TraceEvent::ModelCall {
                    input_tokens,
                    output_tokens,
                    cost_usd,
                    ..
                } => {
                    c.model_calls += 1;
                    c.in_tokens += input_tokens;
                    c.out_tokens += output_tokens;
                    c.cost_usd += cost_usd.unwrap_or(0.0);
                }
                TraceEvent::ReviewVerdict { pass, .. } => {
                    if *pass {
                        c.pass += 1;
                    } else {
                        c.fail += 1;
                    }
                }
                _ => {}
            }
        }
        c
    }
}
