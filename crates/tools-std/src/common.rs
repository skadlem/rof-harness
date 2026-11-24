use serde_json::Value;
use std::time::Duration;
use tool_core::ToolError;

use crate::policy::{Policy, ToolPathError};

pub(crate) const VIEW_CAP: usize = 16_384;
pub(crate) const OUT_CAP: usize = 8_000;
pub(crate) const EDIT_FILE_CAP: usize = 512 * 1024;
pub(crate) const EDIT_REPLACE_CAP: usize = 256 * 1024;
/// Whole-file read bound for view/search. (chosen, no anchor: bounds
/// harness RAM at a survivable level — an allowlisted exec can mint
/// multi-GB files and a host OOM killed a sweep cell on 2026-10-08;
/// re-anchor if a measured task needs more.)
pub(crate) const READ_FILE_CAP: u64 = 64 * 1024 * 1024;
pub(crate) const EXEC_TIMEOUT: Duration = Duration::from_secs(300);

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

/// Tool description true to the policy: under `--exec-policy any` no list
/// applies, so the model must not be told the command is "allowlisted".
pub(crate) fn policy_wording(policy: &Policy, desc: &str) -> String {
    if policy.allow_any_cmd {
        desc.replace("one allowlisted command", "one command")
            .replace("an allowlisted check", "a check")
    } else {
        desc.to_string()
    }
}

/// Exact match or `prefix + " "` boundary match (`cargo test` covers
/// `cargo test foo`, never `cargo test-evil`).
pub fn prefix_allowed(prefixes: &[String], cmd: &str) -> bool {
    prefixes.iter().any(|p| {
        let p = p.trim();
        !p.is_empty() && (cmd == p || cmd.starts_with(&format!("{p} ")))
    })
}

pub(crate) fn cap_chars(s: String, n: usize) -> (String, bool) {
    if s.chars().count() <= n {
        (s, false)
    } else {
        (s.chars().take(n).collect(), true)
    }
}

pub(crate) fn path_err(e: ToolPathError) -> ToolError {
    match e {
        ToolPathError::Denied(m) => ToolError::Denied(m),
        // Recoverable contract error: Failed (retryable), never a denial.
        ToolPathError::MissingParent(m) => ToolError::Failed(m),
    }
}

pub(crate) fn parse_args<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T, ToolError> {
    serde_json::from_value(args.clone()).map_err(|e| ToolError::Failed(format!("bad args: {e}")))
}

/// Drop the first `n` bytes, rounding up to a char boundary so a leading
/// partial UTF-8 sequence never survives.
pub(crate) fn drop_front_bytes(s: &mut String, n: usize) {
    let cut = s.ceil_char_boundary(n.min(s.len()));
    s.drain(..cut);
}

/// Overwrite `p`. A file the tool cannot open for writing but whose directory
/// it owns (root-owned output of an in-container command; observed on
/// dna-insert and pypi-server) is replaced instead of failing the call.
/// The replacement gets default ownership and mode.
pub(crate) fn overwrite(p: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    match std::fs::write(p, bytes) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            std::fs::remove_file(p).map_err(|_| e)?;
            std::fs::write(p, bytes)
        }
        r => r,
    }
}
