use crate::config::RetrievalConfig;
use std::path::{Path, PathBuf};

/// One retrieved file excerpt.
#[derive(Debug, Clone)]
pub struct Snippet {
    pub path: String,
    pub content: String,
}

/// v1 keyword retriever over local files (no embeddings).
/// Walks the workdir with guardrails (depth cap, skipped dirs, extension
/// allowlist), scores files by keyword overlap with the query, and packs
/// the top hits into a char budget. Feeds the mid-term context layer.
pub struct Retriever {
    root: PathBuf,
    cfg: RetrievalConfig,
}

/// Directories never worth retrieving from: build output, VCS state, and run
/// artifacts (a committed report is keyword-dense and crowds out real source).
const SKIP_DIRS: &[&str] = &["target", ".git", "node_modules", ".hg", ".svn", "baselines"];

/// Char cap for a file the goal names by path. Higher than the per-file
/// retrieval cap — the goal is asking for an edit to this file, so it gets
/// whole-file treatment up to a typical source file's size, and a window
/// centred on the named symbol beyond that.
/// The widest window given to a file a goal or an artifact names.
pub(crate) const NAMED_FILE_CAP: usize = 12_000;

/// Paths listed by `Retriever::file_map`. Enough to cover this repo's sources
/// with room over; the map is a lookup aid, not an inventory.
const FILE_MAP_CAP: usize = 150;

impl Retriever {
    pub fn new(root: PathBuf, cfg: RetrievalConfig) -> Self {
        Self { root, cfg }
    }

    pub fn retrieve(&self, query: &str, budget_chars: usize) -> Vec<Snippet> {
        let keys = keywords(query);
        let mut out: Vec<Snippet> = Vec::new();
        let mut used = 0usize;
        // Files the goal names by path go in first. Keyword density is a
        // popularity contest a big file wins, and the goal almost always
        // names the file it wants changed.
        let named = self.named_paths(query);
        for p in &named {
            if out.len() >= self.cfg.max_snippets {
                break;
            }
            if let Some(s) = self.snippet(p, query, budget_chars, &mut used) {
                out.push(s);
            }
        }
        if keys.is_empty() {
            return out;
        }
        let mut files = Vec::new();
        self.walk(&self.root.clone(), 0, &mut files);
        let mut scored: Vec<(usize, PathBuf, String)> = Vec::new();
        for f in files {
            if !self.ext_ok(&f) || named.contains(&f) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&f) else {
                continue;
            };
            if meta.len() > 256 * 1024 {
                continue;
            }
            let Ok(data) = std::fs::read(&f) else {
                continue;
            };
            let raw = String::from_utf8_lossy(&data).to_string();
            // A suite file carries the goal text verbatim: it matches every
            // keyword and spends the whole budget echoing the task back. Gated
            // on length so a short keyword query still matches real files.
            if query.trim().chars().count() >= 40 && raw.contains(query.trim()) {
                continue;
            }
            let text = raw.to_lowercase();
            let score: usize = keys.iter().map(|k| text.matches(k).count()).sum();
            if score > 0 {
                scored.push((score, f, raw));
            }
        }
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        let hs = hints(query);
        for (_, path, content) in scored.into_iter() {
            if out.len() >= self.cfg.max_snippets {
                break;
            }
            let body = excerpt(&content, self.cfg.max_bytes_per_file, &hs);
            if let Some(s) = self.pack(&path, &body, budget_chars, &mut used) {
                out.push(s);
            }
        }
        out
    }

    /// A file named in the query text, e.g. `src/engine/router.rs`.
    fn named_paths(&self, query: &str) -> Vec<PathBuf> {
        let mut v = Vec::new();
        for tok in query.split(|c: char| c.is_whitespace() || c == ',') {
            let t = tok
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '_' && c != '-');
            if t.len() < 4 || !self.ext_ok(Path::new(t)) {
                continue;
            }
            let p = self.root.join(t);
            if p.is_file() && !v.contains(&p) {
                v.push(p);
            }
        }
        v
    }

    fn snippet(
        &self,
        path: &Path,
        query: &str,
        budget_chars: usize,
        used: &mut usize,
    ) -> Option<Snippet> {
        let meta = std::fs::metadata(path).ok()?;
        if meta.len() > 256 * 1024 {
            return None;
        }
        let content = std::fs::read_to_string(path).ok()?;
        // A file the goal names is worth more context than a keyword hit: the
        // default per-file cap cut the anchor out of a 18 KB file and the model
        // then reported "anchor text not retrieved" instead of editing.
        let body = excerpt(&content, NAMED_FILE_CAP, &hints(query));
        self.pack(path, &body, budget_chars, used)
    }

    /// Packs an already-excerpted body: the char budget is the only cap left,
    /// because the caller has already applied the per-file one.
    fn pack(
        &self,
        path: &Path,
        content: &str,
        budget_chars: usize,
        used: &mut usize,
    ) -> Option<Snippet> {
        let head = rel(&self.root, path);
        let take = content.chars().count().min(
            budget_chars
                .saturating_sub(*used)
                .saturating_sub(head.len() + 16),
        );
        if take == 0 {
            return None;
        }
        *used += head.len() + take + 16;
        Some(Snippet {
            path: head,
            content: content.chars().take(take).collect(),
        })
    }

    /// A path map of the tree, one relative path per line. A caller that must
    /// *name* a file to read it uses this: the implementer guesses paths and
    /// guesses wrong (measured: 8 of 31 requested reads hit a path that does not
    /// exist — `src/obs.rs` for `src/obs/trace.rs`), and a map is ~2k chars that
    /// stay byte-stable for the life of a task, so it rides the cached prefix.
    pub fn file_map(&self) -> String {
        let mut files = Vec::new();
        self.walk(&self.root.clone(), 0, &mut files);
        files.retain(|f| self.ext_ok(f));
        files.sort();
        let mut out = String::new();
        for p in files.iter().take(FILE_MAP_CAP) {
            out.push_str(&rel(&self.root, p));
            out.push('\n');
        }
        if files.len() > FILE_MAP_CAP {
            out.push_str(&format!("[...{} more]\n", files.len() - FILE_MAP_CAP));
        }
        out
    }

    fn walk(&self, dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > self.cfg.max_depth || out.len() >= 500 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                self.walk(&p, depth + 1, out);
            } else {
                out.push(p);
            }
        }
    }

    fn ext_ok(&self, p: &Path) -> bool {
        match p.extension().and_then(|e| e.to_str()) {
            Some(ext) => self.cfg.extensions.iter().any(|a| a == ext),
            None => false,
        }
    }
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .map(|r| r.to_string_lossy().to_string())
        .unwrap_or_else(|_| p.to_string_lossy().to_string())
}

