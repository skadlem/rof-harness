//! §4.1: the one budget owner for everything below the layers.
//!
//! `ContextBuilder` budgets the three layers, then `ImplementerAgent` appended
//! a file map, up to three whole requested files and two skill bodies *after*
//! the cut, and the reviewer's evidence was read at 262 KB a file and then
//! head+tail-collapsed by the short layer's budget. So the largest and least
//! governed token consumers in the system were exactly the ones no budget saw,
//! and the same file's bytes could reach a prompt three times in one round.
//!
//! This module is the single owner for those parts. Three rules, kept small:
//!
//! - **Fill, don't optimize.** Every `must_include` item fits, or the assembly
//!   returns [`Assembly::SelectionFailure`] — a first-class result the caller
//!   acts on instead of a silent cut that drops the region a task depends on.
//!   Optional items fill whatever is left. This is a `retain` loop, not a
//!   knapsack: nothing is scored, and nothing is rearranged to make room.
//! - **Dedupe by [`ItemKey`].** A named file appears once per role, whatever
//!   path it arrived by. The metric is [`Self::eliminated_chars`].
//! - **Order for the cache, don't type it.** The caller adds stable parts first
//!   and volatile parts last, so the tail rides the prefix cache. Same effect
//!   as a typed head/tail partition, without the abstraction. The head's own
//!   per-round block rides last too — see [`PromptParts::volatile_head`].
//!
//! The layers keep their own policy: that path is the measured, content-cached
//! summarization, and it is already bounded. What was unbounded is below it.

use super::retriever::window_anchored;
use std::collections::HashSet;

/// Below this many chars a window is too narrow to judge, so a `must_include`
/// item that still does not fit after [`SHRINK_PASSES`] halvings is excess.
///
/// Also the floor for §6's `per_turn_context_cap`: a budget under this is one
/// the assembler can only refuse to narrow, so a cap below it would not make
/// the context smaller, it would make an item vanish.
pub const MIN_WINDOW: usize = 2_000;

/// How much of an item's text may be delivered, and how the elision happens
/// when it does not fit.
#[derive(Debug, Clone, PartialEq)]
pub enum Fidelity {
    /// All of it. Nothing may be elided, so an overflow is a selection failure.
    Exact,
    /// `cap` chars centred on `anchor`, falling back to head+tail when the
    /// anchor is absent. Evidence is what this is for.
    Windowed { anchor: String, cap: usize },
    /// The whole item is the unit of elision: it is taken or left.
    Drop,
}

