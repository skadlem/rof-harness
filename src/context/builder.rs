use super::state::{CtxState, CtxView};
use crate::config::TokenBudgets;

pub struct ContextBuilder {
    budgets: TokenBudgets,
}

/// Nearest char boundary at or below `i` (0 when there is none).
fn floor_boundary(s: &str, i: usize) -> usize {
    let i = i.min(s.len());
    (0..=i).rev().find(|&j| s.is_char_boundary(j)).unwrap_or(0)
}

/// Nearest char boundary at or above `i` (`s.len()` when there is none).
fn ceil_boundary(s: &str, i: usize) -> usize {
    let i = i.min(s.len());
    (i..=s.len())
        .find(|&j| s.is_char_boundary(j))
        .unwrap_or(s.len())
}

impl ContextBuilder {
    pub fn new(budgets: TokenBudgets) -> Self {
        Self { budgets }
    }

    fn fit(s: &str, budget: usize) -> (String, bool) {
        // rough token estimate: 4 chars per token
        let max_chars = budget * 4;
        if s.len() <= max_chars {
            (s.to_string(), false)
        } else {
            // keep head + tail (conventions + latest logs matter most).
            // Cut on char boundaries, never raw byte indices: a cut landing
            // inside a multi-byte character panicked the run (measured live:
            // "start byte index 1 is not a char boundary").
            let head = max_chars * 2 / 3;
            let tail = max_chars - head;
            let hs = floor_boundary(s, head);
            let ts = ceil_boundary(s, s.len() - tail);
            let h = &s[..hs];
            let t = &s[ts..];
            (format!("{h}\n...[truncated]...\n{t}"), true)
        }
    }

    pub fn build(&self, state: &CtxState) -> CtxView {
        let (l, tl) = Self::fit(&state.long_term, self.budgets.long_term);
        let (m, tm) = Self::fit(&state.mid_term, self.budgets.mid_term);
        let (s, ts) = Self::fit(&state.short_term, self.budgets.short_term);
        let prompt = format!("[LONG-TERM]\n{l}\n\n[MID-TERM]\n{m}\n\n[SHORT-TERM]\n{s}");
        let used = prompt.len() / 4;
        CtxView {
            prompt,
            used_tokens: used,
            truncated: tl || tm || ts,
        }
    }
}
