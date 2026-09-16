//! Git as the task copy's tree-state substrate (§4.2): present always,
//! harness-side only.
//!
//! Every implementer attempt starts from a tree the harness committed, and the
//! write gate reads what git reports changed — so "did the tree change?" is
//! ground truth rather than the model's self-reported `writes[]`. The same
//! change set feeds the recall metric (§4.4): one substrate, two reads. `git`
//! is never on `ROF_ALLOW_CMDS` and `.git` is excluded from every path an agent
//! can name, so the substrate is write-only to the harness.

use std::path::{Path, PathBuf};
use std::process::Output;

/// Above this the source `.git` is cloned shallow (`--depth 1`) rather than
/// copied: rollback and the write gate track the working tree, not ancestry, so
/// the copy cost comes off the history axis (§4.2, Q8). Below it a verbatim copy
/// is cheaper and keeps full history — free ancestry for a check that turns out
/// to want it (§8).
const SHALLOW_GIT_BYTES: u64 = 8 * 1024 * 1024;

/// Commits the harness makes carry a fixed identity, so a run is reproducible
/// on a machine whose git config is empty and its diffs stay comparable.
const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "rof"),
    ("GIT_AUTHOR_EMAIL", "rof@local"),
    ("GIT_COMMITTER_NAME", "rof"),
    ("GIT_COMMITTER_EMAIL", "rof@local"),
];

/// Neutralises any hook copied in with a repo: a source's pre-commit contract
/// is not a contract the harness signed, and a failing one would block the
/// baseline a round depends on. A path that cannot hold hooks works everywhere.
const HOOKS_OFF: &str = "/dev/null";

/// The task copy, plus the git operations the harness performs on it.
pub struct TreeService {
    root: PathBuf,
}

/// What git says changed vs the baseline commit — one read for the write gate
/// (§4.2) and one for recall (§4.4), both off the same names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeDiff {
    /// Changed paths, new and deleted files included, gitignored files not.
    pub names: Vec<String>,
    /// `git diff --stat` plus one line per new file — the evidence a report or
    /// a reviewer quotes.
    pub stat: String,
}