fn keywords(query: &str) -> Vec<String> {
    let mut v: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(str::to_string)
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Symbols the query names — backticked spans first, then identifier-shaped
/// words (`ProcRunTool`, `skips_hidden_files`). Used to centre the excerpt.
fn hints(query: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    let mut push = |w: &str| {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != ':');
        if w.len() >= 4 && !v.iter().any(|e| e == w) {
            v.push(w.to_string());
        }
    };
    for part in query.split('`').skip(1).step_by(2) {
        for w in part.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            push(w);
        }
    }
    for w in query.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        // Identifier-shaped: CamelCase or snake_case, so a capitalised English
        // word ("Change") cannot hijack the window. Char-indexed: `w[1..]`
        // panicked on a word starting with a multi-byte character (measured
        // live: "start byte index 1 is not a char boundary ... inside 'λ'").
        let camel = w.chars().skip(1).any(|c| c.is_ascii_uppercase());
        if w.contains('_') || camel {
            push(w);
        }
    }
    // Most specific first: `ProcRunTool` must win over the `struct` that
    // precedes it in the same backticked span, or the window lands on the
    // first struct in the file (measured: it landed on the file head).
    v.sort_by_key(|w| std::cmp::Reverse(w.len()));
    v
}

/// A window into `content` centred on a symbol named in `anchor`, for callers
/// outside retrieval that must put a file in front of a model — the fresh read
/// after a refused patch. Same rule as a named file: a head-only cap cuts the
/// anchor out of a large file, so centre on the *declaration*.
pub fn window_on(content: &str, anchor: &str) -> String {
    window_anchored(content, anchor, NAMED_FILE_CAP)
}

/// `cap` chars of a file centred on `anchor`. §4.1's assembler shapes evidence
/// and requested files through this rather than appending them whole: the
/// reviewer's 262 KB reads used to be head+tail-collapsed by the short layer's
/// budget, which can drop the only region the reviewer is judging.
pub(crate) fn window_anchored(content: &str, anchor: &str, cap: usize) -> String {
    excerpt(content, cap, &hints(anchor))
}

/// `cap` chars of a file. Head-only truncation cut the anchor out of the large
/// files a goal names (`ProcRunTool` sits at line 421 of 521), and the model
/// then guessed a patch anchor it had never seen — so centre the window on a
/// named symbol, and fall back to head+tail when the query names none.
fn excerpt(content: &str, cap: usize, hints: &[String]) -> String {
    let total = content.chars().count();
    if total <= cap {
        return content.to_string();
    }
    if let Some(pos) = hints.iter().find_map(|h| definition_pos(content, h)) {
        let at = content[..pos].chars().count();
        let start = at.saturating_sub(cap / 2);
        let win: String = content.chars().skip(start).take(cap).collect();
        return format!(
            "...[chars {start}..{} of {total}]...\n{win}",
            start + cap.min(total - start)
        );
    }
    let half = cap / 2;
    let head: String = content.chars().take(half).collect();
    let tail: String = content.chars().skip(total - half).collect();
    format!("{head}\n...[{} chars elided]...\n{tail}", total - cap)
}

/// Where `sym` is *declared* in the file, else where it is first mentioned.
/// A goal names a declaration ("`struct ProcRunTool`"), and the symbol is
/// usually mentioned far earlier than it is defined — anchoring on the first
/// mention put the window on the file head and the anchor was still absent.
fn definition_pos(content: &str, sym: &str) -> Option<usize> {
    const DECL: [&str; 8] = [
        "struct", "fn", "enum", "trait", "impl", "const", "static", "type",
    ];
    let mut first = None;
    let mut from = 0;
    while let Some(rel) = content[from..].find(sym) {
        let p = from + rel;
        first.get_or_insert(p);
        let start = p.saturating_sub(24);
        let start = (start..=p)
            .find(|i| content.is_char_boundary(*i))
            .unwrap_or(p);
        let pre = content[start..p].trim_end();
        if DECL.iter().any(|d| pre.ends_with(d)) {
            return Some(p);
        }
        from = p + sym.len();
        if from >= content.len() {
            break;
        }
    }
    first
}

/// Render snippets as a context section.
pub fn render(snips: &[Snippet]) -> String {
    if snips.is_empty() {
        return "retrieved: (none)".to_string();
    }
    let mut s = String::from("retrieved:");
    for sn in snips {
        s.push_str(&format!("\n--- {}\n{}", sn.path, sn.content));
    }
    s
}
