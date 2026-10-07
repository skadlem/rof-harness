use crate::{window_anchored, ContextItem, Fidelity, ItemKey, MIN_WINDOW};
use std::collections::HashSet;

pub struct ContextAssembler {
    budget: usize,
    items: Vec<ContextItem>,
    volatile_items: Vec<ContextItem>,
    seen: HashSet<ItemKey>,
}

impl ContextAssembler {
    pub fn new(_budget_chars: usize) -> Self {
        Self {
            budget: _budget_chars,
            items: Vec::new(),
            volatile_items: Vec::new(),
            seen: HashSet::new(),
        }
    }

    /// Dedupe by key. Volatile items bypass the mid layer.
    pub fn add(&mut self, _item: ContextItem) {
        self.place(_item, false);
    }

    /// Turn-only evidence (requested files, skill bodies): fitted against the
    /// same budget, delivered last so a re-ask extends the cached prefix.
    /// Callers mark these must_include and keep them out of the mid layer.
    pub fn add_volatile(&mut self, _item: ContextItem) {
        self.place(_item, true);
    }

    fn place(&mut self, item: ContextItem, volatile: bool) {
        if self.seen.contains(&item.key) {
            return;
        }
        self.seen.insert(item.key.clone());
        if volatile {
            self.volatile_items.push(item);
        } else {
            self.items.push(item);
        }
    }

    /// Halving-to-floor fit; oversized must_include narrows, never silently cuts.
    pub fn assemble(&self) -> String {
        self.assemble_inner(self.budget)
    }

    fn shape(&self, item: &ContextItem, remaining: usize) -> Option<String> {
        match &item.fidelity {
            Fidelity::Exact => {
                let fits = item.text.chars().count() <= remaining;
                if fits || item.must_include {
                    Some(item.text.clone())
                } else {
                    None
                }
            }
            Fidelity::Windowed { anchor, cap } => {
                let mut cap = *cap;
                loop {
                    let w = window_anchored(&item.text, anchor, cap);
                    if w.chars().count() <= remaining {
                        return Some(w);
                    }
                    if cap <= MIN_WINDOW {
                        return if item.must_include { Some(w) } else { None };
                    }
                    cap = (cap / 2).max(MIN_WINDOW);
                }
            }
        }
    }

    fn fit_list(
        &self,
        list: &[ContextItem],
        out: &mut [Option<String>],
        used: &mut usize,
        budget: usize,
    ) {
        for (item, slot) in list.iter().zip(out.iter_mut()) {
            if let Some(text) = self.shape(item, budget.saturating_sub(*used)) {
                *used += format!("--- {}\n{text}\n", item.key.path).chars().count();
                *slot = Some(text);
            }
        }
    }

    fn assemble_inner(&self, budget: usize) -> String {
        let mut used = 0usize;
        let mut stable = vec![None; self.items.len()];
        let mut vol = vec![None; self.volatile_items.len()];
        self.fit_list(&self.items, &mut stable, &mut used, budget);
        self.fit_list(&self.volatile_items, &mut vol, &mut used, budget);
        let mut sections = Vec::new();
        for (item, text) in self
            .items
            .iter()
            .zip(stable.iter())
            .filter_map(|(i, t)| t.as_ref().map(|t| (i, t)))
        {
            sections.push(format!("--- {}\n{text}", item.key.path));
        }
        for (item, text) in self
            .volatile_items
            .iter()
            .zip(vol.iter())
            .filter_map(|(i, t)| t.as_ref().map(|t| (i, t)))
        {
            sections.push(format!("--- {}\n{text}", item.key.path));
        }
        sections.join("\n\n")
    }
}