/// `(path, region, role)` — a named file appears once per role. Two windows of
/// the same file with different regions are different items; the same window
/// arriving twice is a duplicate, whatever path it came by.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ItemKey {
    pub path: String,
    pub region: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextItem {
    pub key: ItemKey,
    /// `--- src/foo.rs` / `[REQUESTED FILES]` — what the prompt labels it.
    pub label: String,
    pub text: String,
    pub fidelity: Fidelity,
    /// The edit surface and the evidence: dropping it changes what the agent
    /// can do, so it may not be silently elided.
    pub must_include: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptParts {
    /// The layers, unchanged and already budgeted by `ContextBuilder`.
    pub head: String,
    /// Everything this assembler placed, in the order it was added.
    pub tail: String,
    /// The head's per-round block, delivered AFTER `tail` — the round counter,
    /// the reviewer feedback, the check log. Empty when the caller's head has
    /// no per-round block, in which case this is the pre-§6 shape exactly.
    ///
    /// It carries the separator that joined it to `head` in the first place (see
    /// [`split_volatile_head`](super::builder::split_volatile_head)), so
    /// `head + "\n\n" + tail + volatile_head` is the same bytes as the unsplit
    /// `head + "\n\n" + tail`, in an order that keeps the stable tail cacheable.
    pub volatile_head: String,
    /// What [`ContextAssembler::add_volatile`] placed: the items that exist only
    /// on this turn, delivered last so a turn that re-sends a previous one
    /// extends it byte for byte instead of rewriting its tail.
    pub volatile_tail: String,
}

/// The outcome of one assembly. [`Assembly::SelectionFailure`] carries the
/// parts that *did* fit alongside what could not: a caller that cannot narrow
/// further still gets a prompt, and the excess is a signal it can trace and
/// act on — narrow retrieval, drop a requested file, tighten a window.
#[derive(Debug, Clone, PartialEq)]
pub enum Assembly {
    Ok(PromptParts),
    SelectionFailure {
        parts: PromptParts,
        excess: Vec<ContextItem>,
    },
}

impl PromptParts {
    /// The assembled prompt: the stable head, then everything the assembler
    /// placed, then the per-round block, then the items that exist only on this
    /// turn — the order §6's cache prefix needs. Empty parts contribute
    /// nothing, and an absent per-round block leaves the string byte-identical
    /// to the unsplit shape.
    pub fn full(&self) -> String {
        let mut out = self.head.clone();
        if !self.tail.is_empty() {
            out.push_str("\n\n");
            out.push_str(&self.tail);
        }
        // The per-round block carries its own separator, then the turn's own
        // items go below it: a re-ask is this turn's ask plus those, so the
        // second turn of a round reads the first turn's cache.
        out.push_str(&self.volatile_head);
        if !self.volatile_tail.is_empty() {
            out.push_str("\n\n");
            out.push_str(&self.volatile_tail);
        }
        out
    }
}

/// One budget for the volatile parts of one prompt. Add the stable parts
/// first, the volatile parts last, then [`Self::assemble`].
pub struct ContextAssembler<'a> {
    head: &'a str,
    /// The head's per-round block, kept aside so [`PromptParts::full`] can
    /// deliver it after `tail`. See [`ContextAssembler::with_volatile_head`].
    volatile: &'a str,
    budget: usize,
    items: Vec<ContextItem>,
    /// Items that exist only on this turn (what a `reads` request answered).
    /// They are fitted against the same budget, in the same order they were
    /// added, and delivered last — see [`Self::add_volatile`].
    volatile_items: Vec<ContextItem>,
    placed: HashSet<ItemKey>,
    /// Chars a duplicate carried. The metric §4.1 names.
    eliminated: usize,
}

impl<'a> ContextAssembler<'a> {
    pub fn new(head: &'a str, budget: usize) -> Self {
        Self::with_volatile_head(head, "", budget)
    }

    /// An assembler whose head carries a per-round block, delivered after
    /// everything it places instead of inside `head`.
    ///
    /// §6: a per-round counter in the middle of a request invalidates every
    /// byte below it, so the largest stable thing the harness has — the file
    /// map and the goal-named file — was re-sent from scratch on every round
    /// because a 20-char counter sat above it. Splitting the head here is the
    /// fix, and it moves content: `head + "\n\n" + tail` becomes
    /// `stable + "\n\n" + tail + per_round`, the same bytes in an order the
    /// provider's cache can hold on to. Get the two halves from
    /// [`split_volatile_head`](super::builder::split_volatile_head), which
    /// leaves the separator with the per-round half.
    pub fn with_volatile_head(head: &'a str, volatile: &'a str, budget: usize) -> Self {
        Self {
            head,
            volatile,
            budget,
            items: Vec::new(),
            volatile_items: Vec::new(),
            placed: HashSet::new(),
            eliminated: 0,
        }
    }

    /// Registers an item, or drops it as a duplicate of one already added. A
    /// duplicate is counted whether or not its original was delivered: the
    /// dedupe is on the request, not on the placement.
    pub fn add(&mut self, item: ContextItem) -> &mut Self {
        self.place(item, false)
    }

