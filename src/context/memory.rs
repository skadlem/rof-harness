use std::path::Path;

/// Persistent memory v1: project conventions + user lessons, loaded into the
/// stable head. No vector store, no auto-write — the skills proposal flow stays
/// the only write path.
///
/// Learn mode adds a third section, the user-knowledge store
/// (`context::profile`, `~/.rof/PROFILE.md`). It rides here rather than in the
/// loops because this is the ONE place memory becomes context: both the
/// pipeline and the direct loop already call `load` then `render`, so a third
/// section needs no second wiring and cannot be added to one loop only.
#[derive(Debug, Clone, Default)]
pub struct Memory {
    pub project: String,
    pub user: String,
    /// The in-scope profile rows, already head-truncated to
    /// `profile::HEAD_CAP` and placed AFTER the project conventions, so a
    /// profile that grows without bound cannot displace `AGENTS.md`.
    pub profile: String,
}

const CAP: usize = 4000;

/// Load `<workdir>/AGENTS.md` (or lowercase) plus `~/.rof/LESSONS.md` and the
/// user-knowledge store. Missing files are empty strings; content is
/// head-truncated to CAP chars. The profile is scoped to `workdir` here —
/// `repo:<name>` rows for other repos are not read, so one project's internals
/// never reach another's context.
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
    // A missing or malformed profile is an empty store with a surfaced
    // reason (`context::profile`), never a failure: this function has no
    // error type and a run must not be takeable down by a file the user
    // edits by hand.
    let profile = crate::context::profile::load().head_section(workdir);
    Memory {
        project: head(&project),
        user: head(&user),
        profile,
    }
}

/// Render for the stable head. Empty sections are omitted so a task without
/// memory files is byte-identical to before. Order is fixed and load-bearing:
/// project conventions lead, so the profile — the only section a user can
/// append to freely — can never crowd them out.
pub fn render(m: &Memory) -> String {
    let mut out = String::new();
    if !m.project.trim().is_empty() {
        out.push_str(&format!("\n[PROJECT MEMORY]\n{}\n", m.project.trim()));
    }
    if !m.user.trim().is_empty() {
        out.push_str(&format!("\n[USER MEMORY]\n{}\n", m.user.trim()));
    }
    if !m.profile.trim().is_empty() {
        out.push_str(&format!("\n[USER PROFILE]\n{}\n", m.profile.trim()));
    }
    out
}

fn head(s: &str) -> String {
    s.chars().take(CAP).collect()
}
