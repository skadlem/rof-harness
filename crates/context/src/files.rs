use crate::cut_chars;
use std::path::Path;

const SKIP_DIRS: &[&str] = &["target", ".git", "node_modules", ".hg", ".svn", "baselines"];

/// Safety bound on file-map paths, applied post-sort (see [`file_map`]).
/// Kept at the pre-6b walk-stop value so caller budgets (200 at the
/// agent-loop call sites) still resolve to the alphabetically-first
/// `max_paths` paths for any tree; larger asks clamp here, deterministically.
pub const FILE_MAP_WALK_CAP: usize = 2000;

/// Byte-stable capped path listing for the file map (rides the cached prefix).
/// Sorted, dotfiles and build/VCS dirs skipped, truncated to `max_paths`.
/// Deterministic: the full depth-capped walk is collected first, then sorted,
/// then truncated, so the output is the alphabetically-first `max_paths`
/// paths (clamped to [`FILE_MAP_WALK_CAP`] when the caller asks for more).
/// Depth is capped at 8; the cap truncation happens after the sort, never
/// mid-walk in `read_dir` order.
pub fn file_map(_root: &Path, _max_paths: usize) -> Vec<String> {
    let mut v = Vec::new();
    walk(_root, _root, 0, &mut v);
    v.sort();
    v.truncate(_max_paths.min(FILE_MAP_WALK_CAP));
    v
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 8 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk(root, &p, depth + 1, out);
        } else if let Ok(rel) = p.strip_prefix(root) {
            out.push(rel.to_string_lossy().to_string());
        }
    }
}

/// Whole named file up to cap, never summarized. Caller excludes it from mid.
pub fn named_file_contents(
    _root: &Path,
    _path: &str,
    _cap_chars: usize,
) -> std::io::Result<String> {
    let c = std::fs::read_to_string(_root.join(_path))?;
    Ok(cut_chars(&c, _cap_chars).to_owned())
}
