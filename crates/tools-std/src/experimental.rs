use agent_log::{read_log, Item, ItemKind};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::process::Child;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use tool_core::{Invocation, Tool, ToolDefinition, ToolError, ToolOutcome};

use crate::common::{
    apply_hunk, cap_chars, drop_front_bytes, parse_args, path_err, EDIT_FILE_CAP, EDIT_REPLACE_CAP,
    EXEC_TIMEOUT, OUT_CAP,
};
use crate::policy::{resolve_under, Policy};
use crate::runner::{cmd_schema, run_allowed_raw, split_cmd, CmdArgs};
use crate::{edit_tool, exec_tool, test_tool, view_tool, write_tool};

#[derive(Default, Clone, Debug)]
pub struct ExperimentalToolsConfig {
    pub output_dir: Option<PathBuf>,
    /// Fixed host-selected current WAL. When present, enables history_read.
    pub history_log: Option<PathBuf>,
    pub shell: bool,
    pub processes: bool,
    pub repo_lookup: bool,
    pub repo_evidence: bool,
    pub multi_edit: bool,
}

static N: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct Artifacts(Arc<Mutex<HashMap<String, PathBuf>>>);

fn next_id() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

fn archive_dir(policy: &Policy, path: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(path).map_err(|e| format!("cannot create output archive: {e}"))?;
    let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    let root = std::fs::canonicalize(&policy.root).map_err(|e| e.to_string())?;
    if path.starts_with(root) {
        return Err("output archive must be outside the workspace".into());
    }
    Ok(path)
}

pub fn experimental_tools(
    policy: Arc<Policy>,
    config: ExperimentalToolsConfig,
) -> Result<Vec<Arc<dyn Tool>>, String> {
    if (config.shell || config.processes) && !policy.allow_any_cmd {
        return Err("shell and process tools require allow_any_cmd".into());
    }
    if config.repo_evidence && !config.repo_lookup {
        return Err("repo_evidence requires repo_lookup".into());
    }
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(view_tool(policy.clone())),
        Arc::new(crate::search_tool(policy.clone())),
        Arc::new(edit_tool(policy.clone())),
        Arc::new(write_tool(policy.clone())),
    ];
    if let Some(dir) = config.output_dir.as_deref() {
        let dir = archive_dir(&policy, dir)?;
        let handles = Artifacts(Arc::new(Mutex::new(HashMap::new())));
        tools.push(Arc::new(ArchivedExec {
            policy: policy.clone(),
            dir: dir.clone(),
            handles: handles.clone(),
            test: false,
        }));
        tools.push(Arc::new(ArchivedExec {
            policy: policy.clone(),
            dir: dir.clone(),
            handles: handles.clone(),
            test: true,
        }));
        tools.push(Arc::new(OutputRead { handles }));
    } else {
        tools.push(Arc::new(exec_tool(policy.clone())));
        tools.push(Arc::new(test_tool(policy.clone())));
    }
    if let Some(path) = config.history_log.as_deref() {
        if !path.is_absolute() {
            return Err("history log path must be absolute".into());
        }
        tools.push(Arc::new(HistoryRead {
            path: path.to_path_buf(),
        }));
    }
    if config.shell {
        tools.push(Arc::new(Shell {
            policy: policy.clone(),
        }));
    }
    if config.processes {
        tools.push(Arc::new(ProcessTool::new(policy.clone())));
    }
    if config.repo_lookup {
        tools.push(Arc::new(RepoLookup {
            policy: policy.clone(),
            evidence: config.repo_evidence,
        }));
    }
    if config.multi_edit {
        tools.push(Arc::new(MultiEdit { policy }));
    }
    Ok(tools)
}

struct ArchivedExec {
    policy: Arc<Policy>,
    dir: PathBuf,
    handles: Artifacts,
    test: bool,
}

