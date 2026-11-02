//! Shared containment for file tools and deterministic context reads.
use super::{normalize, under, ToolError};
use std::path::{Path, PathBuf};

/// Resolve an existing file/directory, or a new filename under an existing
/// directory. Return the checked canonical path so callers do not re-follow
/// the original directory aliases. This is not an OS-level filesystem sandbox:
/// another process concurrently replacing directories still requires isolation.
pub(crate) fn resolve(root: &Path, target: &Path) -> Result<PathBuf, ToolError> {
    let canon_root = std::fs::canonicalize(root)
        .map_err(|e| ToolError::Denied(format!("tool root is not resolvable: {e}")))?;
    if !under(root, target) && !under(&canon_root, target) {
        return Err(ToolError::Denied("path escapes tool root".to_string()));
    }
    reject_git(target)?;
    if normalize(target) == normalize(root) {
        return Ok(canon_root);
    }
    resolve_target(&canon_root, target, 0)
}

fn resolve_target(root: &Path, target: &Path, links: usize) -> Result<PathBuf, ToolError> {
    if links >= 40 {
        return Err(ToolError::Denied("too many symlinks".to_string()));
    }
    reject_git(target)?;
    if normalize(target) == root {
        return Ok(root.to_path_buf());
    }
    let parent = target.parent().unwrap_or(root);
    let canon_parent = match std::fs::canonicalize(parent) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ToolError::Denied(format!(
                "parent directory does not exist: create {} first, then re-issue the write",
                parent.display()
            )))
        }
        Err(e) => return Err(ToolError::Denied(format!("path is not resolvable: {e}"))),
    };
    check(root, &canon_parent)?;
    let name = target
        .file_name()
        .ok_or_else(|| ToolError::Denied("path has no filename".to_string()))?;
    let path = canon_parent.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let link = std::fs::read_link(&path)
                .map_err(|e| ToolError::Denied(format!("unreadable symlink: {e}")))?;
            let next = canon_parent.join(link);
            resolve_target(root, &next, links + 1)
        }
        Ok(_) => {
            let canon = std::fs::canonicalize(&path)
                .map_err(|e| ToolError::Denied(format!("path is not resolvable: {e}")))?;
            check(root, &canon)?;
            Ok(canon)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            check(root, &path)?;
            Ok(path)
        }
        Err(e) => Err(ToolError::Denied(format!("path is not resolvable: {e}"))),
    }
}

fn check(root: &Path, path: &Path) -> Result<(), ToolError> {
    if !path.starts_with(root) {
        return Err(ToolError::Denied(
            "symlink escapes the tool root".to_string(),
        ));
    }
    reject_git(path)
}

fn reject_git(path: &Path) -> Result<(), ToolError> {
    if path.components().any(|c| c.as_os_str() == ".git") {
        return Err(ToolError::Denied(
            "path is the git substrate (.git): harness-only".to_string(),
        ));
    }
    Ok(())
}
