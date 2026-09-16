use super::state::{CtxState, CtxView};
use crate::config::TokenBudgets;

pub struct ContextBuilder {
    budgets: TokenBudgets,
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
            // keep head + tail (conventions + latest logs matter most)
            let head = max_chars * 2 / 3;
            let tail = max_chars - head;
            let h = &s[..head.min(s.len())];
            let t = &s[s.len().saturating_sub(tail)..];
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