    /// Registers an item that exists only on THIS turn — what a `reads` request
    /// answered, a skill body asked for on the spot — and delivers it last.
    ///
    /// §6: the re-ask re-sends the ask's whole context and adds these. Keeping
    /// them below the per-round block as well as below the stable tail is what
    /// makes the re-ask a byte-exact extension of the ask instead of a rewrite
    /// of its middle, so the second turn of a round reads the first turn's
    /// cache. The budget accounting is unchanged: they are fitted in the order
    /// they were added, against the same budget as [`Self::add`].
    pub fn add_volatile(&mut self, item: ContextItem) -> &mut Self {
        self.place(item, true)
    }

    fn place(&mut self, item: ContextItem, volatile: bool) -> &mut Self {
        if self.placed.contains(&item.key) {
            self.eliminated += item.text.chars().count();
            return self;
        }
        self.placed.insert(item.key.clone());
        if volatile {
            self.volatile_items.push(item);
        } else {
            self.items.push(item);
        }
        self
    }

    pub fn eliminated_chars(&self) -> usize {
        self.eliminated
    }

    /// Shape `item` to fit `remaining` chars, halving a window until it fits
    /// or reaches [`MIN_WINDOW`]. Returns the text at the narrowest fidelity
    /// tried and whether it fits.
    fn fit(&self, item: &ContextItem, remaining: usize) -> (String, bool) {
        match &item.fidelity {
            Fidelity::Exact | Fidelity::Drop => {
                let text = item.text.clone();
                let fits = text.chars().count() <= remaining;
                (text, fits)
            }
            Fidelity::Windowed { anchor, cap } => {
                let mut cap = *cap;
                loop {
                    let text = window_anchored(&item.text, anchor, cap);
                    if text.chars().count() <= remaining {
                        return (text, true);
                    }
                    // ponytail: stop at the floor rather than looping to
                    // zero; a view narrower than MIN_WINDOW judges nothing.
                    if cap <= MIN_WINDOW {
                        return (text, false);
                    }
                    cap = (cap / 2).max(MIN_WINDOW);
                }
            }
        }
    }

