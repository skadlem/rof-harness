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
        // The same formatter the live reducer uses, so a replayed
        // acknowledgement reads identically to the live one.
        TraceEvent::Control(ack) => super::app::control_ack_line(ack),
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
