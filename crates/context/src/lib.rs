//! Budgeted prompt selection. Salvage of ~/rof-harness/src/context/assembler.rs
//! (one budget, dedupe, windowing) + retriever.rs (file map, named files, windows).
//! Rule: select what enters, never summarize the edit surface. Volatile named
//! files are must_include and excluded from mid-layer double delivery: the
//! caller keeps them out of the mid layer, this crate delivers them last.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// Below this many chars a window is too narrow to judge, so a `must_include`
/// windowed item that still does not fit after halvings is emitted at this
/// floor rather than silently cut.
pub const MIN_WINDOW: usize = 2_000;

/// Old observations kept verbatim (SWE-agent collapse-5, +3.0pp over full history).
pub const COLLAPSE_KEEP: usize = 5;

/// Active file window in lines (SWE-agent: 30 lines −3.7pp, full file −5.3pp).
pub const WINDOW_LINES: usize = 100;

const SKIP_DIRS: &[&str] = &["target", ".git", "node_modules", ".hg", ".svn", "baselines"];

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemKey {
    pub path: String,
    pub region: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Fidelity {
    Exact,
    Windowed { anchor: String, cap: usize },
    Drop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextItem {
    pub key: ItemKey,
    pub fidelity: Fidelity,
    pub must_include: bool,
    pub est_chars: usize,
    pub text: String,
}

pub struct ContextAssembler {
    budget: usize,
    items: Vec<ContextItem>,
    volatile_items: Vec<ContextItem>,
    seen: HashSet<ItemKey>,
    eliminated: usize,
}

impl ContextAssembler {
    pub fn new(_budget_chars: usize) -> Self {
        Self {
            budget: _budget_chars,
            items: Vec::new(),
            volatile_items: Vec::new(),
            seen: HashSet::new(),
            eliminated: 0,
        }
    }

    /// Dedupe by key; counts eliminated chars. Volatile items bypass the mid layer.
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
            self.eliminated += item.text.chars().count();
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
        self.assemble_inner(self.budget, false)
    }

    /// Diet-first reask split for the retry turn: volatiles are fitted first so a
    /// tight budget starves background before evidence. Delivery order is
    /// unchanged, so bytes equal `assemble` whenever everything fits.
    pub fn assemble_reask(&self, _budget_chars: usize) -> String {
        self.assemble_inner(_budget_chars, true)
    }

    pub fn eliminated_chars(&self) -> usize {
        self.eliminated
    }

    fn shape(&self, item: &ContextItem, remaining: usize) -> Option<String> {
        match &item.fidelity {
            Fidelity::Exact | Fidelity::Drop => {
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

    fn assemble_inner(&self, budget: usize, volatiles_first: bool) -> String {
        let mut used = 0usize;
        let mut stable = vec![None; self.items.len()];
        let mut vol = vec![None; self.volatile_items.len()];
        if volatiles_first {
            self.fit_list(&self.volatile_items, &mut vol, &mut used, budget);
            self.fit_list(&self.items, &mut stable, &mut used, budget);
        } else {
            self.fit_list(&self.items, &mut stable, &mut used, budget);
            self.fit_list(&self.volatile_items, &mut vol, &mut used, budget);
        }
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

/// Byte-stable capped path listing for the file map (rides the cached prefix).
/// Sorted, dotfiles and build/VCS dirs skipped, truncated to `max_paths`.
pub fn file_map(_root: &Path, _max_paths: usize) -> Vec<String> {
    let mut v = Vec::new();
    walk(_root, _root, 0, &mut v);
    v.sort();
    v.truncate(_max_paths);
    v
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 8 || out.len() >= 2000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk(root, &p, depth + 1, out);
        } else if let Ok(rel) = p.strip_prefix(root) {
            out.push(rel.to_string_lossy().to_string());
        }
    }
}

/// Whole named file up to cap, never summarized. Caller excludes it from mid.
pub fn named_file_contents(
    _root: &Path,
    _path: &str,
    _cap_chars: usize,
) -> std::io::Result<String> {
    let c = std::fs::read_to_string(_root.join(_path))?;
    Ok(cut_chars(&c, _cap_chars).to_owned())
}

/// Window text on an anchor line with head+tail around it: `cap` chars centred
/// on the anchor substring, head+tail with an elision marker when absent.
pub fn window_anchored(_text: &str, _anchor: &str, _cap_chars: usize) -> String {
    let total = _text.chars().count();
    if total <= _cap_chars {
        return _text.to_string();
    }
    if !_anchor.is_empty() {
        if let Some(pos) = _text.find(_anchor) {
            let at = _text[..pos].chars().count();
            let start = at.saturating_sub(_cap_chars / 2);
            let win: String = _text.chars().skip(start).take(_cap_chars).collect();
            let end = start + _cap_chars.min(total - start);
            return format!("...[chars {start}..{end} of {total}]...\n{win}");
        }
    }
    let half = _cap_chars / 2;
    let head: String = _text.chars().take(half).collect();
    let tail: String = _text.chars().skip(total - half).collect();
    format!(
        "{head}\n...[{} chars elided]...\n{tail}",
        total - _cap_chars
    )
}

/// Char-boundary cut. Never splits a UTF-8 sequence (v1 context_edges lesson).
pub fn cut_chars(_text: &str, _max_chars: usize) -> &str {
    match _text.char_indices().nth(_max_chars) {
        Some((i, _)) => &_text[..i],
        None => _text,
    }
}

/// Collapse-5: all but the last `COLLAPSE_KEEP` observations collapse to one
/// line each; the tail stays verbatim.
pub fn collapse_history(observations: &[String]) -> String {
    if observations.len() <= COLLAPSE_KEEP {
        return observations.join("\n");
    }
    let split = observations.len() - COLLAPSE_KEEP;
    let mut s = String::new();
    for o in &observations[..split] {
        s.push_str("[collapsed] ");
        s.push_str(o.lines().next().unwrap_or(""));
        s.push('\n');
    }
    s.push_str(&observations[split..].join("\n"));
    s
}

/// 100-line file window centred on `center_line` (0-indexed).
pub fn file_window(text: &str, center_line: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= WINDOW_LINES {
        return text.to_string();
    }
    let start = center_line
        .saturating_sub(WINDOW_LINES / 2)
        .min(lines.len() - WINDOW_LINES);
    lines[start..start + WINDOW_LINES].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(path: &str, region: &str, text: &str, fidelity: Fidelity, must: bool) -> ContextItem {
        ContextItem {
            key: ItemKey {
                path: path.into(),
                region: region.into(),
                role: "t".into(),
            },
            fidelity,
            must_include: must,
            est_chars: text.chars().count(),
            text: text.into(),
        }
    }

    #[test]
    fn dedupe_by_key_counts_eliminated() {
        let mut a = ContextAssembler::new(100_000);
        a.add(mk("s/a.rs", "cur", "impl A {}", Fidelity::Exact, true));
        a.add(mk("s/a.rs", "cur", "impl A {}", Fidelity::Exact, true));
        assert_eq!(a.eliminated_chars(), "impl A {}".chars().count());
        assert_eq!(a.assemble().matches("impl A {}").count(), 1);
    }

    #[test]
    fn same_path_two_regions_is_not_a_duplicate() {
        let mut a = ContextAssembler::new(100_000);
        a.add(mk("s/a.rs", "head", "pub mod a;", Fidelity::Exact, true));
        a.add(mk("s/a.rs", "tail", "fn main() {}", Fidelity::Exact, true));
        let out = a.assemble();
        assert!(out.contains("pub mod a;") && out.contains("fn main() {}"));
    }

    #[test]
    fn must_include_survives_tiny_budget() {
        let mut a = ContextAssembler::new(10);
        a.add(mk(
            "big.rs",
            "cur",
            &"x".repeat(5000),
            Fidelity::Exact,
            true,
        ));
        a.add(mk(
            "opt.rs",
            "cur",
            &"y".repeat(5000),
            Fidelity::Drop,
            false,
        ));
        let out = a.assemble();
        assert!(out.contains('x') && !out.contains('y'));
    }

    #[test]
    fn windowed_narrows_by_halving_until_it_fits() {
        let mut a = ContextAssembler::new(3000);
        a.add(mk(
            "s/a.rs",
            "cur",
            &"fn anchor_sym() {}\n".repeat(300),
            Fidelity::Windowed {
                anchor: "anchor_sym".into(),
                cap: 4000,
            },
            true,
        ));
        let out = a.assemble();
        assert!(out.contains("anchor_sym"));
        assert!(out.chars().count() < 3000);
    }

    #[test]
    fn oversized_must_include_narrows_to_floor_never_silent_cut() {
        let mut a = ContextAssembler::new(10);
        a.add(mk(
            "s/a.rs",
            "cur",
            &"fn anchor_sym() {}\n".repeat(600),
            Fidelity::Windowed {
                anchor: "anchor_sym".into(),
                cap: 8000,
            },
            true,
        ));
        let out = a.assemble();
        assert!(out.contains("anchor_sym"));
        assert!(out.chars().count() <= MIN_WINDOW + 200);
    }

    #[test]
    fn reask_fits_volatiles_first_but_delivers_stable_order() {
        let mut a = ContextAssembler::new(100_000);
        a.add(mk("bg.rs", "cur", "background", Fidelity::Drop, false));
        a.add_volatile(mk("ev.rs", "cur", "evidence", Fidelity::Exact, true));
        assert_eq!(a.assemble(), a.assemble_reask(100_000));
        let diet = a.assemble_reask("--- ev.rs\nevidence".chars().count() + 4);
        assert!(diet.contains("evidence") && !diet.contains("background"));
    }

    #[test]
    fn window_anchored_on_symbol() {
        let text = "a\n".repeat(3000) + "fn target_sym() {}\n" + &"b\n".repeat(3000);
        let w = window_anchored(&text, "target_sym", 2000);
        assert!(w.contains("target_sym") && w.chars().count() <= 2000 + 100);
        let w2 = window_anchored(&text, "absent", 2000);
        assert!(w2.contains("elided"));
    }

    #[test]
    fn cut_chars_respects_multibyte_boundaries() {
        let t = "λ".repeat(10);
        let c = cut_chars(&t, 4);
        assert_eq!(c.chars().count(), 4);
        assert_eq!(c, "λλλλ");
        assert_eq!(cut_chars("abc", 99), "abc");
    }

    #[test]
    fn named_file_cap_returns_whole_small_file_and_cuts_big() {
        let dir = std::env::temp_dir().join(format!("ctx-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("small.rs"), "fn f() {}").unwrap();
        std::fs::write(dir.join("big.rs"), "λ".repeat(5000)).unwrap();
        assert_eq!(
            named_file_contents(&dir, "small.rs", 2000).unwrap(),
            "fn f() {}"
        );
        let big = named_file_contents(&dir, "big.rs", 2000).unwrap();
        assert_eq!(big.chars().count(), 2000);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn collapse_keeps_last_five_verbatim() {
        let obs: Vec<String> = (0..7).map(|i| format!("obs{i}\nsecond line {i}")).collect();
        let out = collapse_history(&obs);
        assert!(out.contains("[collapsed] obs0") && out.contains("[collapsed] obs1"));
        assert!(out.contains("second line 6") && !out.contains("second line 0"));
    }

    #[test]
    fn file_window_is_100_lines_centred() {
        let text = (0..250)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let w = file_window(&text, 200);
        assert_eq!(w.lines().count(), WINDOW_LINES);
        assert!(w.contains("line 200") && !w.contains("line 0\n"));
    }
}
