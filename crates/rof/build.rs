//! Embed the checkout's short git sha as `ROF_GIT_SHA` so every eval
//! report names the binary that produced it. Std only; "unknown" when git
//! is unavailable — never a fabricated sha.

use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=ROF_GIT_SHA={sha}");
    println!("cargo:rerun-if-changed=build.rs");
    // Refresh the embedded sha when HEAD moves (commit, checkout, branch).
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
}
