//! The 6 built-in tools + containment. Implements [`tool_core::Tool`].
//!
//! Schemas are hand-built `serde_json` values kept inside tool-core's strict
//! subset (`required`, `type`, `additionalProperties: false`, `maxLength`);
//! each tool also parses into a typed `Deserialize` args struct with
//! `deny_unknown_fields`, so schema and parser pin the same contract.
//! (ponytail: no `schemars` dep — five `json!` literals use the already-installed
//! `serde_json`; add `schemars` only if schemas start drifting from structs.)

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tool_core::{CallStatus, Invocation, Tool, ToolCall, ToolDefinition, ToolError, ToolOutcome};
use verify::condense_output;

const VIEW_CAP: usize = 16_384;
const OUT_CAP: usize = 8_000;
const EDIT_FILE_CAP: usize = 512 * 1024;
const EDIT_REPLACE_CAP: usize = 256 * 1024;
const EXEC_TIMEOUT: Duration = Duration::from_secs(300);

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
}

/// The stock secrets deny-glob. Callers constructing `Policy` literally
/// should start here.
pub fn default_denied_globs() -> Vec<String> {
    vec!["**/*.env".to_string()]
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
    if denied_globs.iter().any(|g| glob_match(g, rel)) {
        return Err(ToolPathError::Denied(format!(
            "path matches denied glob: {rel}"
        )));
    }
    if write && p.components().any(|c| c.as_os_str() == ".git") {
        return Err(ToolPathError::Denied(
            "path is the git substrate (.git): harness-only".to_string(),
        ));
    }
    symlink_safe(root, &p)?;
    Ok(p)
}

fn symlink_safe(root: &Path, target: &Path) -> Result<(), ToolPathError> {
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
    }
    Ok(())
}

fn collapse_ws(s: &str) -> (String, Vec<usize>) {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut map = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
            out.push(b' ');
            map.push(i);
            while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
                i += 1;
            }
        } else {
            out.push(b[i]);
            map.push(i);
            i += 1;
        }
    }
    (String::from_utf8(out).unwrap_or_default(), map)
}

/// Exact match first, else one whitespace-collapsed match; must hit exactly
/// once or fail loudly — no silent partial edits.
pub fn apply_hunk(text: &str, search: &str, replace: &str) -> Result<String, String> {
    let exact: Vec<usize> = text.match_indices(search).map(|(i, _)| i).collect();
    let (start, end) = if exact.len() == 1 {
        let s = exact[0];
        (s, s + search.len())
    } else if exact.len() > 1 {
        return Err(format!(
            "search matches {} spans, refusing ambiguous patch",
            exact.len()
        ));
    } else if search.is_empty() {
        return Err("missing search".to_string());
    } else {
        let (ntext, tmap) = collapse_ws(text);
        let (nsearch, _) = collapse_ws(search);
        if nsearch.is_empty() {
            return Err("search is only whitespace".to_string());
        }
        let hits: Vec<usize> = ntext.match_indices(&nsearch).map(|(i, _)| i).collect();
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
        let s = tmap[ns];
        let mut e = tmap[ne - 1] + 1;
        let run_end = if ne < tmap.len() {
            tmap[ne]
        } else {
            text.len()
        };
        while e < run_end
            && e < text.len()
            && matches!(text.as_bytes()[e], b' ' | b'\t' | b'\n' | b'\r')
        {
            e += 1;
        }
        (s, e)
    };
    let mut s = String::with_capacity(text.len() + replace.len());
    s.push_str(&text[..start]);
    s.push_str(replace);
    s.push_str(&text[end..]);
    Ok(s)
}

/// Exact match or `prefix + " "` boundary match (`cargo test` covers
/// `cargo test foo`, never `cargo test-evil`).
pub fn prefix_allowed(prefixes: &[String], cmd: &str) -> bool {
    prefixes.iter().any(|p| {
        let p = p.trim();
        !p.is_empty() && (cmd == p || cmd.starts_with(&format!("{p} ")))
    })
}

fn cap_chars(s: String, n: usize) -> (String, bool) {
    if s.chars().count() <= n {
        (s, false)
    } else {
        (s.chars().take(n).collect(), true)
    }
}

fn path_err(e: ToolPathError) -> ToolError {
    match e {
        ToolPathError::Denied(m) => ToolError::Denied(m),
        // Recoverable contract error: Failed (retryable), never a denial.
        ToolPathError::MissingParent(m) => ToolError::Failed(m),
    }
}

fn parse_args<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T, ToolError> {
    serde_json::from_value(args.clone()).map_err(|e| ToolError::Failed(format!("bad args: {e}")))
}

fn dispatch(call: &ToolCall) -> CallStatus {
    CallStatus::Dispatch(Invocation {
        call_id: call.call_id.clone(),
        name: call.name.clone(),
        args: call.args.clone(),
    })
}

// --- view ---