impl TreeDiff {
    /// Ground truth for `writes_made`: how many paths the attempt touched.
    pub fn changed(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

impl TreeService {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `.git` is present — copied in (§4.2) or made by [`Self::ensure`].
    pub fn is_repo(&self) -> bool {
        self.root.join(".git").exists()
    }

    /// Idempotent: the copy is a repo with a commit on HEAD, whatever the source
    /// was. A copied repo keeps its history (shallow when large); a non-repo
    /// gets `git init` plus one commit; an unborn HEAD gets the commit the
    /// source never made, because `git checkout` refuses an unborn one and
    /// rollback must never silently degrade on a degenerate input.
    pub fn ensure(&self) -> anyhow::Result<()> {
        if !self.is_repo() {
            self.git(["init", "--quiet"])?;
        }
        if self.head_unborn()? {
            self.commit_all("rof: initial tree state")?;
        }
        Ok(())
    }

    /// Records the state the next attempt starts from and rolls back to.
    pub fn baseline(&self) -> anyhow::Result<()> {
        self.commit_all("rof: attempt baseline")
    }

    /// Restores tracked files to the baseline and drops the files the attempt
    /// created. Ignored paths (`target/`) survive: build output the next
    /// attempt's checks still need. Called only when a retry follows, so the
    /// final state of a task stays in the copy for post-mortem reading.
    pub fn rollback(&self) -> anyhow::Result<()> {
        self.git(["checkout", "--", "."])?;
        self.git(["clean", "-fdq"])?;
        Ok(())
    }

    /// The change set vs the baseline. `git status` is the source of truth for
    /// the names: `git diff` alone misses files the attempt created, and a
    /// model that wrote a new file must not read back "no writes".
    pub fn diff(&self) -> anyhow::Result<TreeDiff> {
        let porcelain = self.git(["status", "--porcelain"])?;
        let text = String::from_utf8_lossy(&porcelain.stdout);
        let mut names: Vec<String> = Vec::new();
        let mut new_files: Vec<String> = Vec::new();
        for line in text.lines() {
            let Some((code, path)) = porcelain_pair(line) else {
                continue;
            };
            if !names.contains(&path) {
                names.push(path.clone());
            }
            if code.starts_with('?') && !new_files.contains(&path) {
                new_files.push(path);
            }
        }
        let stat = self.git(["diff", "--stat"])?;
        let mut stat = String::from_utf8_lossy(&stat.stdout).into_owned();
        for path in &new_files {
            // `diff --stat` cannot see untracked files; name them so the
            // evidence line matches the name set.
            stat.push_str(&format!(" {path} | new file\n"));
        }
        names.sort();
        Ok(TreeDiff { names, stat })
    }

    /// True when HEAD names no commit — `git rev-parse` refuses it rather than
    /// printing an empty string.
    fn head_unborn(&self) -> anyhow::Result<bool> {
        Ok(!self.git_ok(["rev-parse", "--quiet", "--verify", "HEAD"]))
    }

    /// Stages everything and commits, tolerating the one benign failure: a tree
    /// that matches HEAD has nothing to commit, which is exactly the state a
    /// baseline wants, so HEAD already *is* the baseline.
    fn commit_all(&self, message: &str) -> anyhow::Result<()> {
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
        anyhow::bail!(
            "git commit in {}: {}",
            self.root.display(),
            combined.trim_end()
        );
    }

    /// Runs git; a non-zero exit is an error, because the substrate is a hard
    /// dependency (§4.2): a git that fails is reported, not papered over.
    fn git<'a>(&self, args: impl IntoIterator<Item = &'a str>) -> anyhow::Result<Output> {
        let out = self.git_status(args)?;
        if !out.status.success() {
            anyhow::bail!(
                "git in {}: {} | {}",
                self.root.display(),
                String::from_utf8_lossy(&out.stdout).trim_end(),
                String::from_utf8_lossy(&out.stderr).trim_end()
            );
        }
        Ok(out)
    }

    /// Runs git and reports whether it succeeded, without treating non-zero as
    /// an error — for the few questions where the exit code *is* the answer.
    fn git_ok<'a>(&self, args: impl IntoIterator<Item = &'a str>) -> bool {
        self.git_status(args)
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    fn git_status<'a>(&self, args: impl IntoIterator<Item = &'a str>) -> anyhow::Result<Output> {
        let args = args.into_iter().collect::<Vec<&str>>();
        let mut cmd = self.command(&args)?;
        cmd.output()
            .map_err(|e| anyhow::anyhow!("git in {}: {}", self.root.display(), e))
    }

    fn command(&self, args: &[&str]) -> anyhow::Result<std::process::Command> {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(&self.root);
        cmd.args(["-c", &format!("core.hooksPath={HOOKS_OFF}")]);
        cmd.args(args);
        for (key, value) in IDENTITY {
            cmd.env(key, value);
        }
        Ok(cmd)
    }
}

/// The source repo's state, cloned shallow (`--depth 1`) when its history is
/// large: rollback and the write gate track the working tree, not ancestry, so
/// the copy cost stays off the history axis (§4.2, Q8). Below the threshold a
/// verbatim copy is cheaper and keeps full history — free ancestry for a check
/// that turns out to want it (§8). The working tree is already copied exact
/// (uncommitted and untracked files included), so the clone skips its own
/// checkout and keeps only the shallow objects.
///
/// Called by `eval::runner::copy_tree` on a task copy that just received the
/// source's working tree.
pub(crate) fn copy_git_state(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let git_dir = src.join(".git");
    if dir_size(&git_dir) <= SHALLOW_GIT_BYTES {
        return copy_dir(&git_dir, &dst.join(".git"));
    }
    // `git clone` ignores `--depth` for local paths (it hardlinks instead), so
    // the transport is forced with `--no-local`; cloning into a path ending in
    // `.git` would make a bare repo, so the clone lands in a scratch dir and
    // only its `.git` moves into the copy.
    let scratch = dst
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".rof-git-{}", uuid::Uuid::new_v4().simple()));
    let cloned = std::process::Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            "--no-local",
            "--no-checkout",
            "--quiet",
        ])
        .arg(src)
        .arg(&scratch)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    let moved = cloned && std::fs::rename(scratch.join(".git"), dst.join(".git")).is_ok();
    let _ = std::fs::remove_dir_all(&scratch);
    if moved {
        return Ok(());
    }
    // A repo git cannot clone shallow (unborn HEAD, odd setup) still serves
    // rollback and diff with its history copied in full.
    copy_dir(&git_dir, &dst.join(".git"))
}

