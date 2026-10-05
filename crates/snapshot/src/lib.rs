//! Git overlay transactions over a task copy: baseline / diff / rollback.
//! Salvage of `~/rof-harness/src/engine/tree.rs` (identity, hooks off,
//! `--no-renames`, exact names, 8KiB/200-line patch caps). Git is never on
//! any command allowlist; rollback runs only when a retry follows, so the
//! final tree stays readable for post-mortem.
use serde::{Deserialize, Serialize};
use std::io::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::OnceLock;

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
    /// HEAD at [`TreeService::ensure`]: the ref [`TreeService::patch_since_start`]
    /// diffs from. Recorded once because `baseline()` moves HEAD per batch, so a
    /// diff against it forgets every earlier batch.
    start: OnceLock<String>,
}

impl TreeService {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            start: OnceLock::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Idempotent: `init` when no repo, one commit when HEAD is unborn
    /// (`checkout` refuses an unborn HEAD, so rollback needs that commit).
    /// Seeds the bytecode exclude before that commit and records the start
    /// HEAD for [`TreeService::patch_since_start`].
    pub fn ensure(&self) -> Result<()> {
        if !self.is_repo() {
            self.git(["init", "--quiet"])?;
        }
        self.exclude_bytecode()?;
        if self.head_unborn()? {
            self.commit_all("rof: initial tree state")?;
        }
        // Unborn HEAD (empty tree, nothing to commit) records no start:
        // there is no revision to diff from yet.
        let head = self.git_status(["rev-parse", "HEAD"])?;
        if head.status.success() {
            let _ = self
                .start
                .set(String::from_utf8_lossy(&head.stdout).trim().to_string());
        }
        Ok(())
    }

    /// `__pycache__/` + `*.pyc` in the copy-local `.git/info/exclude`:
    /// measured, baseline commits (`commit_all` = `git add -A`) were absorbing
    /// bytecode. Local to the copy — never tracked, never a global git setting.
    fn exclude_bytecode(&self) -> Result<()> {
        if !self.root.join(".git").is_dir() {
            return Ok(()); // gitfile (worktree/submodule): not this copy's repo dir
        }
        let info = self.root.join(".git/info");
        std::fs::create_dir_all(&info)?;
        let path = info.join("exclude");
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        for pat in ["__pycache__/", "*.pyc"] {
            if !text.lines().any(|l| l == pat) {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(pat);
                text.push('\n');
            }
        }
        std::fs::write(&path, text)?;
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
        self.diff_at(None)
    }

    /// The whole run's patch, not the last batch's: `diff`/`patch` semantics
    /// against the start HEAD recorded at [`TreeService::ensure`]. A per-batch
    /// `baseline()` commits what landed, so the index-based [`TreeService::diff`]
    /// reports an empty patch once a later batch follows.
    pub fn patch_since_start(&self) -> Result<(DiffSummary, PatchText)> {
        let start = self.start.get().ok_or_else(|| {
            other("patch_since_start: ensure() has not recorded a start HEAD".into())
        })?;
        let diff = self.diff_at(Some(start))?;
        let patch = self.patch_at(&diff, Some(start))?;
        Ok((diff, patch))
    }

    /// `rev = Some(start)`: commit-to-worktree diff; `None`: index-to-worktree
    /// (the per-batch baseline shape).
    fn diff_at(&self, rev: Option<&str>) -> Result<DiffSummary> {
        let mut changed: Vec<String> = Vec::new();
        if rev.is_some() {
            let out = self.git(["diff", "--name-only"].into_iter().chain(rev))?;
            changed.extend(
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(str::to_string),
            );
        }
        let porcelain = self.git(["status", "--porcelain", "--no-renames"])?;
        let text = String::from_utf8_lossy(&porcelain.stdout);
        let mut untracked: Vec<String> = Vec::new();
        for line in text.lines() {
            let Some((code, path)) = porcelain_pair(line) else {
                continue;
            };
            // Against a start rev the rev diff above owns tracked changes;
            // `?` rows are untracked in both modes.
            if rev.is_some() && !code.starts_with('?') {
                continue;
            }
            if !changed.contains(&path) {
                changed.push(path.clone());
            }
            if code.starts_with('?') && !untracked.contains(&path) {
                untracked.push(path);
            }
        }
        changed.sort();
        let stat = self.git(["diff", "--stat"].into_iter().chain(rev))?;
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
        self.patch_at(diff, None)
    }