pub struct ViewTool {
    policy: Arc<Policy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewArgs {
    path: String,
    #[serde(default)]
    max_bytes: Option<u64>,
}

fn view_schema() -> Value {
    json!({
        "type": "object",
        "required": ["path"],
        "additionalProperties": false,
        "properties": {
            "path": {"type": "string", "maxLength": 4096},
            "max_bytes": {"type": "integer"}
        }
    })
}

#[async_trait]
impl Tool for ViewTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "view".to_string(),
            description: "read a file under the tool root, byte-capped".to_string(),
            schema: view_schema(),
        }
    }
    fn prepare(&self, call: &ToolCall) -> CallStatus {
        dispatch(call)
    }
    async fn execute(
        &self,
        inv: Invocation,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let args: ViewArgs = parse_args(&inv.args)?;
        let p = resolve_under(
            &self.policy.root,
            &args.path,
            false,
            &self.policy.denied_globs,
        )
        .map_err(path_err)?;
        let data = std::fs::read(&p).map_err(|e| ToolError::Failed(e.to_string()))?;
        let cap = args
            .max_bytes
            .unwrap_or(VIEW_CAP as u64)
            .min(usize::MAX as u64) as usize;
        let n = data.len().min(cap);
        let (content, truncated) =
            cap_chars(String::from_utf8_lossy(&data[..n]).to_string(), OUT_CAP);
        Ok(ToolOutcome {
            truncated: truncated || data.len() > n,
            content,
        })
    }
}

// --- search ---

pub struct SearchTool {
    policy: Arc<Policy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    max_results: Option<u64>,
}

fn search_schema() -> Value {
    json!({
        "type": "object",
        "required": ["pattern"],
        "additionalProperties": false,
        "properties": {
            "pattern": {"type": "string", "maxLength": 4096},
            "path": {"type": "string", "maxLength": 4096},
            "max_results": {"type": "integer"}
        }
    })
}

fn search_dir(dir: &Path, root: &Path, pat: &str, hits: &mut Vec<String>, limit: usize) {
    if hits.len() >= limit {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if hits.len() >= limit {
            break;
        }
        if e.file_name() == ".git" {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() {
            search_dir(&p, root, pat, hits, limit);
        } else if ft.is_file() {
            search_file(&p, root, pat, hits, limit);
        }
    }
}

/// Collect `pat` hits from one file as `rel:line: text`, stopping at `limit`.
/// A direct file search must not share `search_dir`'s sibling scan: siblings
/// would fill `limit` first and silently drop this file's lines.
/// ponytail: literal contains only, no regex; add regex when a task needs it.
fn search_file(p: &Path, root: &Path, pat: &str, hits: &mut Vec<String>, limit: usize) {
    let Ok(data) = std::fs::read(p) else {
        return;
    };
    let rel = p
        .strip_prefix(root)
        .map(|r| r.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_string_lossy().into_owned());
    for (i, line) in String::from_utf8_lossy(&data).lines().enumerate() {
        if line.contains(pat) {
            hits.push(format!("{}:{}: {}", rel, i + 1, line));
            if hits.len() >= limit {
                break;
            }
        }
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "search".to_string(),
            description: "literal substring search over files under the tool root".to_string(),
            schema: search_schema(),
        }
    }
    fn prepare(&self, call: &ToolCall) -> CallStatus {
        dispatch(call)
    }
    async fn execute(
        &self,
        inv: Invocation,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let args: SearchArgs = parse_args(&inv.args)?;
        let base = resolve_under(
            &self.policy.root,
            args.path.as_deref().unwrap_or("."),
            false,
            &self.policy.denied_globs,
        )
        .map_err(path_err)?;
        let limit = args.max_results.unwrap_or(50).clamp(1, 200) as usize;
        let mut hits = Vec::new();
        if base.is_file() {
            search_file(&base, &self.policy.root, &args.pattern, &mut hits, limit);
        } else {
            search_dir(&base, &self.policy.root, &args.pattern, &mut hits, limit);
        }
        let (content, truncated) = cap_chars(hits.join("\n"), OUT_CAP);
        Ok(ToolOutcome { content, truncated })
    }
}

// --- edit ---

pub struct EditTool {
    policy: Arc<Policy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    search: String,
    replace: String,
}

fn edit_schema() -> Value {
    json!({
        "type": "object",
        "required": ["path", "search", "replace"],
        "additionalProperties": false,
        "properties": {
            "path": {"type": "string", "maxLength": 4096},
            "search": {"type": "string", "maxLength": 65536},
            "replace": {"type": "string", "maxLength": 262144}
        }
    })
}

#[async_trait]
impl Tool for EditTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit".to_string(),
            description: "replace one hunk in a file; match must be unique".to_string(),
            schema: edit_schema(),
        }
    }
    fn prepare(&self, call: &ToolCall) -> CallStatus {
        dispatch(call)
    }
    async fn execute(
        &self,
        inv: Invocation,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let args: EditArgs = parse_args(&inv.args)?;
        if args.replace.len() > EDIT_REPLACE_CAP {
            return Err(ToolError::Failed("replace over 256KB cap".to_string()));
        }
        let p = resolve_under(
            &self.policy.root,
            &args.path,
            true,
            &self.policy.denied_globs,
        )
        .map_err(path_err)?;
        if p.is_dir() {
            return Err(ToolError::Failed(
                "refusing to patch a directory".to_string(),
            ));
        }
        let original = std::fs::read_to_string(&p).map_err(|e| ToolError::Failed(e.to_string()))?;
        if original.len() > EDIT_FILE_CAP {
            return Err(ToolError::Failed("file over 512KB cap".to_string()));
        }
        let updated =
            apply_hunk(&original, &args.search, &args.replace).map_err(ToolError::Failed)?;
        if let Some(argv) = self.policy.syntax_cmd.clone() {
            if !argv.is_empty() {
                check_syntax(&self.policy, &updated).await?;
            }
        }
        std::fs::write(&p, updated).map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(ToolOutcome {
            content: format!("patched {}", args.path),
            truncated: false,
        })
    }
}