#[async_trait]
impl Tool for ArchivedExec {
    fn definition(&self) -> ToolDefinition {
        let mut definition = if self.test {
            crate::test_tool(self.policy.clone()).definition()
        } else {
            crate::exec_tool(self.policy.clone()).definition()
        };
        definition.description.push_str(
            " Output is archived up to 64 MiB per command; use output_read with the returned handle to retrieve pages or search it.",
        );
        definition
    }
    async fn execute(
        &self,
        inv: Invocation,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let args: CmdArgs = parse_args(&inv.args)?;
        let id = next_id();
        let path = self.dir.join(format!("output-{id}.log"));
        let (ok, output, truncated) = crate::runner::run_allowed_archived(
            &self.policy,
            &args.cmd,
            EXEC_TIMEOUT,
            &cancel,
            &path,
        )
        .await?;
        self.handles
            .0
            .lock()
            .map_err(|_| ToolError::Failed("artifact lock poisoned".into()))?
            .insert(id.clone(), path);
        let prefix = if self.test {
            if ok {
                format!("PASS: {}\n", args.cmd)
            } else {
                format!("FAIL: {}\n", args.cmd)
            }
        } else {
            String::new()
        };
        Ok(ToolOutcome {
            content: format!("output handle={id}\n{prefix}{output}"),
            truncated,
            success: ok,
        })
    }
}

struct OutputRead {
    handles: Artifacts,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    handle: String,
    #[serde(default)]
    offset: u64,
    #[serde(default = "default_limit")]
    limit: usize,
    search: Option<String>,
}
fn default_limit() -> usize {
    OUT_CAP
}
#[async_trait]
impl Tool for OutputRead {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "output_read".into(),
            description: "read archived output by run-owned handle; offset is a byte offset for pages or a match index for search, and limit is page bytes or match count respectively".into(),
            schema: json!({"type":"object","required":["handle"],"additionalProperties":false,"properties":{"handle":{"type":"string","maxLength":80},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":HISTORY_PAGE_MAX},"search":{"type":"string","maxLength":4096}}}),
        }
    }
    async fn execute(
        &self,
        inv: Invocation,
        _: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let a: ReadArgs = parse_args(&inv.args)?;
        if a.limit == 0 || a.limit > HISTORY_PAGE_MAX {
            return Err(ToolError::Failed(format!(
                "limit must be 1..{HISTORY_PAGE_MAX}"
            )));
        }
        if a.search
            .as_ref()
            .is_some_and(|q| q.is_empty() || q.len() > 4096)
        {
            return Err(ToolError::Failed(
                "search must contain 1..4096 bytes".into(),
            ));
        }
        let path = self
            .handles
            .0
            .lock()
            .map_err(|_| ToolError::Failed("artifact lock poisoned".into()))?
            .get(&a.handle)
            .cloned()
            .ok_or_else(|| ToolError::Denied("unknown output handle".into()))?;
        let size = tokio::fs::metadata(&path)
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?
            .len();
        if let Some(query) = a.search.as_deref() {
            let mut file = tokio::fs::File::open(path)
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            let mut buf = [0u8; 8192];
            let mut carry = Vec::new();
            let mut matches = Vec::new();
            let mut total = 0u64;
            let mut response_bytes = 0usize;
            loop {
                let n = file
                    .read(&mut buf)
                    .await
                    .map_err(|e| ToolError::Failed(e.to_string()))?;
                if n == 0 {
                    break;
                }
                carry.extend_from_slice(&buf[..n]);
                for (hit, _) in carry
                    .windows(query.len())
                    .enumerate()
                    .filter(|(_, window)| *window == query.as_bytes())
                {
                    let current = total;
                    total += 1;
                    if current < a.offset
                        || matches.len() >= a.limit
                        || response_bytes >= HISTORY_PAGE_MAX
                    {
                        continue;
                    }
                    let start = hit.saturating_sub(128);
                    let end = (hit + query.len() + 128).min(carry.len());
                    let snippet = String::from_utf8_lossy(&carry[start..end]).into_owned();
                    response_bytes += snippet.len() + 1;
                    if response_bytes <= HISTORY_PAGE_MAX {
                        matches.push(snippet);
                    }
                }
                let keep = query.len().saturating_sub(1).min(carry.len());
                carry.drain(..carry.len() - keep);
            }
            return Ok(ToolOutcome {
                content: format!(
                    "matches={total} bytes={size} offset={}\n{}",
                    a.offset,
                    matches.join("\n")
                ),
                truncated: total > a.offset + matches.len() as u64,
                success: true,
            });
        }
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        let offset = a.offset.min(size);
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        let mut page = vec![0; a.limit.min((size - offset) as usize)];
        file.read_exact(&mut page)
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(ToolOutcome {
            content: format!(
                "{}\n(bytes={size} offset={offset})",
                String::from_utf8_lossy(&page)
            ),
            truncated: offset + (page.len() as u64) < size,
            success: true,
        })
    }
}

const HISTORY_PAGE_DEFAULT: usize = 8_000;
const HISTORY_PAGE_MAX: usize = 32_768;

struct HistoryRead {
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryReadArgs {
    item_id: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "history_page_default")]
    limit: usize,
}

