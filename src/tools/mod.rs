use crate::config::{PermissionPolicy, SkillsConfig};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub mod skills;
pub use skills::{SkillsListTool, SkillsManageTool, SkillsViewTool};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub ok: bool,
    pub output: String,
    pub error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("denied: {0}")]
    Denied(String),
    #[error("failed: {0}")]
    Failed(String),
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    async fn exec(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError>;
}

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    policy: PermissionPolicy,
}

impl ToolRegistry {
    pub fn new(policy: PermissionPolicy) -> Self {
        Self {
            tools: HashMap::new(),
            policy,
        }
    }

    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        self.tools.insert(tool.name().to_string(), Arc::new(tool));
    }

    /// The commands a `proc.run` may name. Used by the implementer's
    /// post-write test run: it only fires for a command the policy already
    /// grants, so a configuration that gives the implementer no shell is
    /// unchanged.
    pub fn allowed_commands(&self) -> Vec<String> {
        self.policy.allowed_commands.clone()
    }

    /// The full v1 toolset in one place, so the CLI and the eval runner
    /// cannot drift apart when a tool is added (live-bitten once already).
    /// The workdir is anchored here; every other grant comes from `policy`.
    ///
    /// The skills root is deliberately NOT added to `allowed_dirs`: the skill
    /// tools address skills by name and enforce their own containment, and
    /// putting the root on the allowlist would hand `fs.write`/`fs.patch` —
    /// which the implementer holds — a way around the skills write policy
    /// (Propose by default). One tool, one door.
    pub fn with_defaults(root: PathBuf, policy: PermissionPolicy, skills: SkillsConfig) -> Self {
        let mut p = policy;
        p.allowed_dirs = vec![root.clone()];
        let mut r = Self::new(p.clone());
        r.register(FsListTool::new(root.clone()));
        r.register(FsReadTool::new(root.clone()));
        r.register(FsPatchTool::new(root.clone()));
        r.register(FsWriteTool::new(root.clone()));
        r.register(ProcRunTool::new(root.clone(), p.allowed_commands.clone()));
        r.register(HttpGetTool::new(p.allowed_hosts.clone()));
        // A repo can ship skills with it (`<workdir>/skills/`), read-only.
        let extra_root = {
            let d = root.join("skills");
            d.is_dir().then_some(d)
        };
        let skill_root = skills
            .root
            .clone()
            .unwrap_or_else(crate::skills::SkillManager::default_root);
        let manager = Arc::new(crate::skills::SkillManager::new(
            skill_root,
            extra_root,
            skills.policy,
        ));
        r.register(SkillsListTool::new(manager.clone()));
        r.register(SkillsViewTool::new(manager.clone()));
        r.register(SkillsManageTool::new(manager));
        r
    }

    fn allowed(&self, agent: &str, tool: &str, path: Option<&Path>) -> Result<(), ToolError> {
        let allowed_tools = self
            .policy
            .agents_tools
            .get(agent)
            .ok_or_else(|| ToolError::Denied(format!("unknown agent {agent}")))?;
        if !allowed_tools.iter().any(|t| t == tool) {
            return Err(ToolError::Denied(format!("{agent} may not use {tool}")));
        }
        if let Some(p) = path {
            let inside = self.policy.allowed_dirs.iter().any(|d| under(d, p));
            if !inside {
                return Err(ToolError::Denied(format!(
                    "path {} outside allowlist",
                    p.to_string_lossy()
                )));
            }
        }
        Ok(())
    }

    /// Policy gate lives HERE so agents cannot bypass it.
    pub async fn call(
        &self,
        agent: &str,
        tool: &str,
        path: Option<&Path>,
        input: serde_json::Value,
    ) -> (Result<ToolOutput, ToolError>, u64) {
        let t = Instant::now();
        let res = match self.allowed(agent, tool, path) {
            Err(e) => Err(e),
            Ok(()) => match self.tools.get(tool) {
                None => Err(ToolError::Failed(format!("unknown tool {tool}"))),
                Some(rt) => rt.exec(input).await,
            },
        };
        (res, t.elapsed().as_millis() as u64)
    }
}

fn normalize(p: &Path) -> PathBuf {
    use std::path::Component::*;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Prefix(p) => out.push(p.as_os_str()),
            RootDir => out.push("/"),
            CurDir => {}
            ParentDir => {
                out.pop();
            }
            Normal(s) => out.push(s),
        }
    }
    out
}

// ponytail: lexical containment only, no symlink resolution — a link inside the
// root is followed out of it. Canonicalize the parent before a write.
// Component-wise starts_with: /allowed2 never matches /allowed, .. can't escape.
pub(crate) fn under(root: &Path, cand: &Path) -> bool {
    normalize(cand).starts_with(normalize(root))
}