// --- write ---

pub struct WriteTool {
    policy: Arc<Policy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
}

fn write_schema() -> Value {
    json!({
        "type": "object",
        "required": ["path", "content"],
        "additionalProperties": false,
        "properties": {
            "path": {"type": "string", "maxLength": 4096},
            "content": {"type": "string", "maxLength": 262144}
        }
    })
}

/// Create missing parents one level at a time, re-jailing each level before
/// descending: `create_dir_all` would happily mkdir through a symlink that
/// points out of the root.
fn create_parent_dirs(root: &Path, path: &str) -> Result<(), ToolError> {
    let mut cur = root.to_path_buf();
    for c in Path::new(path)
        .parent()
        .unwrap_or(Path::new(""))
        .components()
    {
        cur.push(c);
        if cur.symlink_metadata().is_err() {
            std::fs::create_dir(&cur)
                .map_err(|e| ToolError::Failed(format!("cannot create {}: {e}", cur.display())))?;
        }
        symlink_safe(root, &cur).map_err(path_err)?;
    }
    Ok(())
}

fn resolve_for_write(policy: &Policy, path: &str) -> Result<PathBuf, ToolError> {
    match resolve_under(&policy.root, path, true, &policy.denied_globs) {
        Err(ToolPathError::MissingParent(_)) => {
            create_parent_dirs(&policy.root, path)?;
            resolve_under(&policy.root, path, true, &policy.denied_globs).map_err(path_err)
        }
        other => other.map_err(path_err),
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write".to_string(),
            description: "create or overwrite a whole file".to_string(),
            schema: write_schema(),
        }
    }
    fn prepare(&self, call: &ToolCall) -> CallStatus {
        dispatch(call)
    }
    async fn execute(
        &self,
        inv: Invocation,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let args: WriteArgs = parse_args(&inv.args)?;
        if args.content.len() > EDIT_REPLACE_CAP {
            return Err(ToolError::Failed("content over 256KB cap".to_string()));
        }
        let p = resolve_for_write(&self.policy, &args.path)?;
        if p.is_dir() {
            return Err(ToolError::Failed(
                "refusing to write a directory".to_string(),
            ));
        }
        // Same guard as edit: never clobber a big file with a small stub.
        if std::fs::metadata(&p)
            .map(|m| m.len() > EDIT_FILE_CAP as u64)
            .unwrap_or(false)
        {
            return Err(ToolError::Failed("file over 512KB cap".to_string()));
        }
        if let Some(argv) = self.policy.syntax_cmd.clone() {
            if !argv.is_empty() {
                check_syntax(&self.policy, &args.content).await?;
            }
        }
        std::fs::write(&p, &args.content).map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(ToolOutcome {
            content: format!("wrote {}", args.path),
            truncated: false,
        })
    }
}

// --- exec / test share the runner: exact-or-prefix allowlist, no shell ---

/// Split a command into argv honoring single/double quotes and backslash
/// escapes (no shell: `;`, `$()`, `&&`, `|` stay literal characters). Naive
/// whitespace splitting mangles quoted args (`python3 -c "print(x)"` broke).
fn split_cmd(cmd: &str) -> Vec<String> {
    let (mut parts, mut cur, mut quote) = (Vec::new(), String::new(), None);
    let mut chars = cmd.chars().peekable();
    let mut pushed = false;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, '\'') => quote = Some('\''),
            (None, '"') => quote = Some('"'),
            (Some(q), c) if c == q => quote = None,
            (Some(_), '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() || pushed {
                    parts.push(std::mem::take(&mut cur));
                    pushed = false;
                }
            }
            (_, c) => {
                cur.push(c);
                pushed = true;
            }
        }
    }
    if !cur.is_empty() || pushed {
        parts.push(cur);
    }
    parts
}

