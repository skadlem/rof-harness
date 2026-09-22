use std::path::Path;

/// Persistent memory v1: project conventions + user lessons, loaded into the
/// stable head. No vector store, no auto-write — the skills proposal flow stays
/// the only write path.
#[derive(Debug, Clone, Default)]
pub struct Memory {
    pub project: String,
    pub user: String,
}

const CAP: usize = 4000;

/// Load `<workdir>/AGENTS.md` (or lowercase) plus `~/.rof/LESSONS.md`.
/// Missing files are empty strings; content is head-truncated to CAP chars.
pub fn load(workdir: &Path) -> Memory {
    let project = ["AGENTS.md", "agents.md", "CLAUDE.md"]
        .iter()
        .find_map(|n| std::fs::read_to_string(workdir.join(n)).ok())
        .unwrap_or_default();
    let user = std::env::var("HOME")
        .ok()
        .map(|h| Path::new(&h).join(".rof/LESSONS.md"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    Memory {
        project: head(&project),
        user: head(&user),
    }
}

/// Render for the stable head. Empty sections are omitted so a task without
/// memory files is byte-identical to before.
pub fn render(m: &Memory) -> String {
    let mut out = String::new();
    if !m.project.trim().is_empty() {
        out.push_str(&format!("\n[PROJECT MEMORY]\n{}\n", m.project.trim()));
    }
    if !m.user.trim().is_empty() {
        out.push_str(&format!("\n[USER MEMORY]\n{}\n", m.user.trim()));
    }
    out
}

fn head(s: &str) -> String {
    s.chars().take(CAP).collect()
}