fn resolve_under(root: &Path, rel: &str) -> Result<PathBuf, ToolError> {
    let p = root.join(rel);
    if !under(root, &p) {
        return Err(ToolError::Denied("path escapes tool root".to_string()));
    }
    // §4.2: `.git` is the tree-state substrate the write gate and rollback
    // read. An agent that reaches it could forge the gate's evidence or undo a
    // rollback, so no path naming it is readable or writable, at any depth.
    if p.components().any(|c| c.as_os_str() == ".git") {
        return Err(ToolError::Denied(
            "path is the git substrate (.git): harness-only".to_string(),
        ));
    }
    symlink_safe(root, &p)?;
    Ok(p)
}

/// §4.6: `under()` is lexical, so a symlink inside the root can point anywhere
/// — deny-by-default must hold for symlinks, not just spellings. Resolve the
/// containing directory (and a symlinked final name) against the canonical root.
/// Reads and writes both route here: a link out of the root leaks data one way
/// and corrupts it the other.
fn symlink_safe(root: &Path, target: &Path) -> Result<(), ToolError> {
    // The root may itself be a link; compare canonical to canonical.
    let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    // The root is the trust anchor: only what hangs under it gets resolved.
    if normalize(target) == normalize(root) {
        return Ok(());
    }
    let parent = target.parent().unwrap_or(root);
    // Writes create no parent dirs, so this exists for every real call; a
    // failure means a dangling link in the path, and that path is not writable.
    //
    // A missing parent is *not* a security denial: it is a recoverable
    // contract error, and reporting it as one made the model treat a fixable
    // write as permanently refused -- then claim success anyway. Say which.
    let canon = match std::fs::canonicalize(parent) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ToolError::Denied(format!(
                "parent directory does not exist: create {} first, then re-issue the write",
                parent.display()
            )))
        }
        Err(e) => return Err(ToolError::Denied(format!("path is not resolvable: {e}"))),
    };
    if !canon.starts_with(&canon_root) {
        return Err(ToolError::Denied(
            "symlink escapes the tool root".to_string(),
        ));
    }
    // A verified directory is not enough: writing through `/root/link -> /etc/x`
    // still lands in /etc.
    if std::fs::symlink_metadata(target)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        let link = std::fs::read_link(target)
            .map_err(|e| ToolError::Denied(format!("unreadable symlink: {e}")))?;
        let resolved = if link.is_absolute() {
            link
        } else {
            canon.join(link)
        };
        // Chains resolve fully when the target exists; a dangling link still
        // names where the write would land, so it is checked, not trusted.
        let real = std::fs::canonicalize(&resolved).unwrap_or(resolved);
        if !under(&canon_root, &real) {
            return Err(ToolError::Denied(
                "symlink escapes the tool root".to_string(),
            ));
        }
    }
    Ok(())
}

/// Safe tool 1: list files under root (read-only).
pub struct FsListTool {
    root: PathBuf,
}
impl FsListTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl Tool for FsListTool {
    fn name(&self) -> &'static str {
        "fs.list"
    }
    async fn exec(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let rel = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let dir = resolve_under(&self.root, rel)?;
        let mut names: Vec<serde_json::Value> = Vec::new();
        let rd = std::fs::read_dir(&dir).map_err(|e| ToolError::Failed(e.to_string()))?;
        for e in rd.flatten() {
            // The substrate stays invisible even in a listing: the gate denies
            // it, and a name the model cannot see is a name it cannot probe.
            if e.file_name() == ".git" {
                continue;
            }
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            names.push(serde_json::json!({
                "name": e.file_name().to_string_lossy().to_string(),
                "is_dir": is_dir,
            }));
        }
        names.sort_by_key(|a| a["name"].as_str().unwrap_or("").to_string());
        Ok(ToolOutput {
            ok: true,
            output: serde_json::to_string(&names).unwrap_or_default(),
            error: None,
        })
    }
}

/// Safe tool 2: read a file under root with a byte cap (read-only).
pub struct FsReadTool {
    root: PathBuf,
}
impl FsReadTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl Tool for FsReadTool {
    fn name(&self) -> &'static str {
        "fs.read"
    }
    async fn exec(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let rel = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Failed("missing path".to_string()))?;
        let cap = input
            .get("max_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(16_384) as usize;
        let p = resolve_under(&self.root, rel)?;
        let data = std::fs::read(&p).map_err(|e| ToolError::Failed(e.to_string()))?;
        let n = data.len().min(cap);
        Ok(ToolOutput {
            ok: true,
            output: String::from_utf8_lossy(&data[..n]).to_string(),
            error: if data.len() > cap {
                Some(format!("truncated {} -> {}", data.len(), cap))
            } else {
                None
            },
        })
    }
}

