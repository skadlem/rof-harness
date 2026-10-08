use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Policy {
    pub root: PathBuf,
    pub allowed_commands: Vec<String>,
    pub allowed_prefixes: Vec<String>,
    /// Optional edit-gate argv prefix (e.g. `["python3", "-m", "py_compile"]`);
    /// `None` skips the check with zero behavior change.
    pub syntax_cmd: Option<Vec<String>>,
    /// Deny-globs matched against the root-relative path (e.g. `**/*.env`).
    pub denied_globs: Vec<String>,
    /// Extra env names forwarded to `exec`/`test` children on top of the
    /// fixed pass-through set. A provider key must never go here: names
    /// ending in `_API_KEY` (and malformed names) are dropped at spawn.
    pub pass_env: Vec<String>,
    /// Optional argv prefix prepended at spawn only (in-container eval:
    /// `"docker", "exec", <container>` so every exec/test tool call lands
    /// inside the container). The shell-op guard, the allowlist check, and
    /// the canonical join always see the UNWRAPPED argv; `None` (the
    /// default at every construction site) is zero behavior change.
    pub exec_wrap: Option<Vec<String>>,
}

/// The stock secrets deny-glob. Callers constructing `Policy` literally
/// should start here.
pub fn default_denied_globs() -> Vec<String> {
    vec![
        "**/.env".to_string(),
        "**/.env.*".to_string(),
        "**/*.pem".to_string(),
        "**/*.key".to_string(),
        "**/id_rsa*".to_string(),
        "**/.npmrc".to_string(),
        "**/.netrc".to_string(),
        "**/.aws".to_string(),
        "**/.aws/**".to_string(),
        "**/.ssh".to_string(),
        "**/.ssh/**".to_string(),
    ]
}

/// `.env.example` (and `.sample`/`.template`) are shareable templates, not
/// secrets: they bypass the `**/.env*` denials but nothing else.
fn is_env_template(rel: &str) -> bool {
    matches!(
        rel.rsplit('/').next().unwrap_or(""),
        ".env.example" | ".env.sample" | ".env.template"
    )
}

/// Hand-rolled glob: `*` matches any run without `/`, `**` any run with
/// `/`, and a leading `**/` also matches zero dirs (so `**/*.env` hits
/// root-level `.env` but never `.env.example`).
// ponytail: no glob crate for one pattern; add `glob` if patterns grow.
pub fn glob_match(pat: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        if p.is_empty() {
            return s.is_empty();
        }
        if p.len() >= 3 && p[0] == b'*' && p[1] == b'*' && p[2] == b'/' {
            if go(&p[3..], s) {
                return true;
            }
            let mut i = 0;
            while i < s.len() {
                while i < s.len() && s[i] != b'/' {
                    i += 1;
                }
                if i >= s.len() {
                    break;
                }
                i += 1;
                if go(&p[3..], &s[i..]) {
                    return true;
                }
            }
            return false;
        }
        if p[0] == b'*' {
            let mut q = 1;
            while q < p.len() && p[q] == b'*' {
                q += 1;
            }
            let rest = &p[q..];
            if rest.is_empty() {
                // `**` matches all; single `*` stops at `/`.
                return q >= 2 || !s.contains(&b'/');
            }
            let mut i = 0;
            loop {
                if go(rest, &s[i..]) {
                    return true;
                }
                if i >= s.len() || s[i] == b'/' {
                    return false;
                }
                i += 1;
            }
        }
        if s.is_empty() || p[0] != s[0] {
            return false;
        }
        go(&p[1..], &s[1..])
    }
    go(pat.as_bytes(), path.as_bytes())
}

#[derive(Debug, Clone)]
pub enum ToolPathError {
    Denied(String),
    MissingParent(String),
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s),
        }
    }
    out
}

fn under(root: &Path, cand: &Path) -> bool {
    normalize(cand).starts_with(normalize(root))
}

/// Containment: lexical check, secrets deny-glob, `.git` denied on writes
/// (reads may traverse it, still root-anchored + symlink-safe), then
/// canonical parent + symlinked-final check. A missing parent is
/// `MissingParent` (recoverable: "create it first, re-issue"), never a denial.
/// Same-inode hardlinks with a clean spelling are not detectable by path
/// checks; the write TOCTOU note in `write.rs` applies here too.
pub fn resolve_under(
    root: &Path,
    path: &str,
    write: bool,
    denied_globs: &[String],
) -> Result<PathBuf, ToolPathError> {
    let p = root.join(path);
    if !under(root, &p) {
        return Err(ToolPathError::Denied("path escapes tool root".to_string()));
    }
    let rel = normalize(&p)
        .strip_prefix(normalize(root))
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let rel = rel.trim_start_matches('/');
    for g in denied_globs {
        if g.starts_with("**/.env") && is_env_template(rel) {
            continue;
        }
        if glob_match(g, rel) {
            return Err(ToolPathError::Denied(format!(
                "path matches denied glob: {rel}"
            )));
        }
    }
    if write && p.components().any(|c| c.as_os_str() == ".git") {
        return Err(ToolPathError::Denied(
            "path is the git substrate (.git): harness-only".to_string(),
        ));
    }
    symlink_safe(root, &p, write)?;
    Ok(p)
}

fn git_component_hit(canon_root: &Path, canon: &Path) -> bool {
    canon
        .strip_prefix(canon_root)
        .map(|rel| rel.components().any(|c| c.as_os_str() == ".git"))
        .unwrap_or(false)
}

pub(crate) fn symlink_safe(root: &Path, target: &Path, write: bool) -> Result<(), ToolPathError> {
    let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if normalize(target) == normalize(root) {
        return Ok(());
    }
    let parent = target.parent().unwrap_or(root);
    let canon = match std::fs::canonicalize(parent) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ToolPathError::MissingParent(format!(
                "parent directory does not exist: create {} first, then re-issue",
                parent.display()
            )));
        }
        Err(e) => {
            return Err(ToolPathError::Denied(format!(
                "path is not resolvable: {e}"
            )));
        }
    };
    if !canon.starts_with(&canon_root) {
        return Err(ToolPathError::Denied(
            "symlink escapes the tool root".to_string(),
        ));
    }
    if write && git_component_hit(&canon_root, &canon) {
        // The lexical `.git` check above is blind to symlinks: a link to
        // the git substrate anywhere in the resolved parent is the same
        // substrate, denied on writes all the same.
        return Err(ToolPathError::Denied(
            "path is the git substrate (.git): harness-only".to_string(),
        ));
    }
    if std::fs::symlink_metadata(target)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        let link = std::fs::read_link(target)
            .map_err(|e| ToolPathError::Denied(format!("unreadable symlink: {e}")))?;
        let resolved = if link.is_absolute() {
            link
        } else {
            canon.join(link)
        };
        let real = std::fs::canonicalize(&resolved).unwrap_or(resolved);
        if !under(&canon_root, &real) {
            return Err(ToolPathError::Denied(
                "symlink escapes the tool root".to_string(),
            ));
        }
        if write && git_component_hit(&canon_root, &real) {
            // A final-component link into the substrate (e.g. `evil` ->
            // `.git/HEAD`): the parent check above sees the link's dir,
            // not its target, so check the resolved target too.
            return Err(ToolPathError::Denied(
                "path is the git substrate (.git): harness-only".to_string(),
            ));
        }
    }
    Ok(())
}