/// Tool-output bound: `verify::condense_output` keeps signal lines anywhere in the full output (noise dropped, ≤80 kept lines), then `OUT_CAP` chars is the outer cap; the `ok` verdict is exit-status on the raw output, decided before condensing.
async fn run_allowed(
    policy: &Policy,
    cmd: &str,
    timeout: Duration,
) -> Result<(bool, String), ToolError> {
    if !policy.allowed_commands.iter().any(|a| a == cmd)
        && !prefix_allowed(&policy.allowed_prefixes, cmd)
    {
        return Err(ToolError::Denied(format!("command not allowlisted: {cmd}")));
    }
    let argv = split_cmd(cmd);
    let (bin, rest) = argv
        .split_first()
        .ok_or_else(|| ToolError::Failed("empty cmd".to_string()))?;
    // No shell: argv exec only, so `;`, `$()`, `&&` are literal arguments.
    let out = tokio::time::timeout(
        timeout,
        tokio::process::Command::new(bin)
            .args(rest)
            .current_dir(&policy.root)
            // kill_on_drop (tokio default false): a timed-out child must not
            // keep running and mutating the workdir after we report timeout.
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| ToolError::Failed(format!("timeout after {timeout:?}")))?
    .map_err(|e| ToolError::Failed(e.to_string()))?;
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.is_empty() {
        s.push_str("\n[stderr]\n");
        s.push_str(&err);
    }
    let ok = out.status.success();
    let (content, _) = cap_chars(condense_output(&s, None), OUT_CAP);
    Ok((ok, content))
}

static SYNTAX_N: AtomicUsize = AtomicUsize::new(0);

/// Edit-gate syntax check: candidate bytes go to a tempfile OUTSIDE the
/// tree, `syntax_cmd + tempfile` runs through the existing `run_allowed`
/// gate (so the checker itself must be allowlisted), tempfile deleted
/// best-effort. A failing check vetoes the write; the file is untouched.
async fn check_syntax(policy: &Policy, candidate: &str) -> Result<(), ToolError> {
    let argv = policy.syntax_cmd.clone().unwrap_or_default();
    let tmp = std::env::temp_dir().join(format!(
        "tools-std-syntax-{}-{}",
        std::process::id(),
        SYNTAX_N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(&tmp, candidate).map_err(|e| ToolError::Failed(e.to_string()))?;
    let cmdline = format!("{} {}", argv.join(" "), tmp.display());
    let res = run_allowed(policy, &cmdline, EXEC_TIMEOUT).await;
    let _ = std::fs::remove_file(&tmp);
    match res {
        Ok((true, _)) => Ok(()),
        Ok((false, out)) => Err(ToolError::Failed(format!("syntax check failed: {out}"))),
        Err(e) => Err(e),
    }
}

pub struct ExecTool {
    policy: Arc<Policy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CmdArgs {
    cmd: String,
}

fn cmd_schema() -> Value {
    json!({
        "type": "object",
        "required": ["cmd"],
        "additionalProperties": false,
        "properties": {
            "cmd": {"type": "string", "maxLength": 8192}
        }
    })
}

#[async_trait]
impl Tool for ExecTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "exec".to_string(),
            description: "run an allowlisted command without a shell".to_string(),
            schema: cmd_schema(),
        }
    }
    fn prepare(&self, call: &ToolCall) -> CallStatus {
        dispatch(call)
    }
    async fn execute(
        &self,
        inv: Invocation,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let args: CmdArgs = parse_args(&inv.args)?;
        let (_ok, content) = run_allowed(&self.policy, &args.cmd, EXEC_TIMEOUT).await?;
        Ok(ToolOutcome {
            content,
            truncated: false,
        })
    }
}

// --- test: same gate + runner, pass/fail verdict shape ---

pub struct TestTool {
    policy: Arc<Policy>,
}

#[async_trait]
impl Tool for TestTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "test".to_string(),
            description: "run an allowlisted check, report PASS/FAIL verdict".to_string(),
            schema: cmd_schema(),
        }
    }
    fn prepare(&self, call: &ToolCall) -> CallStatus {
        dispatch(call)
    }
    async fn execute(
        &self,
        inv: Invocation,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let args: CmdArgs = parse_args(&inv.args)?;
        let (ok, out) = run_allowed(&self.policy, &args.cmd, EXEC_TIMEOUT).await?;
        // Cascade policy (what FAIL does to the loop) lives in loop/bets, not here.
        let content = if ok {
            format!("PASS: {}\n{out}", args.cmd)
        } else {
            format!("FAIL: {}\n{out}", args.cmd)
        };
        Ok(ToolOutcome {
            content,
            truncated: false,
        })
    }
}

