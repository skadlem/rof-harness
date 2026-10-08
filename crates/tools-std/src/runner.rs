use serde::Deserialize;
use serde_json::{json, Value};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use tool_core::ToolError;

use crate::common::{cap_chars, prefix_allowed, EXEC_TIMEOUT, OUT_CAP};
use crate::condense::condense_output;
use crate::policy::Policy;

/// Split a command into argv honoring single/double quotes and backslash
/// escapes (no shell: `;`, `$()`, `&&`, `|` stay literal characters). Naive
/// whitespace splitting mangles quoted args (`python3 -c "print(x)"` broke).
pub(crate) fn split_cmd(cmd: &str) -> Vec<String> {
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

/// Fixed env pass-through for exec children. The provider key (e.g.
/// `OPENAI_API_KEY`) is not on this list, and `env_clear` drops everything
/// else, so key material cannot leak into a tool child by default.
const PASS_ENV: [&str; 9] = [
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "LC_ALL",
    "TERM",
    "TMPDIR",
    "CARGO_HOME",
    "RUSTUP_HOME",
];

fn spawn_allowed(policy: &Policy, argv: &[String]) -> Result<tokio::process::Child, ToolError> {
    let (bin, rest) = argv
        .split_first()
        .ok_or_else(|| ToolError::Failed("empty cmd".to_string()))?;
    let mut cmd = std::process::Command::new(bin);
    cmd.args(rest)
        .current_dir(&policy.root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for name in PASS_ENV
        .iter()
        .map(|s| s.to_string())
        .chain(policy.pass_env.iter().cloned())
    {
        // Malformed names would panic `Command::env`; key-shaped names are
        // never forwarded even when an operator lists them in `pass_env`.
        if name.bytes().any(|b| b == b'=' || b == b'\0') || name.ends_with("API_KEY") {
            continue;
        }
        if let Some(v) = std::env::var_os(&name) {
            cmd.env(&name, v);
        }
    }
    #[cfg(unix)]
    {
        // Own process group: timeout/cancel/drop kills the whole tree
        // (grandchildren included), not just the direct child.
        // Batch courtesy: agent-spawned host children run at the lowest
        // scheduling priority — they still get the full idle CPU (no
        // wall-time change on an idle host) but yield to interactive work
        // instead of making the desktop unusable during agent builds;
        // descendants inherit the niceness. (ponytail: one syscall;
        // affinity/cgroup caps would change task wall times and are the
        // operator's isolation job per the threat model.)
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
        unsafe {
            cmd.pre_exec(|| {
                libc::setpriority(libc::PRIO_PROCESS, 0, 19);
                Ok(())
            });
        }
    }
    // Non-unix fallback: no groups, so `kill_on_drop` only reaps the direct
    // child and a grandchild holding the pipe can delay reaping. Unix is the
    // primary platform; operators elsewhere isolate at the OS level.
    tokio::process::Command::from(cmd)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| ToolError::Failed(e.to_string()))
}

/// Per-pipe bound: head + tail cap the retained bytes; the dropped middle
/// is counted and reported, never stored, so a flood cannot grow memory.
const PIPE_HEAD_CAP: usize = 64 * 1024;
const PIPE_TAIL_CAP: usize = 64 * 1024;

#[derive(Default)]
struct BoundedOut {
    head: Vec<u8>,
    tail: Vec<u8>,
    total: u64,
}

impl BoundedOut {
    fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len() as u64;
        let mut rest = chunk;
        if self.head.len() < PIPE_HEAD_CAP {
            let take = (PIPE_HEAD_CAP - self.head.len()).min(rest.len());
            self.head.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
        }
        if !rest.is_empty() {
            self.tail.extend_from_slice(rest);
            if self.tail.len() > PIPE_TAIL_CAP {
                self.tail.drain(..self.tail.len() - PIPE_TAIL_CAP);
            }
        }
    }

    fn dropped(&self) -> u64 {
        self.total - (self.head.len() + self.tail.len()) as u64
    }

    fn bytes(&self) -> Vec<u8> {
        let dropped = self.dropped();
        let mut out = Vec::with_capacity(self.head.len() + self.tail.len() + 64);
        out.extend_from_slice(&self.head);
        if dropped > 0 {
            out.extend_from_slice(
                format!(
                    "\n[...{dropped} bytes of child output omitted: bounded read keeps head+tail]\n"
                )
                .as_bytes(),
            );
        }
        out.extend_from_slice(&self.tail);
        out
    }
}

