//! Git overlay transactions over a task copy: baseline / diff / rollback.
//! Salvage of `~/rof-harness/src/engine/tree.rs` (identity, hooks off,
//! `--no-renames`, exact names, 8KiB/200-line patch caps). Git is never on
//! any command allowlist; rollback runs only when a retry follows, so the
//! final tree stays readable for post-mortem.
use serde::{Deserialize, Serialize};
use std::io::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Output;

/// Same change set as [`TreeService::diff`]: changed paths, stat evidence,
/// plus the untracked subset (status alone would miss created files).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffSummary {
    pub changed: Vec<String>,
    pub stat: String,
    pub untracked: Vec<String>,
}

impl DiffSummary {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
    }
}

/// Patch-text bounds: either cap alone leaves a hole (many tiny lines fit
/// in few bytes; one minified line is one line however long).
pub const PATCH_MAX_BYTES: usize = 8 * 1024;
pub const PATCH_MAX_LINES: usize = 200;
/// Appended on a cut so the text is honest on its own, in a pane and in JSONL.
pub const PATCH_TRUNCATED_MARKER: &str = "… [diff truncated: harness evidence bound reached]";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PatchText {
    pub text: String,
    pub truncated: bool,
}

pub struct TreeService {
    root: PathBuf,
}

impl TreeService {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Idempotent: `init` when no repo, one commit when HEAD is unborn
    /// (`checkout` refuses an unborn HEAD, so rollback needs that commit).
    pub fn ensure(&self) -> Result<()> {
        if !self.is_repo() {
            self.git(["init", "--quiet"])?;
        }
        if self.head_unborn()? {
            self.commit_all("rof: initial tree state")?;
        }
        Ok(())
    }

    /// Commits the tree the next attempt starts from and rolls back to.
    pub fn baseline(&self) -> Result<()> {
        self.commit_all("rof: attempt baseline")
    }

    /// Restores tracked files, drops created ones. Only when a retry follows.
    pub fn rollback(&self) -> Result<()> {
        self.git(["checkout", "--", "."])?;
        self.git(["clean", "-fdq"])?;
        Ok(())
    }

    /// Porcelain names (untracked included) + `diff --stat` with one
    /// "new file" line per untracked path. `--no-renames`: rename detection
    /// reports a moved oracle test under its new (clean) name; off, the same
    /// move reads as delete + new file and the delete is caught.
    pub fn diff(&self) -> Result<DiffSummary> {
        let porcelain = self.git(["status", "--porcelain", "--no-renames"])?;
        let text = String::from_utf8_lossy(&porcelain.stdout);
        let mut changed: Vec<String> = Vec::new();
        let mut untracked: Vec<String> = Vec::new();
        for line in text.lines() {
            let Some((code, path)) = porcelain_pair(line) else {
                continue;
            };
            if !changed.contains(&path) {
                changed.push(path.clone());
            }
            if code.starts_with('?') && !untracked.contains(&path) {
                untracked.push(path);
            }
        }
        changed.sort();
        let stat = self.git(["diff", "--stat"])?;
        let mut stat = String::from_utf8_lossy(&stat.stdout).into_owned();
        for path in &untracked {
            stat.push_str(&format!(" {path} | new file\n"));
        }
        untracked.sort();
        Ok(DiffSummary {
            changed,
            stat,
            untracked,
        })
    }

    /// The ONLY diff text produced: `diff --patch` plus one evidence line
    /// per untracked path (empty in `git diff`), cut to both caps.
    pub fn patch(&self, diff: &DiffSummary) -> Result<PatchText> {
        let raw = self.git(["diff", "--patch"])?;
        let mut text = String::from_utf8_lossy(&raw.stdout).into_owned();
        for path in &diff.untracked {
            let shown = match std::fs::metadata(self.root.join(path)).map(|m| m.len()) {
                Ok(bytes) => format!(" (untracked, new file, {bytes} bytes)"),
                Err(_) => " (untracked, new file)".to_string(),
            };
            text.push_str(&format!("--- /dev/null\n+++ b/{path}{shown}\n"));
        }
        Ok(bound_patch(&text))
    }

    fn is_repo(&self) -> bool {
        self.root.join(".git").exists()
    }

    fn head_unborn(&self) -> Result<bool> {
        Ok(!self.git_ok(["rev-parse", "--quiet", "--verify", "HEAD"]))
    }

    /// Tolerates the one benign failure: a tree matching HEAD has nothing
    /// to commit, and HEAD already is the baseline then.
    fn commit_all(&self, message: &str) -> Result<()> {
        self.git(["add", "-A"])?;
        let out = self.git_status(["commit", "--quiet", "-m", message])?;
        if out.status.success() {
            return Ok(());
        }
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if combined.contains("nothing to commit") || combined.contains("nothing added to commit") {
            return Ok(());
        }
        Err(other(format!("git commit: {}", combined.trim_end())))
    }

