#[path = "src/eval/provenance.rs"]
mod provenance;

fn main() {
    let root = std::path::Path::new(".");
    let revision = provenance::git(root, &["rev-parse", "HEAD"])
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| "unknown".into());
    let state = provenance::snapshot(root);
    let revision = match state {
        Some(state) if state.dirty => format!("{}-dirty:{}", revision.trim(), state.hash),
        _ => revision.trim().to_string(),
    };
    println!("cargo:rustc-env=ROF_BUILD_REVISION={revision}");
    for path in ["src", "Cargo.toml", "Cargo.lock", "build.rs"] {
        println!("cargo:rerun-if-changed={path}");
    }
    // Worktree HEAD and shared branch refs live in different Git directories.
    for args in [
        vec!["rev-parse", "--git-path", "HEAD"],
        vec!["symbolic-ref", "--quiet", "HEAD"],
    ] {
        if let Some(path) = provenance::git(root, &args).and_then(|p| String::from_utf8(p).ok()) {
            let path = if args[0] == "symbolic-ref" {
                provenance::git(root, &["rev-parse", "--git-path", path.trim()])
                    .and_then(|p| String::from_utf8(p).ok())
            } else {
                Some(path)
            };
            if let Some(path) = path {
                println!("cargo:rerun-if-changed={}", path.trim());
            }
        }
    }
}