/// Patch tool: search/replace one hunk inside a file under root.
/// Whitespace-tolerant: exact match first, else both sides are compared with
/// whitespace runs collapsed to one space (a one-line edit then costs ~50
/// output tokens, not a ~1.1k whole-file rewrite). The hunk must match
/// exactly once or the call fails loudly — no silent partial edits.
pub struct FsPatchTool {
    root: PathBuf,
}
impl FsPatchTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

/// Collapse every run of ASCII whitespace to a single space, remembering for
/// each normalized byte the original byte index it came from (first byte of
/// the run), so a normalized match maps back to an original span.
fn normalize_ws(s: &str) -> (String, Vec<usize>) {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut map = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
            out.push(b' ');
            map.push(i);
            while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
                i += 1;
            }
        } else {
            out.push(c);
            map.push(i);
            i += 1;
        }
    }
    (String::from_utf8(out).unwrap_or_default(), map)
}

fn replace_span(original: &str, start: usize, end: usize, with: &str) -> String {
    let mut s = String::with_capacity(original.len() + with.len());
    s.push_str(&original[..start]);
    s.push_str(with);
    s.push_str(&original[end..]);
    s
}

/// Search/replace one hunk: exact unique match first, else a unique match on
/// whitespace-collapsed text. Returns the new text plus the replaced span.
/// Shared by `fs.patch` and the skill manager, so "a patch" means one thing in
/// this harness and a refused patch reads the same wherever it happened.
pub fn apply_hunk(
    original: &str,
    search: &str,
    replace: &str,
) -> Result<(String, usize, usize), String> {
    // Fast path: exact unique match.
    let exact: Vec<usize> = original.match_indices(search).map(|(i, _)| i).collect();
    let (start, end) = if exact.len() == 1 {
        let s = exact[0];
        (s, s + search.len())
    } else if !search.is_empty() && exact.is_empty() {
        // Tolerant path: match on whitespace-collapsed text.
        let (nfile, fmap) = normalize_ws(original);
        let (nsearch, _) = normalize_ws(search);
        if nsearch.is_empty() {
            return Err("search is only whitespace".to_string());
        }
        let hits: Vec<usize> = nfile.match_indices(&nsearch).map(|(i, _)| i).collect();
        if hits.is_empty() {
            return Err("search string not found".to_string());
        }
        if hits.len() > 1 {
            return Err(format!(
                "search matches {} spans, refusing ambiguous patch",
                hits.len()
            ));
        }
        let ns = hits[0];
        let ne = ns + nsearch.len();
        // Map back to the original span. fmap[i] is the original byte
        // where normalized byte i came from; if the match edge lands
        // inside a collapsed run, absorb the rest of that run (it ends
        // where the next normalized byte's source begins).
        let s = fmap[ns];
        let mut e = fmap[ne - 1] + 1;
        let run_end = if ne < fmap.len() {
            fmap[ne]
        } else {
            original.len()
        };
        while e < run_end
            && e < original.len()
            && matches!(original.as_bytes()[e], b' ' | b'\t' | b'\n' | b'\r')
        {
            e += 1;
        }
        (s, e)
    } else if exact.len() > 1 {
        return Err(format!(
            "search matches {} spans, refusing ambiguous patch",
            exact.len()
        ));
    } else {
        return Err("missing search".to_string());
    };
    Ok((replace_span(original, start, end, replace), start, end))
}

#[async_trait]
impl Tool for FsPatchTool {
    fn name(&self) -> &'static str {
        "fs.patch"
    }
    async fn exec(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let rel = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Failed("missing path".to_string()))?;
        let search = input
            .get("search")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Failed("missing search".to_string()))?;
        let replace = input
            .get("replace")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Failed("missing replace".to_string()))?;
        if replace.len() > 256 * 1024 {
            return Err(ToolError::Failed("replace over 256KB cap".to_string()));
        }
        let p = resolve_under(&self.root, rel)?;
        if p.is_dir() {
            return Err(ToolError::Failed(
                "refusing to patch a directory".to_string(),
            ));
        }
        let original = std::fs::read_to_string(&p).map_err(|e| ToolError::Failed(e.to_string()))?;
        if original.len() > 512 * 1024 {
            return Err(ToolError::Failed("file over 512KB cap".to_string()));
        }
        let (updated, start, end) =
            apply_hunk(&original, search, replace).map_err(ToolError::Failed)?;
        std::fs::write(&p, updated).map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(ToolOutput {
            ok: true,
            output: format!("patched {rel} (bytes {start}..{end})"),
            error: None,
        })
    }
}

