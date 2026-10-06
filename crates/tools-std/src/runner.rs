use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tool_core::ToolError;
use verify::condense_output;

use crate::common::{cap_chars, prefix_allowed, EXEC_TIMEOUT, OUT_CAP};
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

/// Tool-output bound: `verify::condense_output` keeps signal lines anywhere in the full output (noise dropped, ≤80 kept lines), then `OUT_CAP` chars is the outer cap whose bool reports a real cut (the loop marks that result `[truncated]`); the `ok` verdict is exit-status on the raw output, decided before condensing.
pub(crate) async fn run_allowed(
    policy: &Policy,
    cmd: &str,
    timeout: Duration,
) -> Result<(bool, String, bool), ToolError> {
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
    let (content, cap_cut) = cap_chars(condense_output(&s, None), OUT_CAP);
    Ok((ok, content, cap_cut))
}

static SYNTAX_N: AtomicUsize = AtomicUsize::new(0);

/// Edit-gate syntax check: candidate bytes go to a tempfile OUTSIDE the
/// tree, `syntax_cmd + tempfile` runs through the existing `run_allowed`
/// gate (so the checker itself must be allowlisted), tempfile deleted
/// best-effort. A failing check vetoes the write; the file is untouched.
pub(crate) async fn check_syntax(policy: &Policy, candidate: &str) -> Result<(), ToolError> {
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
        Ok((true, _, _)) => Ok(()),
        Ok((false, out, _)) => Err(ToolError::Failed(format!("syntax check failed: {out}"))),
        Err(e) => Err(e),
    }
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