/// Constructors take the shared policy. Tool trait impls are the worker's job.
pub fn view_tool(policy: Arc<Policy>) -> ViewTool {
    ViewTool { policy }
}
pub fn search_tool(policy: Arc<Policy>) -> SearchTool {
    SearchTool { policy }
}
pub fn edit_tool(policy: Arc<Policy>) -> EditTool {
    EditTool { policy }
}
pub fn write_tool(policy: Arc<Policy>) -> WriteTool {
    WriteTool { policy }
}
pub fn exec_tool(policy: Arc<Policy>) -> ExecTool {
    ExecTool { policy }
}
pub fn test_tool(policy: Arc<Policy>) -> TestTool {
    TestTool { policy }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tool_core::{GrantGate, Registry};

    static N: AtomicUsize = AtomicUsize::new(0);

    fn tmp_root() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "tools-std-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn policy(root: &Path) -> Arc<Policy> {
        Arc::new(Policy {
            root: root.to_path_buf(),
            allowed_commands: vec!["true".to_string(), "false".to_string()],
            allowed_prefixes: vec!["echo".to_string(), "cargo test".to_string()],
            syntax_cmd: None,
            denied_globs: default_denied_globs(),
        })
    }

    fn syntax_policy(root: &Path, argv: &[&str]) -> Arc<Policy> {
        // The checker runs as `argv + tempfile` through `run_allowed`, so
        // the checker binary must be prefix-allowlisted.
        let prefix = argv.join(" ");
        Arc::new(Policy {
            root: root.to_path_buf(),
            allowed_commands: vec!["true".to_string(), "false".to_string()],
            allowed_prefixes: vec!["echo".to_string(), prefix],
            syntax_cmd: Some(argv.iter().map(|s| s.to_string()).collect()),
            denied_globs: default_denied_globs(),
        })
    }

    fn reg(p: Arc<Policy>) -> Registry {
        let mut r = Registry::new(Arc::new(GrantGate::new(
            [(
                "agent".to_string(),
                vec!["view", "search", "edit", "write", "exec", "test"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            )]
            .into_iter()
            .collect::<HashMap<_, _>>(),
        )));
        r.register(Arc::new(view_tool(p.clone())));
        r.register(Arc::new(search_tool(p.clone())));
        r.register(Arc::new(edit_tool(p.clone())));
        r.register(Arc::new(write_tool(p.clone())));
        r.register(Arc::new(exec_tool(p.clone())));
        r.register(Arc::new(test_tool(p)));
        r
    }

    fn call(id: &str, name: &str, args: Value) -> ToolCall {
        ToolCall {
            call_id: id.into(),
            name: name.into(),
            args,
        }
    }

    async fn run(r: &Registry, name: &str, args: Value) -> Result<ToolOutcome, String> {
        let status = r.prepare("agent", call("c1", name, args));
        let inv = match status {
            CallStatus::Dispatch(inv) => inv,
            CallStatus::Result(res) => return Err(res.content),
        };
        let tool = r.resolve(name).unwrap();
        tool.execute(inv, CancellationToken::new())
            .await
            .map_err(|e| e.to_string())
    }

    // --- containment matrix ---

    #[test]
    fn git_write_denied_read_allowed() {
        let root = tmp_root();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        for rel in [".git/config", "a/.git/x", "a/.git", ".git"] {
            match resolve_under(&root, rel, true, &[]) {
                Err(ToolPathError::Denied(_)) => {}
                other => panic!("{rel} write must be denied, got {other:?}"),
            }
        }
        // Reads may traverse .git (still root-anchored + symlink-safe).
        assert!(resolve_under(&root, ".git/HEAD", false, &[]).is_ok());
    }

    #[test]
    fn escape_denied() {
        let root = tmp_root();
        for rel in ["../evil", "/etc/passwd", "a/../../evil"] {
            for write in [true, false] {
                assert!(
                    matches!(
                        resolve_under(&root, rel, write, &[]),
                        Err(ToolPathError::Denied(_))
                    ),
                    "{rel} must be denied"
                );
            }
        }
    }

    #[test]
    fn symlink_escape_denied() {
        let root = tmp_root();
        let outside = tmp_root();
        std::fs::write(outside.join("secret"), "x").unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), root.join("link")).unwrap();
        assert!(matches!(
            resolve_under(&root, "link", true, &[]),
            Err(ToolPathError::Denied(_))
        ));
        std::os::unix::fs::symlink(&outside, root.join("dirlink")).unwrap();
        std::fs::create_dir_all(root.join("real")).unwrap();
        assert!(matches!(
            resolve_under(&root, "dirlink/secret", true, &[]),
            Err(ToolPathError::Denied(_))
        ));
    }

    #[test]
    fn missing_parent_is_recoverable_never_denial() {
        let root = tmp_root();
        match resolve_under(&root, "no/such/file.txt", true, &[]) {
            Err(ToolPathError::MissingParent(m)) => assert!(m.contains("create"), "{m}"),
            other => panic!("must be MissingParent, got {other:?}"),
        }
    }

    // --- patch ---

    #[test]
    fn hunk_exact_once() {
        assert_eq!(
            apply_hunk("a\nfoo\nb\n", "foo\n", "bar\n").unwrap(),
            "a\nbar\nb\n"
        );
    }

    #[test]
    fn hunk_whitespace_tolerant() {
        assert_eq!(
            apply_hunk("fn f() {\n    x  =  1;\n}\n", "x = 1;", "x = 2;").unwrap(),
            "fn f() {\n    x = 2;\n}\n"
        );
    }

    #[test]
    fn hunk_ambiguous_fails() {
        assert!(apply_hunk("foo\nfoo\n", "foo\n", "bar\n").is_err());
        assert!(apply_hunk("a  b\nc\na   b\n", "a b", "z").is_err());
        assert!(apply_hunk("x\n", "nope", "z").is_err());
        assert!(apply_hunk("x\n", "", "z").is_err());
    }

    #[tokio::test]
    async fn edit_roundtrip_via_registry() {
        let root = tmp_root();
        std::fs::write(root.join("f.txt"), "hello\n").unwrap();
        let r = reg(policy(&root));
        let out = run(
            &r,
            "edit",
            json!({"path": "f.txt", "search": "hello", "replace": "bye"}),
        )
        .await
        .unwrap();
        assert!(out.content.contains("patched"));
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "bye\n"
        );
        assert!(
            run(&r, "view", json!({"path": "f.txt"}))
                .await
                .unwrap()
                .content
                == "bye\n"
        );
    }

    // --- write ---

    #[tokio::test]
    async fn write_creates_new_with_parents_then_overwrites() {
        let root = tmp_root();
        let r = reg(policy(&root));
        let out = run(
            &r,
            "write",
            json!({"path": "a/b/new.txt", "content": "first\n"}),
        )
        .await
        .unwrap();
        assert!(out.content.contains("wrote a/b/new.txt"), "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(root.join("a/b/new.txt")).unwrap(),
            "first\n"
        );
        // Overwrite is the whole point: no search hunk, no read-modify-write.
        run(
            &r,
            "write",
            json!({"path": "a/b/new.txt", "content": "second\n"}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("a/b/new.txt")).unwrap(),
            "second\n"
        );
    }

    #[tokio::test]
    async fn write_jail_escape_and_glob_denied() {
        let root = tmp_root();
        let outside = tmp_root();
        let r = reg(policy(&root));
        for rel in ["../evil.txt", "/etc/passwd"] {
            let err = run(&r, "write", json!({"path": rel, "content": "x"}))
                .await
                .unwrap_err();
            assert!(err.contains("denied"), "{rel}: {err}");
        }
        let err = run(&r, "write", json!({"path": ".env", "content": "SECRET=1"}))
            .await
            .unwrap_err();
        assert!(err.contains("denied"), "{err}");
        assert!(!outside.join("evil.txt").exists());
        assert!(!root.join(".env").exists());
        // A symlinked dir must not be used as a mkdir tunnel out of the root.
        std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
        let err = run(
            &r,
            "write",
            json!({"path": "out/tunneled.txt", "content": "x"}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("denied"), "{err}");
        assert!(!outside.join("tunneled.txt").exists());
    }

    #[tokio::test]
    async fn write_refuses_directory() {
        let root = tmp_root();
        std::fs::create_dir_all(root.join("d")).unwrap();
        let r = reg(policy(&root));
        let err = run(&r, "write", json!({"path": "d", "content": "x"}))
            .await
            .unwrap_err();
        assert!(err.contains("directory"), "{err}");
    }

    #[tokio::test]
    async fn write_caps_enforced() {
        let root = tmp_root();
        let r = reg(policy(&root));
        let big = "x".repeat(EDIT_REPLACE_CAP + 1);
        let err = run(&r, "write", json!({"path": "big.txt", "content": big}))
            .await
            .unwrap_err();
        assert!(err.contains("256KB cap"), "{err}");
        assert!(!root.join("big.txt").exists());

        std::fs::write(root.join("huge.txt"), "y".repeat(EDIT_FILE_CAP + 1)).unwrap();
        let err = run(
            &r,
            "write",
            json!({"path": "huge.txt", "content": "small\n"}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("512KB cap"), "{err}");
        assert_eq!(
            std::fs::read_to_string(root.join("huge.txt"))
                .unwrap()
                .len(),
            EDIT_FILE_CAP + 1
        );
    }

    #[tokio::test]
    async fn write_syntax_veto_leaves_file_untouched() {
        let root = tmp_root();
        std::fs::write(root.join("f.txt"), "hello\n").unwrap();
        let r = reg(syntax_policy(&root, &["false"]));
        let err = run(&r, "write", json!({"path": "f.txt", "content": "bye\n"}))
            .await
            .unwrap_err();
        assert!(err.contains("syntax check failed"), "{err}");
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "hello\n"
        );
    }

    // --- exec ---

    #[tokio::test]
    async fn edit_syntax_veto_leaves_file_untouched() {
        let root = tmp_root();
        std::fs::write(root.join("f.txt"), "hello\n").unwrap();
        let r = reg(syntax_policy(&root, &["false"]));
        let err = run(
            &r,
            "edit",
            json!({"path": "f.txt", "search": "hello", "replace": "bye"}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("syntax check failed"), "{err}");
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "hello\n"
        );
    }

    #[tokio::test]
    async fn edit_syntax_pass_on_true_writes() {
        let root = tmp_root();
        std::fs::write(root.join("f.txt"), "hello\n").unwrap();
        let r = reg(syntax_policy(&root, &["true"]));
        let out = run(
            &r,
            "edit",
            json!({"path": "f.txt", "search": "hello", "replace": "bye"}),
        )
        .await
        .unwrap();
        assert!(out.content.contains("patched"));
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "bye\n"
        );
    }

    #[tokio::test]
    async fn edit_unconfigured_syntax_skips_with_zero_behavior_change() {
        let root = tmp_root();
        std::fs::write(root.join("f.txt"), "hello\n").unwrap();
        assert!(policy(&root).syntax_cmd.is_none());
        let r = reg(policy(&root));
        run(
            &r,
            "edit",
            json!({"path": "f.txt", "search": "hello", "replace": "bye"}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "bye\n"
        );
    }

    #[test]
    fn denied_glob_blocks_env_allows_example() {
        assert!(glob_match("**/*.env", ".env"));
        assert!(glob_match("**/*.env", "a/.env"));
        assert!(!glob_match("**/*.env", ".env.example"));
        assert!(!glob_match("**/*.env", "a/.env.example"));
        let root = tmp_root();
        let globs = default_denied_globs();
        for rel in [".env", "a/.env"] {
            match resolve_under(&root, rel, false, &globs) {
                Err(ToolPathError::Denied(_)) => {}
                other => panic!("{rel} must be denied, got {other:?}"),
            }
            match resolve_under(&root, rel, true, &globs) {
                Err(ToolPathError::Denied(_)) => {}
                other => panic!("{rel} write must be denied, got {other:?}"),
            }
        }
        assert!(resolve_under(&root, ".env.example", false, &globs).is_ok());
    }

    #[tokio::test]
    async fn git_view_allowed_write_denied_via_registry() {
        let root = tmp_root();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(root.join("f.txt"), "hello\n").unwrap();
        let r = reg(policy(&root));
        let out = run(&r, "view", json!({"path": ".git/HEAD"})).await.unwrap();
        assert!(out.content.contains("ref:"), "{}", out.content);
        let err = run(
            &r,
            "edit",
            json!({"path": ".git/HEAD", "search": "ref:", "replace": "x"}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("denied"), "{err}");
    }

    #[test]
    fn prefix_boundary() {
        let p = ["cargo test".to_string()];
        assert!(prefix_allowed(&p, "cargo test"));
        assert!(prefix_allowed(&p, "cargo test foo"));
        assert!(!prefix_allowed(&p, "cargo test-evil"));
        assert!(!prefix_allowed(&p, "cargo testx"));
        assert!(!prefix_allowed(&p, ""));
        assert!(!prefix_allowed(&[" ".to_string()], "cargo test"));
    }

    #[tokio::test]
    async fn exec_deny_unlisted() {
        let root = tmp_root();
        let r = reg(policy(&root));
        let err = run(&r, "exec", json!({"cmd": "rm -rf /"}))
            .await
            .unwrap_err();
        assert!(err.contains("denied"), "{err}");
        let err = run(&r, "exec", json!({"cmd": "cargo test-evil"}))
            .await
            .unwrap_err();
        assert!(err.contains("denied"), "{err}");
    }

    #[tokio::test]
    async fn exec_no_shell_proof() {
        // Under a shell `echo hello; echo PWNED error` prints two lines; with
        // argv exec the `;` is a literal argument to echo. The `error` word
        // keeps the echoed line in the condensed tool output.
        let root = tmp_root();
        let r = reg(policy(&root));
        let out = run(&r, "exec", json!({"cmd": "echo hello; echo PWNED error"}))
            .await
            .unwrap();
        assert_eq!(out.content, "hello; echo PWNED error");
    }

    #[tokio::test]
    async fn exec_timeout_kills_child() {
        // Child would touch the marker at t=1s; the 100ms timeout must kill
        // it first (kill_on_drop) or the timed-out exec keeps mutating root.
        let root = tmp_root();
        let pol = Policy {
            root: root.clone(),
            allowed_commands: vec![],
            allowed_prefixes: vec!["sh".to_string()],
            syntax_cmd: None,
            denied_globs: default_denied_globs(),
        };
        let err = run_allowed(
            &pol,
            "sh -c 'sleep 1; touch late-marker'",
            Duration::from_millis(100),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("timeout"), "{err}");
        std::thread::sleep(Duration::from_millis(1100));
        assert!(
            !root.join("late-marker").exists(),
            "child survived the timeout"
        );
    }

    #[test]
    fn split_cmd_honors_quotes() {
        assert_eq!(split_cmd("ls -la"), vec!["ls", "-la"]);
        assert_eq!(
            split_cmd("python3 -c \"print(open('f').read())\""),
            vec!["python3", "-c", "print(open('f').read())"]
        );
        assert_eq!(split_cmd("echo 'a b' c"), vec!["echo", "a b", "c"]);
        assert!(split_cmd("").is_empty());
    }

    // --- test tool verdict ---

    #[tokio::test]
    async fn test_verdict_shape() {
        let root = tmp_root();
        let r = reg(policy(&root));
        let pass = run(&r, "test", json!({"cmd": "true"})).await.unwrap();
        assert!(pass.content.starts_with("PASS: true"), "{}", pass.content);
        let fail = run(&r, "test", json!({"cmd": "false"})).await.unwrap();
        assert!(fail.content.starts_with("FAIL: false"), "{}", fail.content);
    }

    #[tokio::test]
    async fn test_noisy_output_condensed_verdict_still_pass() {
        // Early PASS marker + noise storm: the marker survives condensing,
        // filler does not, the verdict (exit status, decided on raw output)
        // stays PASS, and the evidence stays within the output bound.
        let root = tmp_root();
        let mut noisy = String::from("test result: ok. 1 passed; 0 failed\n");
        for i in 0..20_000 {
            noisy.push_str(&format!("Compiling noise v0.1.0\nfiller line {i}\n"));
        }
        assert!(noisy.len() > OUT_CAP);
        std::fs::write(root.join("noisy.txt"), &noisy).unwrap();
        let pol = Arc::new(Policy {
            root: root.clone(),
            allowed_commands: vec![],
            allowed_prefixes: vec!["cat".to_string()],
            syntax_cmd: None,
            denied_globs: default_denied_globs(),
        });
        let out = run(&reg(pol), "test", json!({"cmd": "cat noisy.txt"}))
            .await
            .unwrap();
        assert!(
            out.content.starts_with("PASS: cat noisy.txt"),
            "{}",
            out.content
        );
        assert!(
            out.content.contains("test result: ok. 1 passed; 0 failed"),
            "{}",
            out.content
        );
        assert!(!out.content.contains("filler line"), "{}", out.content);
        assert!(
            out.content.chars().count() <= OUT_CAP,
            "{}",
            out.content.chars().count()
        );
    }

    // --- search ---

    #[tokio::test]
    async fn search_finds_lines_skips_git() {
        let root = tmp_root();
        std::fs::write(root.join("a.txt"), "needle here\nplain\n").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/needle.txt"), "needle hidden\n").unwrap();
        let r = reg(policy(&root));
        let out = run(&r, "search", json!({"pattern": "needle"}))
            .await
            .unwrap();
        assert!(out.content.contains("a.txt:1:"), "{}", out.content);
        assert!(!out.content.contains(".git"), "{}", out.content);
    }

    #[tokio::test]
    async fn search_single_file_not_starved_by_sibling_limit() {
        let root = tmp_root();
        // a.txt sorts first and alone exceeds the limit, so a sibling scan
        // would cut z.txt's lines; the direct file search must still find them.
        std::fs::write(root.join("a.txt"), "needle sibling\n".repeat(60)).unwrap();
        std::fs::write(root.join("z.txt"), "needle target\n").unwrap();
        let r = reg(policy(&root));
        let out = run(&r, "search", json!({"path": "z.txt", "pattern": "needle"}))
            .await
            .unwrap();
        assert!(
            out.content.contains("z.txt:1: needle target"),
            "{}",
            out.content
        );
        assert!(!out.content.contains("a.txt"), "{}", out.content);
    }

    // --- schema strictness incl no-coercion ---

    #[test]
    fn schemas_strict_no_coercion() {
        let root = tmp_root();
        let r = reg(policy(&root));
        for (name, valid) in [
            ("view", json!({"path": "f"})),
            ("search", json!({"pattern": "x"})),
            ("edit", json!({"path": "f", "search": "a", "replace": "b"})),
            ("write", json!({"path": "f", "content": "b"})),
            ("exec", json!({"cmd": "true"})),
            ("test", json!({"cmd": "true"})),
        ] {
            assert!(
                matches!(
                    r.prepare("agent", call("v", name, valid)),
                    CallStatus::Dispatch(_)
                ),
                "{name} valid args must dispatch"
            );
        }
        // Extra property rejected on every tool.
        for name in ["view", "search", "edit", "write", "exec", "test"] {
            let args = match name {
                "view" => json!({"path": "f", "zzz": 1}),
                "search" => json!({"pattern": "x", "zzz": 1}),
                "edit" => json!({"path": "f", "search": "a", "replace": "b", "zzz": 1}),
                "write" => json!({"path": "f", "content": "b", "zzz": 1}),
                _ => json!({"cmd": "true", "zzz": 1}),
            };
            match r.prepare("agent", call("x", name, args)) {
                CallStatus::Result(res) => assert!(res.is_error, "{name} extra prop"),
                CallStatus::Dispatch(_) => panic!("{name} must reject extra property"),
            }
        }
        // No coercion: string "42" is not an integer; missing required fails.
        match r.prepare(
            "agent",
            call("n", "view", json!({"path": "f", "max_bytes": "42"})),
        ) {
            CallStatus::Result(res) => assert!(res.is_error),
            CallStatus::Dispatch(_) => panic!("view must reject string-for-integer"),
        }
        match r.prepare("agent", call("m", "view", json!({}))) {
            CallStatus::Result(res) => assert!(res.is_error),
            CallStatus::Dispatch(_) => panic!("view must reject missing required"),
        }
        match r.prepare("agent", call("w", "exec", json!({"cmd": 42}))) {
            CallStatus::Result(res) => assert!(res.is_error),
            CallStatus::Dispatch(_) => panic!("exec must reject wrong type"),
        }
    }

    #[test]
    fn definitions_pinned() {
        let root = tmp_root();
        let r = reg(policy(&root));
        let defs: Vec<String> = r.definitions().iter().map(|d| d.name.clone()).collect();
        assert_eq!(
            defs,
            vec!["edit", "exec", "search", "test", "view", "write"]
        );
        for d in r.definitions() {
            assert_eq!(
                d.schema.get("additionalProperties"),
                Some(&Value::Bool(false)),
                "{} must be closed",
                d.name
            );
            assert!(
                d.schema.get("required").is_some(),
                "{} needs required",
                d.name
            );
        }
    }
}