async fn drain_pipe<R>(r: Option<R>) -> BoundedOut
where
    R: AsyncReadExt + Unpin,
{
    let mut acc = BoundedOut::default();
    if let Some(mut r) = r {
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => acc.push(&buf[..n]),
            }
        }
    }
    acc
}

/// SIGKILL the whole process group (unix) plus the direct child (all
/// platforms), then reap. Killing an exited child errors; ignored.
async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // ponytail: one libc syscall instead of a `nix` dependency.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Drop guard: abandoning the run future (caller-side drop) still kills the
/// group; `kill_on_drop` alone would only reap the direct child.
struct GroupGuard {
    child: Option<tokio::process::Child>,
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(c) = self.child.as_ref() {
            if let Some(pid) = c.id() {
                unsafe {
                    libc::killpg(pid as libc::pid_t, libc::SIGKILL);
                }
            }
        }
    }
}

/// Tool-output bound: each pipe keeps 64KB head + 64KB tail (dropped middle
/// counted, `truncated` set), then `condense_output` keeps signal lines
/// anywhere in the retained output (noise dropped, ≤80 kept lines), then
/// `OUT_CAP` chars is the outer cap whose bool also reports a real cut (the
/// loop marks that result `[truncated]`); the `ok` verdict is exit-status on
/// the raw output, decided before condensing.
pub(crate) async fn run_allowed_argv(
    policy: &Policy,
    argv: &[String],
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<(bool, String, bool), ToolError> {
    // Canonical join: the allowlist sees exactly what exec will run, so a
    // quoted space can neither smuggle an arg past the check nor split one
    // apart after it. (An exact-allowlisted command with irregular internal
    // whitespace no longer matches; prefixes are unaffected.)
    let canonical = argv.join(" ");
    if !policy.allowed_commands.iter().any(|a| a == &canonical)
        && !prefix_allowed(&policy.allowed_prefixes, &canonical)
    {
        return Err(ToolError::Denied(format!(
            "command not allowlisted: {canonical}"
        )));
    }
    let mut guard = GroupGuard {
        child: Some(spawn_allowed(policy, argv)?),
    };
    let child = guard.child.as_mut().expect("child just spawned");
    let out_task = tokio::spawn(drain_pipe(child.stdout.take()));
    let err_task = tokio::spawn(drain_pipe(child.stderr.take()));
    enum End {
        Exited(std::process::ExitStatus),
        Timeout,
        Cancelled,
    }
    let child = guard.child.as_mut().expect("child just spawned");
    let end = tokio::select! {
        res = child.wait() => res
            .map(End::Exited)
            .map_err(|e| ToolError::Failed(e.to_string()))?,
        _ = tokio::time::sleep(timeout) => End::Timeout,
        _ = cancel.cancelled() => End::Cancelled,
    };
    let child = guard.child.as_mut().expect("child just spawned");
    let status = match end {
        End::Timeout => {
            kill_tree(child).await;
            let _ = tokio::join!(out_task, err_task);
            return Err(ToolError::Failed(format!("timeout after {timeout:?}")));
        }
        End::Cancelled => {
            kill_tree(child).await;
            let _ = tokio::join!(out_task, err_task);
            return Err(ToolError::Failed("cancelled".to_string()));
        }
        End::Exited(status) => status,
    };
    // Exit first, then join the readers: the pipes still hold buffered bytes
    // after the child is gone, and on unix the group kill above guarantees
    // no grandchild keeps them open.
    let out = out_task
        .await
        .map_err(|e| ToolError::Failed(format!("output reader failed: {e}")))?;
    let err = err_task
        .await
        .map_err(|e| ToolError::Failed(format!("output reader failed: {e}")))?;
    let mut s = String::from_utf8_lossy(&out.bytes()).to_string();
    let err_bytes = err.bytes();
    let err_text = String::from_utf8_lossy(&err_bytes);
    if !err_text.is_empty() {
        s.push_str("\n[stderr]\n");
        s.push_str(&err_text);
    }
    let ok = status.success();
    let (content, cap_cut) = cap_chars(condense_output(&s, None), OUT_CAP);
    Ok((ok, content, cap_cut || out.dropped() + err.dropped() > 0))
}

/// String-cmd entry point: split (no shell) then run the argv path, so the
/// allowlist and exec agree on the same argv.
pub(crate) async fn run_allowed(
    policy: &Policy,
    cmd: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<(bool, String, bool), ToolError> {
    let argv = split_cmd(cmd);
    run_allowed_argv(policy, &argv, timeout, cancel).await
}

static SYNTAX_N: AtomicUsize = AtomicUsize::new(0);

/// Edit-gate syntax check: candidate bytes go to a file inside a private
/// staging dir OUTSIDE the tree, `syntax_cmd + [file]` runs through the
/// `run_allowed_argv` gate (so the checker itself must be allowlisted) as
/// real argv — never joined into a string — and the staging dir is deleted
/// best-effort. A failing check vetoes the write; the file is untouched.
pub(crate) async fn check_syntax(
    policy: &Policy,
    candidate: &str,
    cancel: &CancellationToken,
) -> Result<(), ToolError> {
    let argv = policy.syntax_cmd.clone().unwrap_or_default();
    if argv.is_empty() {
        return Ok(());
    }
    stage_and_check(policy, &argv, candidate, cancel, &std::env::temp_dir()).await
}

/// Staging parent is a parameter so tests can point it at a path with a
/// space; production always passes the shared temp dir.
pub(crate) async fn stage_and_check(
    policy: &Policy,
    argv: &[String],
    candidate: &str,
    cancel: &CancellationToken,
    staging_parent: &Path,
) -> Result<(), ToolError> {
    let (dir, file) = stage_candidate(staging_parent, candidate).await?;
    let mut full = argv.to_vec();
    full.push(file.to_string_lossy().into_owned());
    let res = run_allowed_argv(policy, &full, EXEC_TIMEOUT, cancel).await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    match res {
        Ok((true, _, _)) => Ok(()),
        Ok((false, out, _)) => Err(ToolError::Failed(format!("syntax check failed: {out}"))),
        Err(e) => Err(e),
    }
}

/// Private staging dir (`0700` on unix) holding one `create_new` candidate
/// file, all through async fs so no executor thread blocks. `create_new`
/// fails rather than truncating a file that appeared between staging calls.
async fn stage_candidate(parent: &Path, candidate: &str) -> Result<(PathBuf, PathBuf), ToolError> {
    for _ in 0..8 {
        let dir = parent.join(format!(
            "tools-std-syntax-{}-{}",
            std::process::id(),
            SYNTAX_N.fetch_add(1, Ordering::SeqCst)
        ));
        match tokio::fs::create_dir(&dir).await {
            Ok(()) => {
                if let Err(e) = lock_down(&dir).await {
                    let _ = tokio::fs::remove_dir_all(&dir).await;
                    return Err(e);
                }
                let file = dir.join("candidate");
                let staged = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&file)
                    .await
                    .map_err(|e| ToolError::Failed(format!("cannot stage syntax candidate: {e}")));
                match staged {
                    Ok(mut f) => {
                        if let Err(e) = f.write_all(candidate.as_bytes()).await {
                            let _ = tokio::fs::remove_dir_all(&dir).await;
                            return Err(ToolError::Failed(format!(
                                "cannot stage syntax candidate: {e}"
                            )));
                        }
                        if let Err(e) = lock_down(&file).await {
                            let _ = tokio::fs::remove_dir_all(&dir).await;
                            return Err(e);
                        }
                        return Ok((dir, file));
                    }
                    Err(e) => {
                        let _ = tokio::fs::remove_dir_all(&dir).await;
                        return Err(e);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(ToolError::Failed(format!(
                    "cannot create syntax staging dir: {e}"
                )));
            }
        }
    }
    Err(ToolError::Failed(
        "could not claim a syntax staging dir".to_string(),
    ))
}

/// `0700` on unix (dir and candidate file alike); elsewhere creation modes
/// are umask-governed and there is nothing to tighten from here.
async fn lock_down(path: &Path) -> Result<(), ToolError> {
    #[cfg(unix)]
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|e| ToolError::Failed(format!("cannot lock down {path:?}: {e}")))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CmdArgs {
    pub(crate) cmd: String,
}

pub(crate) fn cmd_schema() -> Value {
    json!({
        "type": "object",
        "required": ["cmd"],
        "additionalProperties": false,
        "properties": {
            "cmd": {"type": "string", "maxLength": 8192}
        }
    })
}
