//! Git overlay transactions over a task copy: baseline / diff / rollback.
//! Fixed identity (`rof@local`), copied-in hooks off, rename detection off
//! (`--no-renames`), patch text cut to 8KiB / 200 lines. Git is never on
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

/// Error type for [`TreeService::patch_since_start_full`]: the same I/O
/// error the git-overlay helpers already return, named so the
/// deliverable-patch contract spells its failure mode.
pub type SnapshotError = Error;

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

    /// Bytecode + build dirs in the copy-local `.git/info/exclude`:
    /// measured, baseline commits (`commit_all` = `git add -A`) were absorbing
    /// bytecode; `cargo`/`npm` outputs would do the same. Local to the copy —
    /// never tracked, never a global git setting.
    fn exclude_bytecode(&self) -> Result<()> {
        if !self.root.join(".git").is_dir() {
            return Ok(()); // gitfile (worktree/submodule): not this copy's repo dir
        }
        let info = self.root.join(".git/info");
        std::fs::create_dir_all(&info)?;
        let path = info.join("exclude");
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        for pat in [
            "__pycache__/",
            "*.pyc",
            "target/",
            "node_modules/",
            "dist/",
            "build/",
        ] {
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

    /// The whole run's FULL patch for the deliverable path: the same start
    /// HEAD and tracked `diff --patch` as [`TreeService::patch_since_start`],
    /// but unbounded (no [`PATCH_MAX_BYTES`]/[`PATCH_MAX_LINES`] cut, no
    /// [`PATCH_TRUNCATED_MARKER`]) and with new-file CONTENTS for untracked
    /// paths via `git diff --no-index -- /dev/null <file>` instead of the
    /// stub-only headers the bounded model-evidence path emits. Unborn HEAD
    /// (no start recorded) stays `Err`, as today; the caller maps it.
    pub fn patch_since_start_full(&self) -> std::result::Result<String, SnapshotError> {
        let start = self.start.get().ok_or_else(|| {
            other("patch_since_start_full: ensure() has not recorded a start HEAD".into())
        })?;
        let diff = self.diff_at(Some(start))?;
        let raw = self.git(["diff", "--patch"].into_iter().chain(Some(start.as_str())))?;
        let mut text = String::from_utf8_lossy(&raw.stdout).into_owned();
        for path in &diff.untracked {
            for fragment in self.full_untracked_fragments(path)? {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&fragment);
                if !text.ends_with('\n') {
                    text.push('\n');
                }
            }
        }
        Ok(text)
    }

    /// Full-text fragments for one [`DiffSummary::untracked`] entry: a
    /// collapsed `dir/` entry expands via `ls-files --others` (which honours
    /// the copy-local exclude, so ignored junk never leaks into the patch)
    /// into one `diff --no-index` fragment per file; anything undiffable
    /// (vanished path, embedded-repo gitlink) falls back to a stub header,
    /// so evidence stays honest.
    fn full_untracked_fragments(&self, path: &str) -> Result<Vec<String>> {
        if self.root.join(path).is_dir() {
            let out = self.git(["ls-files", "--others", "--exclude-standard", "--", path])?;
            let mut fragments = Vec::new();
            for entry in String::from_utf8_lossy(&out.stdout).lines() {
                if entry.is_empty() {
                    continue;
                }
                if self.root.join(entry).is_file() {
                    fragments.push(self.diff_new_file(entry)?);
                } else {
                    fragments.push(self.untracked_stub(entry));
                }
            }
            if fragments.is_empty() {
                fragments.push(self.untracked_stub(path));
            }
            return Ok(fragments);
        }
        if self.root.join(path).is_file() {
            return Ok(vec![self.diff_new_file(path)?]);
        }
        Ok(vec![self.untracked_stub(path)])
    }

    /// New-file contents as a `diff --no-index` fragment against `/dev/null`
    /// (the empty-blob equivalent): handles text, no-trailing-newline, and
    /// binary ("Binary files differ") via git itself. `--no-index` exits 1
    /// on a real diff, so the unchecked status call is read by content: any
    /// stdout is the fragment; empty stdout means git refused.
    fn diff_new_file(&self, rel: &str) -> Result<String> {
        let out = self.git_status(["diff", "--no-index", "--patch", "--", "/dev/null", rel])?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        if !text.is_empty() {
            return Ok(text);
        }
        if self.root.join(rel).is_file() {
            return Err(other(format!(
                "git diff --no-index {rel}: {} | {}",
                String::from_utf8_lossy(&out.stdout).trim_end(),
                String::from_utf8_lossy(&out.stderr).trim_end()
            )));
        }
        // Raced away between `diff_at` and now: stub header, the only
        // non-restorable shape left (no `@@` for `split_patch`).
        Ok(self.untracked_stub(rel))
    }

    /// Fallback stub header for vanished/undiffable untracked paths (no `@@`,
    /// so `split_patch` yields nothing restorable and restore fails closed).
    /// Everything present on disk takes the `diff --no-index` hunk path above.
    fn untracked_stub(&self, path: &str) -> String {
        let shown = match std::fs::metadata(self.root.join(path)).map(|m| m.len()) {
            Ok(bytes) => format!(" (untracked, new file, {bytes} bytes)"),
            Err(_) => " (untracked, new file)".to_string(),
        };
        format!("--- /dev/null\n+++ b/{path}{shown}\n")
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

    /// The ONLY bounded diff text: `diff --patch` plus real new-file hunks
    /// for untracked paths (`git diff --no-index`, so `split_patch` /
    /// `restore_hunks` can replay them), cut to both caps.
    pub fn patch(&self, diff: &DiffSummary) -> Result<PatchText> {
        self.patch_at(diff, None)
    }

    fn patch_at(&self, diff: &DiffSummary, rev: Option<&str>) -> Result<PatchText> {
        let raw = self.git(["diff", "--patch"].into_iter().chain(rev))?;
        let mut text = String::from_utf8_lossy(&raw.stdout).into_owned();
        for path in &diff.untracked {
            for fragment in self.full_untracked_fragments(path)? {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&fragment);
                if !text.ends_with('\n') {
                    text.push('\n');
                }
            }
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
        let mut cmd = hermetic_git(&self.root);
        cmd.args(["apply", "-"])
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

    /// Benign no-op baselines commit nothing: a tree matching HEAD, or dirt
    /// git refuses to stage (modified content inside an embedded repo stages
    /// an unchanged gitlink, so `add -A` stages nothing). The staged-empty
    /// check owns that case, so a failed commit is always an error — no
    /// stdout text matching.
    fn commit_all(&self, message: &str) -> Result<()> {
        self.git(["add", "-A"])?;
        if self
            .git_status(["diff", "--cached", "--quiet"])
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Ok(());
        }
        self.git(["commit", "--quiet", "-m", message])?;
        Ok(())
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
        let mut cmd = hermetic_git(&self.root);
        cmd.args(args);
        cmd.output().map_err(|e| other(format!("spawn git: {e}")))
    }
}

/// Every git child the overlay spawns: the copy's baseline must not depend
/// on the host's git setup. Parent-inherited `GIT_DIR` (and friends) would
/// redirect commands into the wrong repo; system/global config could turn
/// on signing, hooks, fsmonitor, or an external diff. Command-line `-c`
/// flags outrank any config file git still reads.
///
/// // ponytail: hand-rolled `Command` setup instead of a git library to keep
/// the dependency list at serde only.
fn hermetic_git(root: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(root)
        // Copied-in hooks must not block the baseline; a host opt-in to
        // signing must not break overlay commits; fsmonitor would leak host
        // state into evidence. (No `diff.external` override: an empty value
        // does NOT disable it — git execs "" and every diff dies with
        // "cannot run". System/global config, the realistic leak vectors,
        // are nulled via env below; a fresh `init` writes no local driver.)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.fsmonitor=false",
        ])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_NAMESPACE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_config())
        // Fixed identity: reproducible on machines with empty git config.
        .env("GIT_AUTHOR_NAME", "rof")
        .env("GIT_AUTHOR_EMAIL", "rof@local")
        .env("GIT_COMMITTER_NAME", "rof")
        .env("GIT_COMMITTER_EMAIL", "rof@local")
        // Fixed locale: deterministic git diagnostics in errors.
        .env("LC_ALL", "C")
        .env("LANG", "C");
    cmd
}

