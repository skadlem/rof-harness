//! Goal-quality pre-check + auto-poke decision.
//!
//! Both are cheap and local — no model call, no network. The check *informs*
//! (a trace event, and a note in the planner prompt when enabled); it never
//! refuses a goal, because a refusal here would be the harness claiming a
//! judgement it cannot actually make. The auto-poke decision buys exactly one
//! extra round after a task failed at its cap; the caller owns the bound.
//!
//! Design bias: a false positive costs a trace line, a false negative costs
//! nothing. Both features are off by default (`AppConfig::goal_quality`,
//! `AppConfig::auto_poke`) because enabling either changes prompts / rounds,
//! and a change to prompts is an A/B'able change (see docs/STATUS.md), not a
//! silent one.

/// Below this many chars, a goal cannot name a file, symbol and expected
/// result — the three things every measured failure lacked.
const MIN_CHARS: usize = 24;

/// Below this many whitespace-separated words, same argument.
const MIN_WORDS: usize = 5;

/// Markers of an actionable, repo-tethered goal: a path (`src/...`), a file
/// extension, a code-shaped token (`foo()`, `foo_bar`, `A::b`), or a quoted
/// identifier.
fn has_anchor(goal: &str) -> bool {
    if goal.contains('/') || goal.contains(".rs") || goal.contains(".json") || goal.contains(".md")
    {
        return true;
    }
    if goal.contains("::") {
        return true;
    }
    if goal.contains('`') && goal.matches('`').count() >= 2 {
        return true;
    }
    // A snake_case or camelCase identifier inside a word boundary.
    goal.split(|c: char| c.is_whitespace() || c == ',' || c == '.' || c == ';')
        .any(|w| {
            let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '(');
            let has_underscore = w.contains('_') && w.len() > 3;
            let has_paren = w.ends_with("()");
            has_underscore || has_paren
        })
}

/// `Some(note)` when the goal looks too vague or too untethered from the tree
/// to be worth a plan; `None` when it is at least actionable-looking.
///
/// Two objective rules only (length, anchor): a subjective "quality" judgement
/// from a keyword heuristic would misfire in both directions, and the note is
/// shown to a model that is better at the judgement than this function is.
pub fn check_goal_quality(goal: &str) -> Option<String> {
    let g = goal.trim();
    let words = g.split_whitespace().count();
    if g.chars().count() < MIN_CHARS || words < MIN_WORDS {
        return Some(format!(
            "goal is too brief ({words} words, {} chars) to name a file, a symbol and an \
             expected result; restate it with the file it touches and what should be true \
             afterwards",
            g.chars().count()
        ));
    }
    if !has_anchor(g) {
        return Some(
            "goal names no file, symbol or code-shaped token; name the file it touches (or \
             the symbol it concerns) so retrieval and the implementer have an anchor"
                .to_string(),
        );
    }
    None
}

/// Should the harness buy one extra round after a task failed at its cap?
///
/// No when the task passed (nothing to poke) or the token ceiling aborted it
/// (spending more after hitting a budget guard contradicts the guard). Yes for
/// the two failure shapes the cap is wrong for: the reviewer rejected a pass
/// because no writes landed when writes were expected (an instruction problem,
/// not a capability problem), and a task that burned every round on the same
/// unfixed feedback.
pub fn should_auto_poke(
    passed: bool,
    ran: u32,
    writes_made: usize,
    expect_writes: bool,
    aborted: bool,
) -> bool {
    if passed || aborted {
        return false;
    }
    (expect_writes && writes_made == 0) || ran >= 2
}

/// What the extra round is told, built for the reason the poke was earned.
pub fn poke_reason(ran: u32, writes_made: usize, expect_writes: bool) -> String {
    let head = format!("AUTO-POKE: the round cap ({ran}) was reached without an accepted result.");
    if expect_writes && writes_made == 0 {
        format!(
            "{head} This task requires a file change and NONE was applied. Do not answer with a \
             plan or prose: read the target file, then emit the write/patch that makes the change."
        )
    } else {
        format!(
            "{head} Re-read the file you are changing and the last check output above; fix the \
             specific complaint rather than restating the previous attempt."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_goals_are_flagged() {
        assert!(check_goal_quality("fix it").is_some());
        assert!(check_goal_quality("improve things").is_some());
        assert!(check_goal_quality("").is_some());
    }

    #[test]
    fn anchored_goals_pass_and_vague_long_ones_do_not() {
        // Real suite goals: anchored, must pass.
        assert!(check_goal_quality(
            "In src/eval/metrics.rs, add a public method `tool_failures(&self) -> usize` on \
             EvalReport returning tool_calls"
        )
        .is_none());
        assert!(check_goal_quality(
            "Add a unit test to src/context/retriever.rs for the public helper `window_on`."
        )
        .is_none());
        // Long but unanchored: flagged with the anchor note.
        let note = check_goal_quality("Make the whole thing better and generally improve it")
            .expect("long vague goal flags");
        assert!(note.contains("anchor"), "{note}");
    }

    #[test]
    fn auto_poke_decision_matrix() {
        // Passed: never.
        assert!(!should_auto_poke(true, 2, 0, true, false));
        // Budget abort: never (spending after a budget guard contradicts it).
        assert!(!should_auto_poke(false, 2, 0, true, true));
        // Expected writes, none made: yes, even at one round.
        assert!(should_auto_poke(false, 1, 0, true, false));
        // No-write expectation (analysis task): the ran>=2 exhaustion rule.
        assert!(!should_auto_poke(false, 1, 0, false, false));
        assert!(should_auto_poke(false, 2, 0, false, false));
        assert!(should_auto_poke(false, 2, 3, true, false));
    }

    #[test]
    fn poke_reason_names_the_missing_write() {
        let r = poke_reason(2, 0, true);
        assert!(r.contains("NONE was applied"), "{r}");
        let r = poke_reason(2, 1, true);
        assert!(r.contains("specific complaint"), "{r}");
    }
}
