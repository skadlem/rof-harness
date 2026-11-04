//! Embed the source revision; mark locally modified workspace code as dirty.

use std::process::Command;

fn main() {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
    };
    let sha = git(&["rev-parse", "--short", "HEAD"]);
    let root = git(&["rev-parse", "--show-toplevel"]);
    let revision = match (sha, root) {
        (Some(sha), Some(root)) if !sha.is_empty() => {
            match git(&[
                "-C",
                &root,
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--",
                "crates",
                "Cargo.toml",
                "Cargo.lock",
            ]) {
                Some(status) if status.is_empty() => sha,
                Some(_) => format!("{sha}-dirty"),
                None => "unknown".into(),
            }
        }
        _ => "unknown".into(),
    };
    println!("cargo:rustc-env=ROF_GIT_SHA={revision}");
    for path in [
        "build.rs",
        "../../crates",
        "../../Cargo.toml",
        "../../Cargo.lock",
        "../../.git/HEAD",
        "../../.git/index",
        "../../.git/refs/heads",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
}