    fn git<'a>(&self, args: impl IntoIterator<Item = &'a str>) -> Result<Output> {
        let out = self.git_status(args)?;
        if !out.status.success() {
            return Err(other(format!(
                "git: {} | {}",
                String::from_utf8_lossy(&out.stdout).trim_end(),
                String::from_utf8_lossy(&out.stderr).trim_end()
            )));
        }
        Ok(out)
    }

    fn git_ok<'a>(&self, args: impl IntoIterator<Item = &'a str>) -> bool {
        self.git_status(args)
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn git_status<'a>(&self, args: impl IntoIterator<Item = &'a str>) -> Result<Output> {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C")
            .arg(&self.root)
            .args(["-c", "core.hooksPath=/dev/null"]) // copied-in hooks must not block the baseline
            .args(args)
            // Fixed identity: reproducible on machines with empty git config.
            .env("GIT_AUTHOR_NAME", "rof")
            .env("GIT_AUTHOR_EMAIL", "rof@local")
            .env("GIT_COMMITTER_NAME", "rof")
            .env("GIT_COMMITTER_EMAIL", "rof@local");
        cmd.output().map_err(|e| other(format!("spawn git: {e}")))
    }
}

fn other(msg: String) -> Error {
    Error::other(msg)
}

/// Whole lines only: a line that does not fit is dropped, so a multi-byte
/// codepoint is never sliced.
fn bound_patch(text: &str) -> PatchText {
    let mut out = String::new();
    let mut truncated = false;
    for (lines, line) in text.split_inclusive('\n').enumerate() {
        if lines >= PATCH_MAX_LINES || out.len() + line.len() > PATCH_MAX_BYTES {
            truncated = true;
            break;
        }
        out.push_str(line);
    }
    if truncated {
        out.push_str(PATCH_TRUNCATED_MARKER);
        out.push('\n');
    }
    PatchText {
        text: out,
        truncated,
    }
}

/// Splits a porcelain line into status code + path: new name of a rename,
/// unquoted (git C-quotes spaces/high bytes; recall needs the exact name).
fn porcelain_pair(line: &str) -> Option<(&str, String)> {
    let bytes = line.as_bytes();
    if bytes.len() < 3 || bytes[2] != b' ' {
        return None;
    }
    let code = std::str::from_utf8(&bytes[..2]).ok()?;
    let path = &line[3..];
    let path = path.split(" -> ").last().unwrap_or(path);
    Some((code, unquote(path)))
}

fn unquote(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'"' || *bytes.last().unwrap() != b'"' {
        return path.to_string();
    }
    let body = &bytes[1..bytes.len() - 1];
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            b'\\' if i + 1 < body.len() => {
                match body[i + 1] {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'"' | b'\\' => out.push(body[i + 1]),
                    d if d.is_ascii_digit() => {
                        let mut v: u16 = (d - b'0') as u16;
                        let end = (i + 4).min(body.len());
                        let mut j = i + 2;
                        while j < end && body[j].is_ascii_digit() {
                            v = v * 8 + (body[j] - b'0') as u16;
                            j += 1;
                        }
                        out.push(v as u8);
                        i = j;
                        continue;
                    }
                    c => {
                        out.push(b'\\');
                        out.push(c);
                    }
                }
                i += 2;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rof-snap-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn baseline_modify_diff_detects_changed_and_untracked() {
        let dir = scratch("diff");
        let tree = TreeService::new(&dir);
        std::fs::write(dir.join("a.rs"), "one\n").unwrap();
        tree.ensure().unwrap();
        tree.baseline().unwrap();
        assert!(tree.diff().unwrap().is_empty());

        std::fs::write(dir.join("a.rs"), "two\n").unwrap();
        std::fs::write(dir.join("b.rs"), "new\n").unwrap();
        let d = tree.diff().unwrap();
        assert_eq!(d.changed, vec!["a.rs", "b.rs"]);
        assert_eq!(d.untracked, vec!["b.rs"]);
        assert!(d.stat.contains("b.rs"), "new file named: {}", d.stat);
        let p = tree.patch(&d).unwrap();
        assert!(p.text.contains("b/b.rs"));
    }

    #[test]
    fn rollback_restores_byte_identical_tree() {
        let dir = scratch("rollback");
        let tree = TreeService::new(&dir);
        std::fs::write(dir.join("a.rs"), "one\n").unwrap();
        tree.ensure().unwrap();
        tree.baseline().unwrap();
        let before = std::fs::read(dir.join("a.rs")).unwrap();

        std::fs::write(dir.join("a.rs"), "two\n").unwrap();
        std::fs::write(dir.join("b.rs"), "new\n").unwrap();
        tree.rollback().unwrap();
        assert_eq!(std::fs::read(dir.join("a.rs")).unwrap(), before);
        assert!(!dir.join("b.rs").exists());
        assert!(tree.diff().unwrap().is_empty());
    }

    #[test]
    fn ensure_on_unborn_repo_commits() {
        let dir = scratch("unborn");
        std::fs::write(dir.join("a.rs"), "one\n").unwrap();
        let tree = TreeService::new(&dir);
        tree.git(["init", "--quiet"]).unwrap(); // HEAD unborn: no commit
        tree.ensure().unwrap(); // makes the commit the source skipped
        assert!(tree.git_ok(["rev-parse", "--verify", "HEAD"]));
        tree.baseline().unwrap();
        std::fs::write(dir.join("a.rs"), "two\n").unwrap();
        assert_eq!(tree.diff().unwrap().changed, vec!["a.rs"]);
        tree.rollback().unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.rs")).unwrap(), "one\n");
    }

    #[test]
    fn double_baseline_is_harmless() {
        let dir = scratch("clean");
        let tree = TreeService::new(&dir);
        tree.ensure().unwrap();
        tree.baseline().unwrap();
        tree.baseline().unwrap(); // nothing to commit: benign, not an error
        assert!(tree.diff().unwrap().is_empty());
    }
}
