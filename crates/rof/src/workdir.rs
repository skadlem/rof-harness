//! Disposable-workdir guard and run-record sidecar paths.

use std::path::{Path, PathBuf};

use crate::cli::Args;

/// Disposable-workdir guard: `--workdir` is mutated in place (`git init`,
/// `git add -A`, tool exec). Refuse values that would destroy the machine
/// or the harness checkout itself. Non-empty task dirs stay allowed.
/// An existing repo with uncommitted changes is refused unless `allow_dirty`
/// (`--allow-dirty-workdir`): pre-existing dirt would otherwise leak into
/// the reported patch, so running there is an explicit opt-in.
pub(crate) fn validate_workdir(path: &Path, allow_dirty: bool) -> Result<PathBuf, String> {
    if !path.is_dir() {
        return Err(format!("workdir is not a directory: {}", path.display()));
    }
    let canon = path
        .canonicalize()
        .map_err(|e| format!("workdir cannot be canonicalized: {e}"))?;
    if canon.parent().is_none() {
        return Err("workdir must not be the filesystem root".into());
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() && canon == Path::new(&home) {
            return Err("workdir must not be $HOME".into());
        }
    }
    if canon.join("crates/rof/Cargo.toml").is_file() {
        return Err("workdir must not be the harness checkout itself".into());
    }
    if !allow_dirty && snapshot::workdir_state(&canon) == snapshot::WorkdirState::Dirty {
        return Err(format!(
            "workdir has uncommitted changes: {} (pass --allow-dirty-workdir to run here anyway)",
            canon.display()
        ));
    }
    Ok(canon)
}

/// WAL filename for the shipped path: the fail-closed log lives inside
/// `--workdir` so a real run is durable without extra flags.
const DEFAULT_LOG_NAME: &str = ".rof-events.jsonl";

/// Default WAL path for one run: inside `--workdir` (which `main` already
/// verified is a directory, so the open cannot fail on a missing parent).
fn default_log_path(workdir: &Path) -> PathBuf {
    workdir.join(DEFAULT_LOG_NAME)
}

/// Shipped-path WAL wiring: default on (inside `--workdir`), `--log-path`
/// overrides the location, `--no-log` disables it (wins over the override).
pub(crate) fn resolve_log_path(args: &Args) -> Option<PathBuf> {
    if args.no_log {
        return None;
    }
    if let Some(p) = args.log_path.as_deref() {
        return Some(PathBuf::from(p));
    }
    Some(default_log_path(&args.workdir))
}

/// Keep run-record sidecars (WAL + incremental dump) out of the reported
/// patch: `snapshot` baselines `git add -A` mid-run, so a sidecar under
/// `--workdir` would otherwise be committed and diffed into stdout.
/// Appends the workdir-relative path to the copy-local `.git/info/exclude`
/// (never a tracked file); paths outside the workdir cannot be staged.
/// Runs after `ensure()` (stale sidecars are removed before its commit), so
/// the mid-run baselines — not the initial commit — are what this excludes.
pub(crate) fn exclude_sidecar(workdir: &Path, path: &Path) {
    // Never create `.git` here: `snapshot::is_repo` is existence-based, so a
    // bare `.git/info/exclude` would fake a repo and skip `git init`.
    // Callers run this after `ensure()`; a missing `.git` means ensure
    // failed and the loop reports it — exclusion then stays a no-op.
    let git = workdir.join(".git");
    if !git.is_dir() {
        return;
    }
    let Ok(rel) = path.strip_prefix(workdir) else {
        return;
    };
    let rel = rel.to_string_lossy().replace('\\', "/");
    if rel.is_empty() {
        return;
    }
    let exclude = git.join("info/exclude");
    if let Some(dir) = exclude.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let text = std::fs::read_to_string(&exclude).unwrap_or_default();
    if text.lines().any(|l| l == rel) {
        return;
    }
    let mut out = text;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&rel);
    out.push('\n');
    let _ = std::fs::write(&exclude, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::args_for;
    use std::path::Path;

    #[test]
    fn workdir_guard_rejects_root_home_and_self_repo() {
        assert!(validate_workdir(Path::new("/"), false).is_err());
        if let Ok(home) = std::env::var("HOME") {
            assert!(validate_workdir(Path::new(&home), false).is_err());
        }
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest.parent().unwrap().parent().unwrap();
        assert!(validate_workdir(workspace_root, false).is_err());
    }

    #[test]
    fn workdir_guard_accepts_tmp() {
        let tmp = std::env::temp_dir();
        assert!(validate_workdir(&tmp, false).is_ok());
    }

    /// Raw-git repo builder for the dirt tests (identity via `-c`, never the
    /// host config).
    fn git_repo_with_commit(dir: &Path) {
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {:?}", out);
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(&["add", "a.txt"]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "seed",
        ]);
    }

    #[test]
    fn workdir_guard_refuses_dirty_repo_without_opt_in() {
        let dir = std::env::temp_dir().join(format!("rof-dirty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git_repo_with_commit(&dir);
        assert!(validate_workdir(&dir, false).is_ok(), "clean repo runs");
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        let err = validate_workdir(&dir, false).unwrap_err();
        assert!(
            err.contains("--allow-dirty-workdir"),
            "refusal names the opt-in: {err}"
        );
        assert!(
            validate_workdir(&dir, true).is_ok(),
            "opt-in runs in the dirty repo"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn workdir_guard_untracked_file_is_dirty() {
        let dir = std::env::temp_dir().join(format!("rof-untracked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git_repo_with_commit(&dir);
        std::fs::write(dir.join("new.txt"), "x\n").unwrap();
        assert!(validate_workdir(&dir, false).is_err());
        assert!(validate_workdir(&dir, true).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn log_path_defaults_inside_workdir_with_opt_out_and_override() {
        use crate::cli::run_config;
        let dir = Path::new("/tmp/w");
        assert_eq!(
            resolve_log_path(&args_for(dir, None)),
            Some(dir.join(".rof-events.jsonl")),
            "shipped path defaults the WAL inside --workdir"
        );
        assert_eq!(
            run_config(&args_for(dir, None)).log_path,
            Some(dir.join(".rof-events.jsonl")),
            "real config carries the default (not RunConfig::default None)"
        );
        let mut over = args_for(dir, None);
        over.log_path = Some("/tmp/custom.jsonl".into());
        assert_eq!(
            run_config(&over).log_path,
            Some(PathBuf::from("/tmp/custom.jsonl"))
        );
        let mut off = args_for(dir, None);
        off.no_log = true;
        assert_eq!(run_config(&off).log_path, None);
        // Opt-out wins over an explicit override.
        off.log_path = Some("/tmp/custom.jsonl".into());
        assert_eq!(run_config(&off).log_path, None);
    }
}