/// Config-file sink for `GIT_CONFIG_GLOBAL`: the null device, so no user or
/// host global config is read. Unix primary; `NUL` elsewhere.
#[cfg(unix)]
fn null_config() -> &'static str {
    "/dev/null"
}

#[cfg(not(unix))]
fn null_config() -> &'static str {
    "NUL"
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

    /// Process env is process-wide: a test that mutates it would break a
    /// parallel test's raw `git` spawn mid-flight. Hold the guard in every
    /// test that sets process env AND in every test that spawns raw git.
    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn embedded_repo_dirt_is_a_benign_no_op_baseline() {
        // Regression: modified content inside a nested git repo stages as an
        // unchanged gitlink, so the baseline commit reports "no changes added
        // to commit" and (before this gate) killed the whole run.
        let _guard = env_guard(); // spawns raw git: serialize vs env-mutating tests
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

    #[test]
    fn baseline_stable_under_hostile_locale_env() {
        let _guard = env_guard();
        let dir = scratch("locale");
        let tree = TreeService::new(&dir);
        std::fs::write(dir.join("a.rs"), "one\n").unwrap();
        tree.ensure().unwrap();
        // Hostile parent locale: git children still run under C (see
        // `hermetic_git`), and the benign no-op baseline needs no output
        // parsing (staged-empty check), so locale cannot skew it.
        let old_lc = std::env::var("LC_ALL").ok();
        let old_lang = std::env::var("LANG").ok();
        std::env::set_var("LC_ALL", "xx_XX.UTF-8");
        std::env::set_var("LANG", "xx_XX.UTF-8");
        tree.baseline().unwrap();
        tree.baseline().unwrap(); // nothing to commit: benign, not an error
        match old_lc {
            Some(v) => std::env::set_var("LC_ALL", v),
            None => std::env::remove_var("LC_ALL"),
        }
        match old_lang {
            Some(v) => std::env::set_var("LANG", v),
            None => std::env::remove_var("LANG"),
        }
        assert!(tree.diff().unwrap().is_empty());
    }

    #[test]
    fn parent_git_dir_env_is_ignored() {
        let _guard = env_guard();
        let dir = scratch("gitdir-env");
        let tree = TreeService::new(&dir);
        std::fs::write(dir.join("a.rs"), "one\n").unwrap();
        // Hostile parent env: every overlay child strips GIT_DIR, so the
        // copy still baselines, diffs, and rolls back in place.
        let old = std::env::var("GIT_DIR").ok();
        std::env::set_var("GIT_DIR", "/nonexistent-git-dir");
        let outcome = (|| -> Result<()> {
            tree.ensure()?;
            tree.baseline()?;
            std::fs::write(dir.join("a.rs"), "two\n").unwrap();
            assert!(!tree.diff()?.is_empty());
            tree.rollback()?;
            assert_eq!(std::fs::read_to_string(dir.join("a.rs")).unwrap(), "one\n");
            Ok(())
        })();
        match old {
            Some(v) => std::env::set_var("GIT_DIR", v),
            None => std::env::remove_var("GIT_DIR"),
        }
        outcome.unwrap();
    }

    #[test]
    fn global_gpgsign_true_does_not_break_baseline() {
        let _guard = env_guard();
        let dir = scratch("gpgsign");
        std::fs::write(dir.join("a.rs"), "one\n").unwrap();
        std::fs::write(dir.join("signing.config"), "[commit]\n\tgpgsign = true\n").unwrap();
        // Hostile parent global config: without the hermetic `-c
        // commit.gpgsign=false` the baseline commit dies in gpg ("No secret
        // key"); the overlay must not depend on host signing setup.
        let old = std::env::var("GIT_CONFIG_GLOBAL").ok();
        std::env::set_var("GIT_CONFIG_GLOBAL", dir.join("signing.config"));
        let tree = TreeService::new(&dir);
        let outcome = (|| -> Result<()> {
            tree.ensure()?;
            tree.baseline()?;
            tree.baseline()?;
            Ok(())
        })();
        match old {
            Some(v) => std::env::set_var("GIT_CONFIG_GLOBAL", v),
            None => std::env::remove_var("GIT_CONFIG_GLOBAL"),
        }
        outcome.unwrap();
    }

    #[test]
    fn ensure_excludes_build_dirs_from_commits() {
        let dir = scratch("build-dirs");
        let tree = TreeService::new(&dir);
        std::fs::write(dir.join("a.rs"), "one\n").unwrap();
        for d in ["target", "node_modules", "dist", "build"] {
            let sub = dir.join(d);
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(sub.join("out.bin"), "junk\n").unwrap();
        }
        std::fs::create_dir_all(dir.join("inner/target")).unwrap();
        std::fs::write(dir.join("inner/target/nested.bin"), "junk\n").unwrap();
        tree.ensure().unwrap();
        tree.baseline().unwrap();

        let ls = tree.git(["ls-tree", "-r", "--name-only", "HEAD"]).unwrap();
        let committed = String::from_utf8_lossy(&ls.stdout).into_owned();
        assert!(committed.contains("a.rs"), "{committed}");
        for junk in ["out.bin", "nested.bin", "target", "node_modules", "dist"] {
            assert!(
                !committed.contains(junk),
                "build output reached the commit: {committed}"
            );
        }
        assert!(
            tree.diff().unwrap().is_empty(),
            "build output stays out of evidence"
        );
        let exclude = std::fs::read_to_string(dir.join(".git/info/exclude")).unwrap();
        for pat in ["target/", "node_modules/", "dist/", "build/"] {
            assert!(
                exclude.lines().any(|l| l == pat),
                "missing exclude {pat}: {exclude}"
            );
        }
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

    /// Big-tree fixture for the bound-vs-full pair: a 300-line tracked
    /// rewrite (blows both caps on its own) plus a 300-line untracked file
    /// with sentinel content. Returns the tree; the start HEAD is the
    /// `ensure()` commit, nothing baselined after.
    fn big_tree(name: &str) -> (PathBuf, TreeService) {
        let dir = scratch(name);
        let tree = TreeService::new(&dir);
        std::fs::write(dir.join("a.rs"), "one\n").unwrap();
        tree.ensure().unwrap();
        let tracked: String = (0..300)
            .map(|i| format!("tracked line {i:04} padding xxxxxxxxxxxxxxxxxxxx\n"))
            .collect();
        std::fs::write(dir.join("a.rs"), tracked).unwrap();
        let fresh: String = (0..300)
            .map(|i| format!("new-file-content line {i:04} padding yyyyyyyyyyyyyyyy\n"))
            .collect();
        std::fs::write(dir.join("fresh.rs"), fresh).unwrap();
        (dir, tree)
    }

    #[test]
    fn patch_since_start_full_is_unbounded_with_new_file_contents() {
        let (_dir, tree) = big_tree("full-big");
        let full = tree.patch_since_start_full().unwrap();
        assert!(
            full.len() > PATCH_MAX_BYTES,
            "full patch stays over the byte cap: {} bytes",
            full.len()
        );
        assert!(
            full.lines().count() > PATCH_MAX_LINES,
            "full patch stays over the line cap: {} lines",
            full.lines().count()
        );
        assert!(
            !full.contains(PATCH_TRUNCATED_MARKER),
            "no truncation marker on the full patch"
        );
        assert!(
            full.contains("tracked line 0299"),
            "tracked tail survives uncut"
        );
        assert!(
            full.contains("+new-file-content line 0299"),
            "untracked contents present, not stub-only:\n{full}"
        );
    }

    #[test]
    fn bounded_patch_since_start_still_truncates_same_tree() {
        let (_dir, tree) = big_tree("bounded-big");
        let (_, bounded) = tree.patch_since_start().unwrap();
        assert!(bounded.truncated, "bounded variant still cuts");
        assert!(
            bounded.text.contains(PATCH_TRUNCATED_MARKER),
            "bounded variant still marks the cut"
        );
        assert!(
            !bounded.text.contains("new-file-content line 0299"),
            "bounded cut drops the untracked tail"
        );
        // Same tree, same tracked bytes: the bounded whole-line prefix
        // (marker stripped) is a prefix of the unbounded text.
        let full = tree.patch_since_start_full().unwrap();
        let prefix = bounded
            .text
            .strip_suffix(&format!("{PATCH_TRUNCATED_MARKER}\n"))
            .unwrap();
        assert!(
            full.starts_with(prefix),
            "tracked bytes identical between paths"
        );
        assert!(full.len() > bounded.text.len(), "full is strictly longer");
    }

    #[test]
    fn patch_since_start_full_unborn_head_errors_like_bounded() {
        let dir = scratch("full-unborn");
        let tree = TreeService::new(&dir);
        tree.ensure().unwrap(); // empty tree: HEAD stays unborn, no start
        let bounded = tree.patch_since_start().unwrap_err();
        let full: SnapshotError = tree.patch_since_start_full().unwrap_err();
        for e in [&bounded, &full] {
            assert!(
                e.to_string().contains("has not recorded a start HEAD"),
                "unborn HEAD stays Err: {e}"
            );
        }
    }

    #[test]
    fn patch_embeds_new_file_hunk_restorable() {
        let dir = scratch("newfile-hunk");
        let tree = TreeService::new(&dir);
        std::fs::write(dir.join("a.rs"), "one\n").unwrap();
        tree.ensure().unwrap();
        tree.baseline().unwrap();

        std::fs::write(dir.join("made.rs"), "new-file-marker-9d2c\nsecond\n").unwrap();
        let diff = tree.diff().unwrap();
        assert_eq!(diff.untracked, vec!["made.rs"]);
        let p = tree.patch(&diff).unwrap();
        assert!(!p.truncated);
        assert!(
            p.text.contains("+new-file-marker-9d2c"),
            "contents, not stub-only: {}",
            p.text
        );
        assert!(
            p.text.contains("@@"),
            "split_patch needs a hunk header: {}",
            p.text
        );
        let hunks = split_patch(&p.text);
        assert_eq!(hunks.len(), 1, "{hunks:?}");
        // Full restore replays the creation (was: split yielded [] == rollback).
        let d = tree.restore_hunks(&hunks).unwrap();
        assert_eq!(d.changed, vec!["made.rs"]);
        assert_eq!(
            std::fs::read_to_string(dir.join("made.rs")).unwrap(),
            "new-file-marker-9d2c\nsecond\n"
        );
    }

    #[test]
    fn partial_prefix_with_new_file_restores_instead_of_failing() {
        let dir = scratch("partial-newfile");
        let tree = TreeService::new(&dir);
        std::fs::write(dir.join("a.rs"), "one\ntwo\nthree\nfour\n").unwrap();
        tree.ensure().unwrap();
        tree.baseline().unwrap();

        std::fs::write(dir.join("a.rs"), "ONE\ntwo\nthree\nfour\n").unwrap();
        std::fs::write(dir.join("made.rs"), "made-marker-51ef\n").unwrap();
        let hunks = split_patch(&tree.patch(&tree.diff().unwrap()).unwrap().text);
        assert_eq!(hunks.len(), 2, "{hunks:?}"); // was 1 + stub tail that failed closed
        assert!(hunks[0].contains("a.rs"), "tracked hunk first: {hunks:?}");
        assert!(hunks[1].contains("made.rs"), "{hunks:?}");
        // Keep only the tracked prefix: no apply error, new file reverted.
        let d = tree.restore_hunks(&hunks[..1]).unwrap();
        assert_eq!(d.changed, vec!["a.rs"]);
        assert!(!dir.join("made.rs").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("a.rs")).unwrap(),
            "ONE\ntwo\nthree\nfour\n"
        );
        // Keep both: the Partial-kept new file restores with contents.
        let d = tree.restore_hunks(&hunks).unwrap();
        assert_eq!(d.changed, vec!["a.rs", "made.rs"]);
        assert_eq!(
            std::fs::read_to_string(dir.join("made.rs")).unwrap(),
            "made-marker-51ef\n"
        );
    }

    #[test]
    fn patch_since_start_bounded_carries_new_file_contents() {
        let dir = scratch("since-newfile");
        let tree = TreeService::new(&dir);
        std::fs::write(dir.join("a.rs"), "one\n").unwrap();
        tree.ensure().unwrap();
        std::fs::write(dir.join("a.rs"), "two\n").unwrap();
        tree.baseline().unwrap();
        std::fs::write(dir.join("made.rs"), "since-marker-3b77\n").unwrap();
        let (d, p) = tree.patch_since_start().unwrap();
        assert!(!p.truncated);
        assert_eq!(d.changed, vec!["a.rs", "made.rs"]);
        assert!(
            p.text.contains("+two") && p.text.contains("+since-marker-3b77"),
            "tracked edit plus new-file contents: {}",
            p.text
        );
        assert_eq!(split_patch(&p.text).len(), 2, "{}", p.text);
        // Whole-run hunks are start-relative (not baseline-relative), so the
        // restorable check stays on the per-batch patch: it carries the same
        // new-file hunk and replays it from baseline.
        let batch = tree.patch(&tree.diff().unwrap()).unwrap();
        let hunks = split_patch(&batch.text);
        assert_eq!(hunks.len(), 1, "{hunks:?}");
        tree.restore_hunks(&hunks).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("made.rs")).unwrap(),
            "since-marker-3b77\n"
        );
    }
}
