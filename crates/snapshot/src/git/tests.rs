use super::*;
use crate::{PATCH_MAX_BYTES, PATCH_MAX_LINES, PATCH_TRUNCATED_MARKER};

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
fn rollback_before_ensure_errors() {
    let dir = scratch("rollback-no-ensure");
    std::fs::write(dir.join("a.rs"), "one\n").unwrap();
    let tree = TreeService::new(&dir);
    let e = tree.rollback().unwrap_err();
    assert!(e.to_string().contains("ensure()"), "{e}");
    // Nothing was touched: no repo created, file intact.
    assert!(!dir.join(".git").exists());
    assert_eq!(std::fs::read_to_string(dir.join("a.rs")).unwrap(), "one\n");
}

#[test]
fn rollback_without_recorded_start_errors() {
    // ensure() ran but the tree was empty: HEAD unborn, no start rev.
    let dir = scratch("rollback-unborn");
    let tree = TreeService::new(&dir);
    tree.ensure().unwrap();
    let e = tree.rollback().unwrap_err();
    assert!(e.to_string().contains("start HEAD"), "{e}");
}

#[test]
fn workdir_state_reports_repo_cleanliness() {
    let plain = scratch("state-plain");
    assert_eq!(workdir_state(&plain), WorkdirState::NotARepo);

    let dir = scratch("state-dirty");
    let tree = TreeService::new(&dir);
    std::fs::write(dir.join("a.rs"), "one\n").unwrap();
    tree.ensure().unwrap();
    tree.baseline().unwrap();
    assert_eq!(workdir_state(&dir), WorkdirState::Clean);

    std::fs::write(dir.join("a.rs"), "two\n").unwrap();
    assert_eq!(workdir_state(&dir), WorkdirState::Dirty);
    std::fs::write(dir.join("a.rs"), "one\n").unwrap();
    assert_eq!(workdir_state(&dir), WorkdirState::Clean);

    std::fs::write(dir.join("new.txt"), "x\n").unwrap();
    assert_eq!(workdir_state(&dir), WorkdirState::Dirty);

    // Ignored bytecode does not count as dirty.
    std::fs::remove_file(dir.join("new.txt")).unwrap();
    std::fs::create_dir(dir.join("__pycache__")).unwrap();
    std::fs::write(dir.join("__pycache__/x.pyc"), "junk\n").unwrap();
    assert_eq!(workdir_state(&dir), WorkdirState::Clean);
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
fn ensure_excludes_bytecode_and_package_outputs_from_commits() {
    // `target/` and `node_modules/` stay silenced; `build/` and `dist/`
    // are NOT defaults anymore — new files there must reach the patch,
    // so excluding them silently dropped deliverable content.
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
    for shipped in ["build/out.bin", "dist/out.bin"] {
        assert!(
            committed.contains(shipped),
            "new deliverable file must ship: {committed}"
        );
    }
    for junk in ["target/out.bin", "node_modules/out.bin", "nested.bin"] {
        assert!(
            !committed.contains(junk),
            "build output reached the commit: {committed}"
        );
    }
    let exclude = std::fs::read_to_string(dir.join(".git/info/exclude")).unwrap();
    for pat in ["target/", "node_modules/"] {
        assert!(
            exclude.lines().any(|l| l == pat),
            "missing exclude {pat}: {exclude}"
        );
    }
    for pat in ["build/", "dist/"] {
        assert!(
            !exclude.lines().any(|l| l == pat),
            "stale exclude {pat} hides deliverable files: {exclude}"
        );
    }
}

#[test]
fn new_file_under_build_reaches_baseline_and_patch() {
    let dir = scratch("build-patch");
    let tree = TreeService::new(&dir);
    std::fs::write(dir.join("a.rs"), "one\n").unwrap();
    tree.ensure().unwrap();
    tree.baseline().unwrap();

    std::fs::create_dir_all(dir.join("build")).unwrap();
    std::fs::write(dir.join("build/new.txt"), "build-marker-7a1e\n").unwrap();
    // Untracked under build/: visible in evidence, not ignored away
    // (porcelain collapses the new dir to `build/`; the patch expands it
    // per file via `ls-files --others`).
    let d = tree.diff().unwrap();
    assert_eq!(d.changed, vec!["build/"]);
    assert_eq!(d.untracked, vec!["build/"]);
    let p = tree.patch(&d).unwrap();
    assert!(p.text.contains("+build-marker-7a1e"), "{}", p.text);
    // ... and the next baseline absorbs it instead of dropping it.
    tree.baseline().unwrap();
    let ls = tree.git(["ls-tree", "-r", "--name-only", "HEAD"]).unwrap();
    let committed = String::from_utf8_lossy(&ls.stdout).into_owned();
    assert!(committed.contains("build/new.txt"), "{committed}");
}

#[test]
fn with_excludes_re_adds_build_silence() {
    let dir = scratch("with-excludes");
    let tree = TreeService::new(&dir).with_excludes(&["target/", "build/"]);
    std::fs::write(dir.join("a.rs"), "one\n").unwrap();
    std::fs::create_dir_all(dir.join("build")).unwrap();
    std::fs::write(dir.join("build/out.bin"), "junk\n").unwrap();
    tree.ensure().unwrap();
    tree.baseline().unwrap();

    let ls = tree.git(["ls-tree", "-r", "--name-only", "HEAD"]).unwrap();
    let committed = String::from_utf8_lossy(&ls.stdout).into_owned();
    assert!(committed.contains("a.rs"), "{committed}");
    assert!(
        !committed.contains("out.bin"),
        "caller-silenced output reached the commit: {committed}"
    );
    assert!(tree.diff().unwrap().is_empty());
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
