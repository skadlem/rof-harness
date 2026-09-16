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

const SKIP_DIRS: &[&str] = &["target", ".git", "node_modules", ".hg", ".svn"];

impl Retriever {
    pub fn new(root: PathBuf, cfg: RetrievalConfig) -> Self {
        Self { root, cfg }
    }

    pub fn retrieve(&self, query: &str, budget_chars: usize) -> Vec<Snippet> {
        let keys = keywords(query);
        if keys.is_empty() {
            return Vec::new();
        }
        let mut files = Vec::new();
        self.walk(&self.root.clone(), 0, &mut files);
        let mut scored: Vec<(usize, PathBuf, String)> = Vec::new();
        for f in files {
            if !self.ext_ok(&f) {
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
            let text = String::from_utf8_lossy(&data).to_lowercase();
            let score: usize = keys.iter().map(|k| text.matches(k).count()).sum();
            if score > 0 {
                scored.push((score, f, String::from_utf8_lossy(&data).to_string()));
            }
        }
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        let mut out = Vec::new();
        let mut used = 0;
        for (_, path, content) in scored.into_iter().take(self.cfg.max_snippets) {
            let head = rel(&self.root, &path);
            let take = self
                .cfg
                .max_bytes_per_file
                .min(content.chars().count())
                .min(
                    budget_chars
                        .saturating_sub(used)
                        .saturating_sub(head.len() + 16),
                );
            if take == 0 {
                break;
            }
            used += head.len() + take + 16;
            out.push(Snippet {
                path: head,
                content: content.chars().take(take).collect(),
            });
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
