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

/// Goal-shape classifier. It used to drive the planner's "auto" mode; the
/// planner is gone, and this stays as the router primitive for any future
/// auto-split: true when the goal is already task-shaped — it names a
/// file/symbol anchor AND opens with an imperative code-action verb.
/// Conservative by construction: no anchor or no verb means not task-shaped.
const TASK_VERBS: [&str; 19] = [
    "fix",
    "add",
    "remove",
    "refactor",
    "implement",
    "update",
    "change",
    "create",
    "delete",
    "move",
    "rename",
    "extract",
    "replace",
    "migrate",
    "bump",
    "wire",
    "hoist",
    "collapse",
    "split",
];

/// Why the meta layer did or did not buy a decomposition for a goal.
///
/// Two cheap local checks, no model call, and the answer is recorded rather
/// than inferred: a reader of a trace must be able to tell a DELIBERATE
/// single-task run from a run that never considered decomposing, because
/// the arm measurement later reads exactly that difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecompositionGate {
    /// The goal already names one task. The measured win in this repo was
    /// SKIPPING the planner, so this is the case that must stay free.
    AlreadyOneTask,
    /// Too brief, or too untethered, to split into tasks that each name a
    /// file and an expected result. A decomposition of "make it better"
    /// produces three tasks that each do nothing.
    TooThinToSplit,
    /// Not one task, and substantial enough to be worth splitting. This is
    /// the only arm that spends a call.
    Fire,
}

impl DecompositionGate {
    /// Does this goal buy a decomposition call?
    pub fn fires(self) -> bool {
        matches!(self, DecompositionGate::Fire)
    }

    /// Why the gate decided what it did, in the words a reader sees.
    pub fn reason(self) -> &'static str {
        match self {
            DecompositionGate::AlreadyOneTask => {
                "goal is already one task (imperative verb + a file/symbol anchor)"
            }
            DecompositionGate::TooThinToSplit => {
                "goal is too brief or names no file/symbol to split into tasks that each do \
                 something"
            }
            DecompositionGate::Fire => {
                "goal is not one task and is substantial and anchored enough to split"
            }
        }
    }
}

/// The gate on the meta layer's one paid call (design report §9 item 4).
///
/// It is the CONJUNCTION of two cheap local checks, and both are necessary:
///
/// 1. `!goal_is_task_shaped(goal)` — the goal is not already one task.
/// 2. `check_goal_quality(goal).is_none()` — the goal is substantial enough
///    (MIN_CHARS / MIN_WORDS) and names a file, symbol or code-shaped token.
///
/// Check 1 alone is NOT a price control, and that was measured rather than
/// assumed: over the 30 goals in `eval/suites/*.json` it fires on 24 (80%),
/// because it demands the goal OPEN WITH an imperative verb and almost every
/// real ticket opens with "In src/foo.rs," or "Explain ...". A gate that
/// fires on four goals in five is not a gate.
///
/// Check 2 is what makes it one. It reuses the thresholds the quality note
/// already applies, so there is one notion of "too thin to act on" rather
/// than two. Together they fire on 20 of the 30 suite goals (67%) and on
/// none of the goals the existing tests use, so the stage costs those runs
/// exactly what they cost before it existed.
///
/// Read the 67% as an UPPER BOUND, not a typical rate: these suites are a
/// curated corpus of deliberately hard, multi-part tickets, the most
/// decomposition-friendly traffic that exists. The price question is not
/// answered by this rate at all — it needs an arm (decomposition on vs off,
/// quality at equal or lower cost).
pub fn decomposition_gate(goal: &str) -> DecompositionGate {
    if goal_is_task_shaped(goal) {
        return DecompositionGate::AlreadyOneTask;
    }
    if check_goal_quality(goal).is_some() {
        return DecompositionGate::TooThinToSplit;
    }
    DecompositionGate::Fire
}

pub fn goal_is_task_shaped(goal: &str) -> bool {
    let first = goal.split_whitespace().next().unwrap_or("");
    let verb = first
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_ascii_lowercase();
    TASK_VERBS.contains(&verb.as_str()) && has_anchor(goal)
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
    fn task_shaped_goals_skip_the_planner() {
        assert!(goal_is_task_shaped("Fix the login redirect in src/auth.rs"));
        assert!(goal_is_task_shaped(
            "Add retry with backoff to src/llm/openrouter.rs"
        ));
    }

    #[test]
    fn vague_or_anchorless_goals_still_plan() {
        assert!(!goal_is_task_shaped("Review the architecture of rof"));
        assert!(!goal_is_task_shaped("fix it"));
        assert!(!goal_is_task_shaped("How does the retriever work?"));
        assert!(!goal_is_task_shaped("Verify the build is green"));
    }

    #[test]
    fn the_decomposition_gate_is_the_conjunction_of_both_checks() {
        // One task already: never pay.
        assert_eq!(
            decomposition_gate("Fix the login redirect in src/auth.rs"),
            DecompositionGate::AlreadyOneTask
        );
        // Not one task, and too thin to split: still never pay. A plan for
        // "make it better" is three tasks that each do nothing.
        assert_eq!(
            decomposition_gate("make it better"),
            DecompositionGate::TooThinToSplit
        );
        // Not one task, substantial, anchored: the one arm that pays.
        assert_eq!(
            decomposition_gate(
                "The retry policy in src/llm/openrouter.rs must back off, must cap total time, \
                 and must say which limit it hit"
            ),
            DecompositionGate::Fire
        );
        // Every decision names itself, so the trace never shows a bare "no".
        for g in [
            "Fix src/a.rs",
            "make it better",
            "The retry policy in src/b.rs must back off and cap time",
        ] {
            assert!(!decomposition_gate(g).reason().is_empty(), "{g:?}");
        }
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