/// HTTP GET with a host allowlist (deny-by-default like every other tool).
/// Redirects are NOT followed — a redirect would silently bypass the
/// allowlist — the scheme must be http(s), and the body is capped.
pub struct HttpGetTool {
    allowed_hosts: Vec<String>,
}
impl HttpGetTool {
    pub fn new(allowed_hosts: Vec<String>) -> Self {
        Self { allowed_hosts }
    }
}

#[async_trait]
impl Tool for HttpGetTool {
    fn name(&self) -> &'static str {
        "http.get"
    }
    async fn exec(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let raw = input
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Failed("missing url".to_string()))?;
        let url =
            reqwest::Url::parse(raw).map_err(|e| ToolError::Failed(format!("bad url: {e}")))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(ToolError::Denied(format!(
                "scheme {} not allowed",
                url.scheme()
            )));
        }
        let host = url
            .host_str()
            .ok_or_else(|| ToolError::Failed("url has no host".to_string()))?;
        // Exact, case-insensitive host match: no suffix or wildcard games.
        if !self
            .allowed_hosts
            .iter()
            .any(|h| h.trim().eq_ignore_ascii_case(host))
        {
            return Err(ToolError::Denied(format!("host not allowlisted: {host}")));
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        let res = client
            .get(url)
            .send()
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        let capped: String = body.chars().take(64_000).collect();
        let truncated = body.len() > capped.len();
        if !status.is_success() {
            return Err(ToolError::Failed(format!(
                "{status}: {}",
                capped.chars().take(200).collect::<String>()
            )));
        }
        Ok(ToolOutput {
            ok: true,
            output: capped,
            error: if truncated {
                Some("truncated to 64000 chars".to_string())
            } else {
                None
            },
        })
    }
}

/// Command runner: exact-allowlist only, no shell, runs in the tool root,
/// output capped, 300s ceiling so a hung build can't wedge the harness.
pub struct ProcRunTool {
    root: PathBuf,
    allowed: Vec<String>,
}
impl ProcRunTool {
    pub fn new(root: PathBuf, allowed: Vec<String>) -> Self {
        Self { root, allowed }
    }
}

#[async_trait]
impl Tool for ProcRunTool {
    fn name(&self) -> &'static str {
        "proc.run"
    }
    async fn exec(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let cmd = input
            .get("cmd")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Failed("missing cmd".to_string()))?;
        // Exact match: the caller cannot invent flags or chain a second command.
        if !self.allowed.iter().any(|a| a == cmd) {
            return Err(ToolError::Denied(format!("command not allowlisted: {cmd}")));
        }
        let mut parts = cmd.split_whitespace();
        let bin = parts
            .next()
            .ok_or_else(|| ToolError::Failed("empty cmd".to_string()))?;
        let fut = tokio::process::Command::new(bin)
            .args(parts)
            .current_dir(&self.root)
            .output();
        let out = tokio::time::timeout(std::time::Duration::from_secs(300), fut)
            .await
            .map_err(|_| ToolError::Failed("timeout after 300s".to_string()))?
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        let mut s = String::from_utf8_lossy(&out.stdout).to_string();
        let err = String::from_utf8_lossy(&out.stderr);
        if !err.is_empty() {
            s.push_str("\n[stderr]\n");
            s.push_str(&err);
        }
        let ok = out.status.success();
        Ok(ToolOutput {
            ok,
            output: s.chars().take(8_000).collect(),
            error: if ok {
                None
            } else {
                Some(format!("exit {}", out.status.code().unwrap_or(-1)))
            },
        })
    }
}

/// Write tool: overwrite-or-create one file under root, content-capped.
/// No mkdir, no append mode — the smallest surface that closes the loop.
/// Policy gate (agent grant + allowlist) applies like all tools.
pub struct FsWriteTool {
    root: PathBuf,
}
impl FsWriteTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl Tool for FsWriteTool {
    fn name(&self) -> &'static str {
        "fs.write"
    }
    async fn exec(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let rel = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Failed("missing path".to_string()))?;
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Failed("missing content".to_string()))?;
        if content.len() > 256 * 1024 {
            return Err(ToolError::Failed("content over 256KB cap".to_string()));
        }
        let p = resolve_under(&self.root, rel)?;
        if p.is_dir() {
            return Err(ToolError::Failed(
                "refusing to overwrite a directory".to_string(),
            ));
        }
        // ponytail: no mkdir; parent must exist. Flat writes beat recursive surprises.
        std::fs::write(&p, content).map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(ToolOutput {
            ok: true,
            output: format!("wrote {} bytes to {}", content.len(), rel),
            error: None,
        })
    }
}
