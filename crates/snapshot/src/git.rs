use crate::patch::bound_patch;
use crate::types::{DiffSummary, PatchText, SnapshotError, WorkdirState};
use std::io::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::OnceLock;

/// Fail-closed workdir probe: anything git cannot confirm clean reads as
/// [`WorkdirState::Dirty`]; only a missing `.git` reads as
/// [`WorkdirState::NotARepo`].
pub fn workdir_state(root: &Path) -> WorkdirState {
    if !root.join(".git").exists() {
        return WorkdirState::NotARepo;
    }
    match hermetic_git(root)
        .args(["status", "--porcelain", "--no-renames"])
        .output()
    {
        Ok(out)
            if out.status.success() && String::from_utf8_lossy(&out.stdout).trim().is_empty() =>
        {
            WorkdirState::Clean
        }
        _ => WorkdirState::Dirty,
    }
}

pub struct TreeService {
    root: PathBuf,
    /// HEAD at [`TreeService::ensure`]: the ref [`TreeService::patch_since_start`]
    /// diffs from. Recorded once because `baseline()` moves HEAD per batch, so a
    /// diff against it forgets every earlier batch.
    start: OnceLock<String>,
    /// Copy-local ignore list written to `.git/info/exclude` by
    /// [`TreeService::ensure`].
    excludes: Vec<String>,
}

/// Copy-local ignore defaults: bytecode and package-manager outputs that
/// must never reach the baseline commit or the evidence. `build/` and
/// `dist/` are deliberately NOT here: new source files there must reach the
/// patch, so silencing them would drop deliverable content.
const DEFAULT_EXCLUDES: &[&str] = &["__pycache__/", "*.pyc", "target/", "node_modules/"];

impl TreeService {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            start: OnceLock::new(),
            excludes: DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Override the copy-local ignore list (replaces the defaults): e.g. a
    /// task whose build output must stay out of evidence re-adds `build/`.
    pub fn with_excludes(mut self, excludes: &[&str]) -> Self {
        self.excludes = excludes.iter().map(|s| s.to_string()).collect();
        self
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

    /// Copy-local ignores in `.git/info/exclude`: baseline commits
    /// (`commit_all` = `git add -A`) would otherwise absorb bytecode and
    /// package-manager outputs. Local to the copy — never tracked, never a
    /// global git setting.
    ///
    /// // ponytail: append-only line merge instead of `.gitignore` handling,
    /// so no ignore-parser dependency is needed.
    fn exclude_bytecode(&self) -> Result<()> {
        if !self.root.join(".git").is_dir() {
            return Ok(()); // gitfile (worktree/submodule): not this copy's repo dir
        }
        let info = self.root.join(".git/info");
        std::fs::create_dir_all(&info)?;
        let path = info.join("exclude");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        // Stale harness lines (`build/`, `dist/` were defaults before they
        // started hiding new deliverable files) are dropped unless the caller
        // re-added them via `with_excludes`; every other existing line is
        // kept, then missing configured patterns are appended.
        let mut lines: Vec<String> = text
            .lines()
            .filter(|l| (*l != "build/" && *l != "dist/") || self.excludes.iter().any(|e| e == *l))
            .map(str::to_string)
            .collect();
        for pat in &self.excludes {
            if !lines.iter().any(|l| l == pat) {
                lines.push(pat.clone());
            }
        }
        let mut out = lines.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        std::fs::write(&path, out)?;
        Ok(())
    }

    /// Commits the tree the next attempt starts from and rolls back to.
    pub fn baseline(&self) -> Result<()> {
        self.commit_all("rof: attempt baseline")
    }

    /// Restores tracked files, drops created ones. Only when a retry follows.
    /// Requires [`TreeService::ensure`] on this instance first: without a
    /// recorded start HEAD there is no known-good state, so rolling back
    /// could destroy work against an unknown base.
    pub fn rollback(&self) -> Result<()> {
        if self.start.get().is_none() {
            return Err(other(
                "rollback: ensure() has not recorded a start HEAD".into(),
            ));
        }
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
    /// but unbounded (no [`crate::PATCH_MAX_BYTES`]/[`crate::PATCH_MAX_LINES`] cut, no
    /// [`crate::PATCH_TRUNCATED_MARKER`]) and with new-file CONTENTS for untracked
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
mod tests;