/// Byte size of a directory, walked: the input to the shallow/verbatim choice.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            match entry.file_type() {
                Ok(ft) if ft.is_file() => total += entry.metadata().map(|m| m.len()).unwrap_or(0),
                Ok(ft) if ft.is_dir() => stack.push(entry.path()),
                _ => {}
            }
        }
    }
    total
}

/// Recursive copy with no skips: every entry of `.git` is repo state.
fn copy_dir(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Splits a `git status --porcelain` line into its status code and the path,
/// taking the new name of a rename and unquoting a quoted path — git C-quotes
/// paths with spaces or high bytes, and recall needs the exact name (§4.4).
fn porcelain_pair(line: &str) -> Option<(&str, String)> {
    let bytes = line.as_bytes();
    if bytes.len() < 3 || bytes[2] != b' ' {
        return None;
    }
    // Codes are two ASCII bytes; the path starts at byte 3, on a char boundary.
    let code = std::str::from_utf8(&bytes[..2]).ok()?;
    let path = &line[3..];
    let path = path.split(" -> ").last().unwrap_or(path);
    Some((code, unquote(path)))
}

/// Reverses git's C-style path quoting when present, so `"q file.rs"` and
/// `"\303\274.rs"` come back as the paths on disk.
fn unquote(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'"' || *bytes.last().unwrap() != b'"' {
        return path.to_string();
    }
    // The surrounding quotes are one byte each, so slicing between them stays
    // on char boundaries.
    let inner = &path[1..path.len() - 1];
    let body = inner.as_bytes();
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            b'\\' if i + 1 < body.len() => {
                let next = body[i + 1];
                match next {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'"' | b'\\' => out.push(next),
                    d if d.is_ascii_digit() => {
                        // Up to three octal digits, as git writes them.
                        let mut value: u16 = (d - b'0') as u16;
                        let end = (i + 4).min(body.len());
                        let mut j = i + 2;
                        while j < end && body[j].is_ascii_digit() {
                            value = value * 8 + (body[j] - b'0') as u16;
                            j += 1;
                        }
                        out.push(value as u8);
                        i = j;
                        continue;
                    }
                    other => {
                        out.push(b'\\');
                        out.push(other);
                    }
                }
                i += 2;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The crate's scratch pattern: one named dir under the temp dir, cleared
    /// first, so tests are hermetic without a temp-dir crate.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rof-tree-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn porcelain_reads_simple_paths() {
        let (code, path) = porcelain_pair(" M src/main.rs").unwrap();
        assert_eq!(code, " M");
        assert_eq!(path, "src/main.rs");
    }

    #[test]
    fn porcelain_takes_the_new_name_of_a_rename() {
        let (_, path) = porcelain_pair("R  sub/b.rs -> sub/c.rs").unwrap();
        assert_eq!(path, "sub/c.rs");
    }

    #[test]
    fn porcelain_unquotes_a_path_with_spaces() {
        let (_, path) = porcelain_pair(r#"?? "q file.rs""#).unwrap();
        assert_eq!(path, "q file.rs");
    }

    #[test]
    fn porcelain_unquotes_an_octal_escaped_path() {
        let (_, path) = porcelain_pair(r#"?? "\303\274n.rs""#).unwrap();
        assert_eq!(path, "ün.rs");
    }

    #[test]
    fn porcelain_rejects_truncated_lines() {
        assert!(porcelain_pair(" M").is_none());
        assert!(porcelain_pair("").is_none());
    }

    #[test]
    fn a_plain_path_is_left_alone() {
        assert_eq!(unquote("src/a.rs"), "src/a.rs");
        assert_eq!(unquote(""), "");
    }

    #[test]
    fn changed_counts_paths_not_lines() {
        let d = TreeDiff {
            names: vec!["a.rs".into(), "src/b.rs".into()],
            stat: String::new(),
        };
        assert_eq!(d.changed(), 2);
        assert!(!d.is_empty());
    }

    /// §4.2's own contract, in-process: baseline, diff, rollback, diff.
    #[test]
    fn baseline_diff_rollback_round_trip() {
        let dir = scratch("round-trip");
        let tree = TreeService::new(dir.clone());
        tree.ensure().unwrap();
        // A task copy carries files the source never committed.
        std::fs::write(dir.join("a.rs"), "fn main() {}\n").unwrap();

        tree.baseline().unwrap();
        assert!(tree.diff().unwrap().is_empty(), "no attempt yet");

        // An attempt: one file changed, one created, one deleted.
        std::fs::write(dir.join("a.rs"), "fn main() { x }\n").unwrap();
        std::fs::write(dir.join("b.rs"), "new\n").unwrap();
        std::fs::remove_file(dir.join("a.rs")).ok();

        let after = tree.diff().unwrap();
        assert_eq!(after.names, vec!["a.rs", "b.rs"]);
        assert_eq!(after.changed(), 2);
        assert!(
            after.stat.contains("b.rs"),
            "new files appear in the stat: {}",
            after.stat
        );

        tree.rollback().unwrap();
        let back = tree.diff().unwrap();
        assert!(
            back.is_empty(),
            "rollback restores the baseline: {:?}",
            back.names
        );
        assert!(dir.join("a.rs").exists());
        assert!(!dir.join("b.rs").exists());
    }

    /// An unborn HEAD (a repo with no commits) must still roll back: `git
    /// checkout` refuses it, so `ensure` makes the commit the source skipped.
    #[test]
    fn an_unborn_head_still_rolls_back() {
        let dir = scratch("unborn");
        std::fs::write(dir.join("a.rs"), "one\n").unwrap();
        let tree = TreeService::new(dir.clone());
        tree.git(["init", "--quiet"]).unwrap();
        // Deliberately no commit: HEAD is unborn.
        tree.ensure().unwrap();
        tree.baseline().unwrap();

        std::fs::write(dir.join("a.rs"), "two\n").unwrap();
        std::fs::write(dir.join("b.rs"), "new\n").unwrap();
        assert_eq!(tree.diff().unwrap().names, vec!["a.rs", "b.rs"]);

        tree.rollback().unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.rs")).unwrap(), "one\n");
        assert!(!dir.join("b.rs").exists());
    }

    /// A clean tree before an attempt is the wanted state, not an error.
    #[test]
    fn baseline_on_a_clean_tree_is_benign() {
        let dir = scratch("clean");
        let tree = TreeService::new(dir);
        tree.ensure().unwrap();
        tree.baseline().unwrap();
        tree.baseline().unwrap();
        assert!(tree.diff().unwrap().is_empty());
    }

    /// A hook copied in with the repo must not block the baseline.
    #[test]
    fn copied_hooks_do_not_run() {
        let dir = scratch("hooks");
        let tree = TreeService::new(dir.clone());
        tree.git(["init", "--quiet"]).unwrap();
        let hooks = dir.join(".git/hooks");
        std::fs::write(hooks.join("pre-commit"), "#!/bin/sh\nexit 1\n").unwrap();
        tree.ensure().unwrap();
        tree.baseline().unwrap();
        std::fs::write(dir.join("a.rs"), "x\n").unwrap();
        assert_eq!(tree.diff().unwrap().names, vec!["a.rs"]);
    }
}