    fn patch_at(&self, diff: &DiffSummary, rev: Option<&str>) -> Result<PatchText> {
        let raw = self.git(["diff", "--patch"].into_iter().chain(rev))?;
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

    /// Restore only the kept hunks: roll back to baseline, then re-apply
    /// each kept hunk's patch text in order. Any hunk that fails to apply
    /// is an error (do not silently skip); that path rolls back again, so
    /// `Err` always means the tree sits at baseline.
    ///
    /// Hunk-text format (restorable): one `git diff --patch` fragment per
    /// string — the file header that names the target (`--- a/<path>` /
    /// `+++ b/<path>`, optionally preceded by `diff --git`/`index` lines)
    /// directly followed by exactly one `@@` hunk. That header is the
    /// envelope `git apply` requires: a bare `@@` fragment names no file
    /// and git rejects it ("patch fragment without header"), which surfaces
    /// as `Err` here. A missing final newline is appended (git otherwise
    /// reports "corrupt patch").
    pub fn restore_hunks(&self, kept_hunks: &[String]) -> Result<DiffSummary> {
        self.rollback()?;
        for hunk in kept_hunks {
            if let Err(e) = self.apply_patch(hunk) {
                let _ = self.rollback(); // failed restore leaves the tree at baseline
                return Err(e);
            }
        }
        self.diff()
    }

    /// One fragment in via stdin: a patch file written into the tree would
    /// itself show up as untracked diff evidence.
    fn apply_patch(&self, hunk: &str) -> Result<()> {
        let mut patch = hunk.to_string();
        if !patch.ends_with('\n') {
            patch.push('\n');
        }
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C")
            .arg(&self.root)
            .args(["apply", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| other(format!("spawn git: {e}")))?;
        {
            use std::io::Write;
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| other("git apply: no stdin".into()))?;
            stdin
                .write_all(patch.as_bytes())
                .map_err(|e| other(format!("git apply stdin: {e}")))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| other(format!("wait git: {e}")))?;
        if out.status.success() {
            return Ok(());
        }
        Err(other(format!(
            "git apply: {} | {}",
            String::from_utf8_lossy(&out.stdout).trim_end(),
            String::from_utf8_lossy(&out.stderr).trim_end()
        )))
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
        // Benign no-op baselines: a clean tree, or dirt git refuses to stage
        // (e.g. modified content inside an embedded repo — the gitlink sha
        // never moves, so `add -A` stages nothing). Measured crash:
        // sanitize-git-repo pilot death (research/diag-snapshot-anomaly.md).
        if combined.contains("nothing to commit")
            || combined.contains("nothing added to commit")
            || combined.contains("no changes added to commit")
        {
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
    fn embedded_repo_dirt_is_a_benign_no_op_baseline() {
        // Regression: modified content inside a nested git repo stages as an
        // unchanged gitlink, so the baseline commit reports "no changes added
        // to commit" and (before this gate) killed the whole run.
        let dir = scratch("embed");
        let tree = TreeService::new(&dir);
        std::fs::write(dir.join("a.rs"), "one\n").unwrap();
        let inner = dir.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        let git = |args: &[&str], cwd: &Path| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {:?}", out);
        };
        git(&["init", "-q"], &inner);
        std::fs::write(inner.join("f.txt"), "x\n").unwrap();
        git(&["add", "f.txt"], &inner);
        git(
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "i",
            ],
            &inner,
        );
        tree.ensure().unwrap();
        std::fs::write(inner.join("f.txt"), "dirty\n").unwrap();
        tree.baseline().unwrap(); // was Err("git commit: ... no changes added to commit")
        tree.baseline().unwrap(); // idempotent
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

    #[test]
    fn patch_since_start_spans_every_baseline() {
        let dir = scratch("since-start");
        let tree = TreeService::new(&dir);
        std::fs::write(dir.join("a.rs"), "one\n").unwrap();
        tree.ensure().unwrap();

        // Two dispatched batches, each absorbed by its own baseline commit
        // (the per-Dispatch `tree.baseline()` shape).
        std::fs::write(dir.join("a.rs"), "two\n").unwrap();
        tree.baseline().unwrap();
        std::fs::write(dir.join("b.rs"), "new\n").unwrap();
        tree.baseline().unwrap();
        assert!(
            tree.diff().unwrap().is_empty(),
            "last baseline absorbed both batches"
        );

        let (d, p) = tree.patch_since_start().unwrap();
        assert_eq!(d.changed, vec!["a.rs", "b.rs"]);
        assert!(
            p.text.contains("-one") && p.text.contains("+two"),
            "{}",
            p.text
        );
        assert!(p.text.contains("b/b.rs"), "{}", p.text);
    }

    #[test]
    fn ensure_excludes_bytecode_from_commits() {
        let dir = scratch("bytecode");
        let tree = TreeService::new(&dir);
        std::fs::write(dir.join("a.py"), "print(1)\n").unwrap();
        std::fs::create_dir(dir.join("__pycache__")).unwrap();
        std::fs::write(dir.join("__pycache__/a.cpython-311.pyc"), "junk\n").unwrap();
        std::fs::write(dir.join("b.pyc"), "junk\n").unwrap();
        tree.ensure().unwrap();
        tree.baseline().unwrap();

        let ls = tree.git(["ls-tree", "-r", "--name-only", "HEAD"]).unwrap();
        let committed = String::from_utf8_lossy(&ls.stdout).into_owned();
        assert!(committed.contains("a.py"), "{committed}");
        assert!(
            !committed.contains(".pyc") && !committed.contains("__pycache__"),
            "bytecode reached the commit: {committed}"
        );
        assert!(
            tree.diff().unwrap().is_empty(),
            "junk stays out of evidence"
        );
    }

    /// One `git diff --patch` fragment per hunk: each file's header lines
    /// (`diff --git`/`index`/`---`/`+++`) lead its first `@@`, later `@@`
    /// lines repeat that header. Fragments end without a trailing newline —
    /// the shape a line-splitting caller hands over.
    fn split_patch(patch: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut header = String::new();
        let mut cur: Option<String> = None;
        for line in patch.lines() {
            if line.starts_with("diff --git") {
                if let Some(done) = cur.take() {
                    out.push(done);
                }
                header = line.to_string();
            } else if line.starts_with("@@") {
                if let Some(done) = cur.take() {
                    out.push(done);
                }
                cur = Some(format!("{header}\n{line}"));
            } else if let Some(hunk) = cur.as_mut() {
                hunk.push('\n');
                hunk.push_str(line);
            } else {
                header.push('\n');
                header.push_str(line);
            }
        }
        if let Some(done) = cur {
            out.push(done);
        }
        out
    }

    #[test]
    fn restore_hunks_keeps_only_the_prefix_hunk() {
        let dir = scratch("restore-prefix");
        let tree = TreeService::new(&dir);
        std::fs::write(dir.join("a.rs"), "one\ntwo\nthree\nfour\n").unwrap();
        std::fs::write(dir.join("b.rs"), "x\ny\nz\n").unwrap();
        tree.ensure().unwrap();
        tree.baseline().unwrap();

        // Both hunks uncommitted, as after a failed batch.
        std::fs::write(dir.join("a.rs"), "ONE\ntwo\nthree\nfour\n").unwrap();
        std::fs::write(dir.join("b.rs"), "x\ny\nZ\n").unwrap();
        let full = tree.patch(&tree.diff().unwrap()).unwrap();
        let hunks = split_patch(&full.text);
        assert_eq!(hunks.len(), 2);

        let d = tree.restore_hunks(&hunks[..1]).unwrap();
        assert!(!d.is_empty());
        assert_eq!(d.changed, vec!["a.rs"]); // hunk 2's file reverted
        assert_eq!(
            std::fs::read_to_string(dir.join("a.rs")).unwrap(),
            "ONE\ntwo\nthree\nfour\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("b.rs")).unwrap(),
            "x\ny\nz\n"
        );
    }

    #[test]
    fn restore_hunks_empty_equals_plain_rollback() {
        let dir = scratch("restore-empty");
        let tree = TreeService::new(&dir);
        std::fs::write(dir.join("a.rs"), "one\n").unwrap();
        tree.ensure().unwrap();
        tree.baseline().unwrap();

        std::fs::write(dir.join("a.rs"), "two\n").unwrap();
        std::fs::write(dir.join("made.rs"), "new\n").unwrap();
        let d = tree.restore_hunks(&[]).unwrap();
        assert!(d.is_empty());
        assert_eq!(std::fs::read_to_string(dir.join("a.rs")).unwrap(), "one\n");
        assert!(!dir.join("made.rs").exists());
    }

    #[test]
    fn restore_hunks_malformed_hunk_errors_and_restores_baseline() {
        let dir = scratch("restore-malformed");
        let tree = TreeService::new(&dir);
        std::fs::write(dir.join("a.rs"), "one\ntwo\nthree\nfour\n").unwrap();
        std::fs::write(dir.join("b.rs"), "x\ny\nz\n").unwrap();
        tree.ensure().unwrap();
        tree.baseline().unwrap();

        std::fs::write(dir.join("a.rs"), "ONE\ntwo\nthree\nfour\n").unwrap();
        std::fs::write(dir.join("b.rs"), "x\ny\nZ\n").unwrap();
        let full = tree.patch(&tree.diff().unwrap()).unwrap();
        let mut hunks = split_patch(&full.text);
        hunks.push("garbage, not a patch\n".to_string());

        let e = tree.restore_hunks(&hunks).unwrap_err();
        assert!(e.to_string().contains("git apply"), "{e}");
        // Chosen error state: rolled back to baseline, nothing half-restored.
        assert!(tree.diff().unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.join("a.rs")).unwrap(),
            "one\ntwo\nthree\nfour\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("b.rs")).unwrap(),
            "x\ny\nz\n"
        );
    }
}