    pub fn assemble(&mut self) -> Assembly {
        let mut tail = String::new();
        let mut volatile_tail = String::new();
        let mut used = 0usize;
        let mut excess = Vec::new();
        // One budget, one pass, in the order the items were added: the split
        // below is a delivery order, not a second fitting pass, so nothing here
        // can be delivered that the unsplit assembler would not have delivered.
        for (item, volatile) in self
            .items
            .iter()
            .map(|i| (i, false))
            .chain(self.volatile_items.iter().map(|i| (i, true)))
        {
            let (text, fits) = self.fit(item, self.budget.saturating_sub(used));
            if !fits {
                if item.must_include {
                    excess.push(item.clone());
                }
                // Neither a failure nor an optional drop consumes the budget,
                // so the next item gets the whole remainder.
                continue;
            }
            let target = if volatile {
                &mut volatile_tail
            } else {
                &mut tail
            };
            target.push_str(&item.label);
            target.push('\n');
            target.push_str(&text);
            target.push('\n');
            used += item.label.chars().count() + 1 + text.chars().count() + 1;
        }
        let parts = PromptParts {
            head: self.head.to_string(),
            tail,
            volatile_head: self.volatile.to_string(),
            volatile_tail,
        };
        if excess.is_empty() {
            Assembly::Ok(parts)
        } else {
            Assembly::SelectionFailure { parts, excess }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(path: &str, text: &str, fidelity: Fidelity, must: bool) -> ContextItem {
        ContextItem {
            key: ItemKey {
                path: path.to_string(),
                region: "current".to_string(),
                role: "reviewer".to_string(),
            },
            label: format!("--- {path}"),
            text: text.to_string(),
            fidelity,
            must_include: must,
        }
    }

    #[test]
    fn duplicate_keys_are_dropped_and_counted() {
        let mut asm = ContextAssembler::new("HEAD", 100_000);
        asm.add(item("src/a.rs", "impl A {}", Fidelity::Exact, true));
        asm.add(item("src/a.rs", "impl A {}", Fidelity::Exact, true));
        assert_eq!(asm.eliminated_chars(), "impl A {}".len());
        match asm.assemble() {
            Assembly::Ok(parts) => {
                assert_eq!(parts.head, "HEAD");
                assert_eq!(parts.tail.matches("impl A {}").count(), 1);
            }
            _ => panic!("two small items must fit"),
        }
    }

    #[test]
    fn the_same_file_in_two_regions_is_not_a_duplicate() {
        let mut asm = ContextAssembler::new("", 100_000);
        let mut top = item("src/a.rs", "pub mod a;", Fidelity::Exact, true);
        top.key.region = "head".to_string();
        let mut bottom = item("src/a.rs", "fn main() {}", Fidelity::Exact, true);
        bottom.key.region = "tail".to_string();
        asm.add(top);
        asm.add(bottom);
        match asm.assemble() {
            Assembly::Ok(parts) => {
                assert!(parts.tail.contains("pub mod a;"));
                assert!(parts.tail.contains("fn main() {}"));
            }
            _ => panic!("different regions are different items"),
        }
    }

    #[test]
    fn a_must_include_that_does_not_fit_is_excess_not_elided() {
        let mut asm = ContextAssembler::new("", 100);
        asm.add(item("big.rs", &"x".repeat(10_000), Fidelity::Exact, true));
        match asm.assemble() {
            Assembly::SelectionFailure { parts, excess } => {
                assert!(parts.tail.is_empty());
                assert_eq!(excess.len(), 1);
                assert_eq!(excess[0].key.path, "big.rs");
            }
            Assembly::Ok(_) => panic!("a 10k exact item cannot fit in 100 chars"),
        }
    }

    #[test]
    fn an_optional_item_is_dropped_when_full_but_fills_when_room_remains() {
        let mut asm = ContextAssembler::new("", 60);
        asm.add(item("big.rs", &"y".repeat(10_000), Fidelity::Drop, false));
        match asm.assemble() {
            Assembly::Ok(parts) => assert!(parts.tail.is_empty()),
            _ => panic!("an optional overflow is a drop, not a failure"),
        }
        let mut asm = ContextAssembler::new("", 100_000);
        asm.add(item("big.rs", "y", Fidelity::Drop, false));
        match asm.assemble() {
            Assembly::Ok(parts) => assert!(parts.tail.contains('y')),
            _ => panic!("room remained"),
        }
    }

    #[test]
    fn a_windowed_item_is_narrowed_until_it_fits() {
        // 5_000 chars, window cap 4_000, budget 3_000: one halving lands at
        // 2_000, which fits — so this is Ok, not a selection failure.
        let mut asm = ContextAssembler::new("", 3_000);
        asm.add(ContextItem {
            key: ItemKey {
                path: "src/a.rs".to_string(),
                region: "current".to_string(),
                role: "reviewer".to_string(),
            },
            label: "--- src/a.rs".to_string(),
            text: "fn the_anchor() {\n".repeat(300),
            fidelity: Fidelity::Windowed {
                anchor: "the_anchor".to_string(),
                cap: 4_000,
            },
            must_include: true,
        });
        match asm.assemble() {
            Assembly::Ok(parts) => {
                // The window centres on the anchor, so the elision marker is
                // present and the delivered text is far under the cap.
                assert!(parts.tail.contains("the_anchor"));
                assert!(parts.tail.len() < 3_000);
            }
            _ => panic!("a window must narrow to fit before it fails"),
        }
    }

    #[test]
    fn excess_items_do_not_consume_the_budget() {
        // A large exact failure must not starve a later small must_include.
        let mut asm = ContextAssembler::new("", 500);
        asm.add(item("big.rs", &"x".repeat(10_000), Fidelity::Exact, true));
        asm.add(item("small.rs", "ok", Fidelity::Exact, true));
        match asm.assemble() {
            Assembly::SelectionFailure { parts, excess } => {
                assert!(parts.tail.contains("ok"));
                assert_eq!(excess.len(), 1);
            }
            _ => panic!("the exact item cannot fit"),
        }
    }
}