fn history_page_default() -> usize {
    HISTORY_PAGE_DEFAULT
}

struct TempHistoryLog(PathBuf);

impl Drop for TempHistoryLog {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn create_history_snapshot() -> Result<(TempHistoryLog, std::fs::File), ToolError> {
    for _ in 0..32 {
        let path = std::env::temp_dir().join(format!("rof-history-{}.jsonl", next_id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((TempHistoryLog(path), file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(ToolError::Failed(e.to_string())),
        }
    }
    Err(ToolError::Failed(
        "could not create bounded history snapshot".into(),
    ))
}

fn read_history(path: &Path, cancel: &CancellationToken) -> Result<Vec<Item>, ToolError> {
    if cancel.is_cancelled() {
        return Err(ToolError::Failed("history read cancelled".into()));
    }
    let mut source_options = std::fs::OpenOptions::new();
    source_options.read(true);
    let path_metadata = std::fs::symlink_metadata(path)
        .map_err(|e| ToolError::Failed(format!("cannot inspect current history log: {e}")))?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(ToolError::Failed(
            "history log must be a regular non-symlink file".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        source_options.custom_flags(libc::O_NOFOLLOW);
    }
    let source = source_options
        .open(path)
        .map_err(|e| ToolError::Failed(format!("cannot open current history log: {e}")))?;
    let metadata = source
        .metadata()
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    if !metadata.is_file() {
        return Err(ToolError::Failed(
            "history log is not a regular file".into(),
        ));
    }
    if metadata.len() > crate::common::READ_FILE_CAP {
        return Err(ToolError::Failed(format!(
            "history log exceeds {}-byte limit",
            crate::common::READ_FILE_CAP
        )));
    }

    // Snapshot at most 64 MiB before using the shared validator, so a growing
    // source cannot bypass the bound that agent_log::read_log itself lacks.
    let (snapshot_path, mut snapshot) = create_history_snapshot()?;
    let mut source = std::io::Read::take(source, crate::common::READ_FILE_CAP + 1);
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(ToolError::Failed("history read cancelled".into()));
        }
        let n = std::io::Read::read(&mut source, &mut buffer)
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > crate::common::READ_FILE_CAP {
            return Err(ToolError::Failed(format!(
                "history log exceeds {}-byte limit",
                crate::common::READ_FILE_CAP
            )));
        }
        std::io::Write::write_all(&mut snapshot, &buffer[..n])
            .map_err(|e| ToolError::Failed(e.to_string()))?;
    }
    drop(snapshot);
    if cancel.is_cancelled() {
        return Err(ToolError::Failed("history read cancelled".into()));
    }
    let items = read_log(&snapshot_path.0)
        .map_err(|e| ToolError::Failed(format!("cannot read current history log: {e}")))?;
    if cancel.is_cancelled() {
        return Err(ToolError::Failed("history read cancelled".into()));
    }
    Ok(items)
}

#[async_trait]
impl Tool for HistoryRead {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "history_read".into(),
            description: "read a paged historical tool observation from this run's fixed WAL; retrieves the recorded result and never replays the command".into(),
            schema: json!({"type":"object","required":["item_id"],"additionalProperties":false,"properties":{"item_id":{"type":"string","minLength":1,"maxLength":128},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":HISTORY_PAGE_MAX}}}),
        }
    }
    async fn execute(
        &self,
        invocation: Invocation,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let args: HistoryReadArgs = parse_args(&invocation.args)?;
        if args.item_id.is_empty() || args.item_id.len() > 128 {
            return Err(ToolError::Failed(
                "item_id must contain 1..128 bytes".into(),
            ));
        }
        if args.limit == 0 || args.limit > HISTORY_PAGE_MAX {
            return Err(ToolError::Failed(format!(
                "limit must be 1..{HISTORY_PAGE_MAX}"
            )));
        }
        let items = read_history(&self.path, &cancel)?;
        let item = items
            .into_iter()
            .find(|item| item.id == args.item_id)
            .ok_or_else(|| ToolError::Failed("unknown history item_id".into()))?;
        let ItemKind::ToolResult {
            call_id,
            content,
            is_error,
            ..
        } = item.kind
        else {
            return Err(ToolError::Failed(
                "history item_id does not identify a tool result".into(),
            ));
        };
        let total = content.chars().count();
        let page: String = content.chars().skip(args.offset).take(args.limit).collect();
        let page_chars = page.chars().count();
        Ok(ToolOutcome {
            content: format!(
                "[historical observation item_id={} call_id={} seq={} recorded_at={:?} is_error={} offset={} total_chars={}]\n{}",
                item.id, call_id, item.seq, item.recorded_at, is_error, args.offset, total, page
            ),
            truncated: args.offset.saturating_add(page_chars) < total,
            success: true,
        })
    }
}

struct Shell {
    policy: Arc<Policy>,
}
#[async_trait]
impl Tool for Shell {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell".into(),
            description:
                "run an explicitly enabled shell command through the configured exec wrapper".into(),
            schema: cmd_schema(),
        }
    }
    async fn execute(
        &self,
        inv: Invocation,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let a: CmdArgs = parse_args(&inv.args)?;
        let argv = ["sh".to_string(), "-lc".to_string(), a.cmd];
        let cmd = argv
            .iter()
            .map(|s| format!("'{}'", s.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join(" ");
        let (ok, content, truncated) =
            run_allowed_raw(&self.policy, &cmd, EXEC_TIMEOUT, &cancel).await?;
        Ok(ToolOutcome {
            content,
            truncated,
            success: ok,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LookupArgs {
    symbol: String,
    #[serde(default = "lookup_limit")]
    limit: usize,
}
fn lookup_limit() -> usize {
    100
}
const LOOKUP_ENTRY_CAP: usize = 100_000;
const LOOKUP_FILE_CAP: usize = 10_000;
const LOOKUP_BYTE_CAP: u64 = 64 * 1024 * 1024;

#[cfg(test)]
mod lookup_scan_tests {
    #[test]
    fn bounded_collection_reads_only_one_entry_past_the_limit() {
        let mut consumed = 0;
        let entries = (0..).map(|i| {
            consumed += 1;
            Ok::<_, ()>(i)
        });
        assert_eq!(super::collect_bounded(entries, 2).unwrap(), None);
        assert_eq!(consumed, 3);
    }
}

fn collect_bounded<T, E>(
    entries: impl Iterator<Item = Result<T, E>>,
    limit: usize,
) -> Result<Option<Vec<T>>, E> {
    let entries = entries
        .take(limit.saturating_add(1))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((entries.len() <= limit).then_some(entries))
}

struct RepoLookup {
    policy: Arc<Policy>,
    evidence: bool,
}
#[async_trait]
impl Tool for RepoLookup {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition{name:"repo_lookup".into(),description:if self.evidence { "find literal occurrences and heuristic definition, test-path, and configuration-path candidates; skips symlinks and files over 1 MiB, scans at most 100,000 entries, 10,000 files, or 64 MiB; no semantic call graph" } else { "find literal symbol occurrences; skips symlinks and files over 1 MiB, scans at most 100,000 entries, 10,000 files, or 64 MiB; no semantic call graph" }.into(),schema:json!({"type":"object","required":["symbol"],"additionalProperties":false,"properties":{"symbol":{"type":"string","minLength":1,"maxLength":256},"limit":{"type":"integer","minimum":1,"maximum":500}}})}
    }
    async fn execute(
        &self,
        inv: Invocation,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let a: LookupArgs = parse_args(&inv.args)?;
        if a.symbol.is_empty() || a.limit == 0 || a.limit > 500 {
            return Err(ToolError::Failed(
                "symbol and limit (1..500) required".into(),
            ));
        }
        if cancel.is_cancelled() {
            return Err(ToolError::Failed("repository lookup cancelled".into()));
        }
        let mut stack = vec![self.policy.root.clone()];
        let mut hits = Vec::new();
        let mut groups: [Vec<String>; 4] = std::array::from_fn(|_| Vec::new());
        let mut hit_locations = HashSet::new();
        let mut snippet_cut = false;
        let mut seen = 0usize;
        let mut entries_seen = 0usize;
        let mut files_seen = 0usize;
        let mut bytes_seen = 0u64;
        let mut scan_capped = false;
        while let Some(dir) = stack.pop() {
            let entries = std::fs::read_dir(&dir).map_err(|e| ToolError::Failed(e.to_string()))?;
            let entries: Box<dyn Iterator<Item = std::io::Result<std::fs::DirEntry>>> =
                if self.evidence {
                    let remaining = LOOKUP_ENTRY_CAP.saturating_sub(entries_seen);
                    let Some(mut bounded) = collect_bounded(entries, remaining)
                        .map_err(|e| ToolError::Failed(e.to_string()))?
                    else {
                        scan_capped = true;
                        break;
                    };
                    bounded.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
                    Box::new(bounded.into_iter().map(Ok))
                } else {
                    Box::new(entries)
                };
            for e in entries {
                if cancel.is_cancelled() {
                    return Err(ToolError::Failed("repository lookup cancelled".into()));
                }
                entries_seen += 1;
                if entries_seen > LOOKUP_ENTRY_CAP {
                    scan_capped = true;
                    break;
                }
                let e = e.map_err(|e| ToolError::Failed(e.to_string()))?;
                let p = e.path();
                let rel = p
                    .strip_prefix(&self.policy.root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/");
                if rel
                    .split('/')
                    .any(|x| x == ".git" || x == "target" || x == "node_modules")
                {
                    continue;
                }
                let Ok(meta) = std::fs::symlink_metadata(&p) else {
                    continue;
                };
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() {
                    stack.push(p);
                    continue;
                }
                if !meta.is_file() {
                    continue;
                }
                if resolve_under(&self.policy.root, &rel, false, &self.policy.denied_globs).is_err()
                {
                    continue;
                }
                if meta.len() > 1024 * 1024 {
                    continue;
                }
                if files_seen >= LOOKUP_FILE_CAP
                    || bytes_seen.saturating_add(meta.len()) > LOOKUP_BYTE_CAP
                {
                    scan_capped = true;
                    break;
                }
                files_seen += 1;
                bytes_seen += meta.len();
                let Ok(s) = std::fs::read_to_string(&p) else {
                    continue;
                };
                for (i, line) in s.lines().enumerate() {
                    if line.contains(&a.symbol) {
                        seen += 1;
                        if self.evidence {
                            let location = (rel.clone(), i + 1);
                            if hit_locations.len() < a.limit && hit_locations.insert(location) {
                                let category = lookup_category(&rel, line, &a.symbol);
                                let text = line.trim();
                                let snippet = text.chars().take(400).collect::<String>();
                                snippet_cut |= text.chars().count() > 400;
                                groups[category].push(format!("{rel}:{}: {snippet}", i + 1));
                            }
                        } else if hits.len() < a.limit {
                            hits.push(format!(
                                "{}:{} [{}] {}",
                                rel,
                                i + 1,
                                if rel.split('/').any(|x| x.starts_with("test")) {
                                    "test"
                                } else {
                                    "source"
                                },
                                line.trim()
                            ));
                        }
                    }
                }
            }
            if scan_capped {
                break;
            }
        }
        let legacy = format!(
                "literal occurrences for {:?} (showing {}, total matches {}; scanned {files_seen} files/{bytes_seen} bytes)\n{}",
                a.symbol,
                hits.len(),
                seen,
                hits.join("\n")
            );
        if !self.evidence {
            return Ok(ToolOutcome {
                content: legacy,
                truncated: scan_capped || seen > hits.len(),
                success: true,
            });
        }
        let names = [
            "literal locations",
            "definition candidates",
            "test path candidates",
            "configuration path candidates",
        ];
        let mut body = format!("repository evidence for {:?} ({} literal matches; scanned {files_seen} files/{bytes_seen} bytes)\n", a.symbol, seen);
        for (name, group) in names.iter().zip(groups.iter()) {
            body.push_str(&format!("\n{name}:\n{}\n", group.join("\n")));
        }
        let mut reasons = Vec::new();
        if scan_capped {
            reasons.push("scan limit reached; results may be incomplete");
        }
        if seen > hit_locations.len() {
            reasons.push("results omitted at shared hit limit");
        }
        if snippet_cut {
            reasons.push("line snippets shortened to 400 characters");
        }
        let make_note = |reasons: &[&str]| {
            if reasons.is_empty() {
                String::new()
            } else {
                format!("\n[{}]", reasons.join("; "))
            }
        };
        let cap_note = format!("output omitted at {OUT_CAP}-character limit");
        let mut note = make_note(&reasons);
        if body.chars().count() + note.chars().count() > OUT_CAP {
            reasons.push(&cap_note);
            note = make_note(&reasons);
        }
        let room = OUT_CAP.saturating_sub(note.chars().count());
        let (mut content, char_cut) = cap_chars(body, room);
        content.push_str(&note);
        Ok(ToolOutcome {
            content,
            truncated: !reasons.is_empty() || char_cut,
            success: true,
        })
    }
}

fn lookup_category(path: &str, line: &str, symbol: &str) -> usize {
    let lower_path = path.to_ascii_lowercase();
    let base = lower_path.rsplit('/').next().unwrap_or(&lower_path);
    if lower_path
        .split('/')
        .any(|part| matches!(part, "test" | "tests" | "spec" | "specs" | "__tests__"))
        || [".test.", ".spec."]
            .iter()
            .any(|suffix| base.contains(suffix))
    {
        return 2;
    }
    if lower_path
        .split('/')
        .any(|part| matches!(part, "config" | "configs"))
        || [
            "cargo.toml",
            "package.json",
            "tsconfig.json",
            "pyproject.toml",
            "setup.cfg",
            ".env",
        ]
        .contains(&base)
        || [".toml", ".yaml", ".yml", ".ini", ".cfg"]
            .iter()
            .any(|ext| base.ends_with(ext))
    {
        return 3;
    }
    let declaration = line.trim_start();
    let declaration = declaration.trim_start_matches(['#', '/', '*', '@']);
    let declaration = declaration.trim_start();
    let declares = [
        "fn ",
        "pub fn ",
        "pub async fn ",
        "async fn ",
        "function ",
        "export function ",
        "def ",
        "class ",
        "interface ",
        "struct ",
        "enum ",
        "type ",
        "func ",
        "const ",
        "var ",
        "let ",
    ]
    .iter()
    .any(|prefix| declaration.starts_with(prefix));
    let boundaries = line.match_indices(symbol).any(|(start, matched)| {
        let before = line[..start].chars().next_back();
        let after = line[start + matched.len()..].chars().next();
        !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
    });
    if declares && boundaries {
        1
    } else {
        0
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Hunk {
    path: String,
    search: String,
    replace: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MultiArgs {
    edits: Vec<Hunk>,
}
struct MultiEdit {
    policy: Arc<Policy>,
}
#[async_trait]
impl Tool for MultiEdit {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "multi_edit".into(),
            description: "validate every unique hunk and restricted path before applying the batch"
                .into(),
            schema: json!({"type":"object","required":["edits"],"additionalProperties":false,"properties":{"edits":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"object","required":["path","search","replace"],"additionalProperties":false,"properties":{"path":{"type":"string","maxLength":4096},"search":{"type":"string","maxLength":65536},"replace":{"type":"string","maxLength":262144}}}}}}),
        }
    }
    async fn execute(
        &self,
        inv: Invocation,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let a: MultiArgs = parse_args(&inv.args)?;
        if a.edits.is_empty() || a.edits.len() > 32 {
            return Err(ToolError::Failed("edits must contain 1..32 hunks".into()));
        }
        let mut staged = Vec::new();
        let mut paths = std::collections::HashSet::new();
        for h in a.edits {
            if h.replace.len() > EDIT_REPLACE_CAP {
                return Err(ToolError::Failed("replace over 256KB cap".into()));
            }
            let p = resolve_under(&self.policy.root, &h.path, true, &self.policy.denied_globs)
                .map_err(path_err)?;
            if !paths.insert(p.clone()) {
                return Err(ToolError::Failed("duplicate path in multi_edit".into()));
            }
            if p.is_dir() {
                return Err(ToolError::Failed("refusing to patch a directory".into()));
            }
            let meta = std::fs::metadata(&p).map_err(|e| ToolError::Failed(e.to_string()))?;
            if meta.len() > EDIT_FILE_CAP as u64 {
                return Err(ToolError::Failed("file over 512KB cap".into()));
            }
            let old = std::fs::read_to_string(&p).map_err(|e| ToolError::Failed(e.to_string()))?;
            let new = apply_hunk(&old, &h.search, &h.replace).map_err(ToolError::Failed)?;
            staged.push((p, old, new, h.path));
        }
        for (written, (p, _, new, _)) in staged.iter().enumerate() {
            if let Err(e) = crate::common::overwrite(p, new.as_bytes()) {
                let mut restore_errors = Vec::new();
                for (rp, old, _, _) in staged[..=written].iter().rev() {
                    if let Err(restore) = crate::common::overwrite(rp, old.as_bytes()) {
                        restore_errors.push(format!("{}: {restore}", rp.display()));
                    }
                }
                return Err(ToolError::Failed(if restore_errors.is_empty() {
                    format!("multi_edit write failed: {e}; prior contents restored")
                } else {
                    format!(
                        "multi_edit write failed: {e}; restoration failed for {}",
                        restore_errors.join(", ")
                    )
                }));
            }
        }
        Ok(ToolOutcome {
            content: format!("patched {} files", staged.len()),
            truncated: false,
            success: true,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessArgs {
    action: String,
    id: Option<String>,
    cmd: Option<String>,
    data: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}
struct ProcessState {
    child: Child,
    pgid: Option<u32>,
    input: Option<tokio::process::ChildStdin>,
    out: Arc<AsyncMutex<String>>,
    dropped_output: Arc<AtomicBool>,
    done: bool,
}
impl ProcessState {
    fn kill_group(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            unsafe {
                libc::killpg(pgid as libc::pid_t, libc::SIGKILL);
            }
        }
        self.pgid = None;
    }
}
impl Drop for ProcessState {
    fn drop(&mut self) {
        self.kill_group();
        let _ = self.child.start_kill();
    }
}
struct Processes {
    sessions: AsyncMutex<HashMap<String, ProcessState>>,
    policy: Arc<Policy>,
}
struct ProcessTool {
    state: Arc<Processes>,
}
impl ProcessTool {
    fn new(policy: Arc<Policy>) -> Self {
        Self {
            state: Arc::new(Processes {
                sessions: AsyncMutex::new(HashMap::new()),
                policy,
            }),
        }
    }
}
#[async_trait]
impl Tool for ProcessTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "process".into(),
            description: "manage bounded pipe sessions (no PTY): start, poll, input, stop; stopping a Docker exec client may leave its container command running".into(),
            schema: json!({"type":"object","required":["action"],"additionalProperties":false,"properties":{"action":{"type":"string","enum":["start","poll","input","stop"]},"id":{"type":"string","maxLength":80},"cmd":{"type":"string","minLength":1,"maxLength":4096},"data":{"type":"string","maxLength":8192},"limit":{"type":"integer","minimum":1,"maximum":HISTORY_PAGE_MAX}}}),
        }
    }
    async fn execute(
        &self,
        inv: Invocation,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        if cancel.is_cancelled()
            && !matches!(
                inv.args.get("action").and_then(serde_json::Value::as_str),
                Some("stop")
            )
        {
            return Err(ToolError::Failed("cancelled".into()));
        }
        let a: ProcessArgs = parse_args(&inv.args)?;
        match a.action.as_str() {
            "start" => {
                let cmd = a
                    .cmd
                    .ok_or_else(|| ToolError::Failed("cmd required".into()))?;
                let argv = split_cmd(&cmd);
                if argv.is_empty() {
                    return Err(ToolError::Failed("empty cmd".into()));
                }
                let child = crate::runner::spawn_process(&self.state.policy, &argv)?;
                let id = next_id();
                let out = Arc::new(AsyncMutex::new(String::new()));
                let dropped_output = Arc::new(AtomicBool::new(false));
                let mut child = child;
                let pgid = child.id();
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let shared = out.clone();
                let dropped = dropped_output.clone();
                tokio::spawn(async move {
                    drain_session(stdout, shared.clone(), dropped.clone()).await;
                });
                let shared = out.clone();
                let dropped = dropped_output.clone();
                tokio::spawn(async move {
                    drain_session(stderr, shared, dropped).await;
                });
                let input = child.stdin.take();
                self.state.sessions.lock().await.insert(
                    id.clone(),
                    ProcessState {
                        child,
                        pgid,
                        input,
                        out,
                        dropped_output,
                        done: false,
                    },
                );
                Ok(ToolOutcome {
                    content: format!("process id={id}; pipe session (no PTY)"),
                    truncated: false,
                    success: true,
                })
            }
            "poll" => {
                let id =
                    a.id.ok_or_else(|| ToolError::Failed("id required".into()))?;
                let mut sessions = self.state.sessions.lock().await;
                let s = sessions
                    .get_mut(&id)
                    .ok_or_else(|| ToolError::Denied("unknown process id".into()))?;
                let (status, success) = match s.child.try_wait() {
                    Ok(v) => {
                        s.done = v.is_some();
                        match v {
                            Some(exit) => {
                                s.kill_group();
                                (format!("exit={exit}"), exit.success())
                            }
                            None => ("running".into(), true),
                        }
                    }
                    Err(e) => return Err(ToolError::Failed(e.to_string())),
                };
                let out = tokio::time::timeout(std::time::Duration::from_millis(25), async {
                    loop {
                        let snapshot = s.out.lock().await.clone();
                        if !snapshot.is_empty() {
                            break snapshot;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                    }
                })
                .await
                .unwrap_or_else(|_| String::new());
                let limit = a.limit.min(HISTORY_PAGE_MAX);
                Ok(ToolOutcome {
                    content: format!(
                        "{status}\n{}",
                        out.chars()
                            .rev()
                            .take(limit)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect::<String>()
                    ),
                    truncated: s.dropped_output.load(Ordering::Relaxed)
                        || out.chars().count() > limit,
                    success,
                })
            }
            "input" => {
                let id =
                    a.id.ok_or_else(|| ToolError::Failed("id required".into()))?;
                let mut sessions = self.state.sessions.lock().await;
                let s = sessions
                    .get_mut(&id)
                    .ok_or_else(|| ToolError::Denied("unknown process id".into()))?;
                let input = s
                    .input
                    .as_mut()
                    .ok_or_else(|| ToolError::Failed("process stdin is closed".into()))?;
                let data = a.data.unwrap_or_default();
                if data.len() > 8192 {
                    return Err(ToolError::Failed(
                        "process input exceeds 8192-byte cap".into(),
                    ));
                }
                let write = tokio::time::timeout(EXEC_TIMEOUT, input.write_all(data.as_bytes()));
                tokio::select! {
                    _ = cancel.cancelled() => return Err(ToolError::Failed("cancelled while writing process input".into())),
                    result = write => match result {
                        Ok(Ok(())) => (),
                        Ok(Err(e)) => return Err(ToolError::Failed(e.to_string())),
                        Err(_) => return Err(ToolError::Failed("process input timed out".into())),
                    }
                }
                Ok(ToolOutcome {
                    content: "input sent".into(),
                    truncated: false,
                    success: true,
                })
            }
            "stop" => {
                let id =
                    a.id.ok_or_else(|| ToolError::Failed("id required".into()))?;
                let mut s = {
                    let mut sessions = self.state.sessions.lock().await;
                    sessions
                        .remove(&id)
                        .ok_or_else(|| ToolError::Denied("unknown process id".into()))?
                };
                crate::runner::kill_tree(&mut s.child).await;
                if cancel.is_cancelled() {
                    return Err(ToolError::Failed(
                        "cancelled; process stopped and reaped".into(),
                    ));
                }
                Ok(ToolOutcome {
                    content: "process stopped and reaped".into(),
                    truncated: false,
                    success: true,
                })
            }
            _ => Err(ToolError::Failed(
                "action must be start, poll, input, or stop".into(),
            )),
        }
    }
}
async fn drain_session<R: AsyncReadExt + Unpin>(
    reader: Option<R>,
    out: Arc<AsyncMutex<String>>,
    dropped: Arc<AtomicBool>,
) {
    let Some(mut reader) = reader else { return };
    let mut b = [0; 4096];
    loop {
        match reader.read(&mut b).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut s = out.lock().await;
                s.push_str(&String::from_utf8_lossy(&b[..n]));
                if s.len() > 65536 {
                    dropped.store(true, Ordering::Relaxed);
                    let cut = s.len() - 65536;
                    drop_front_bytes(&mut s, cut);
                }
            }
        }
    }
}
