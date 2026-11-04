use std::{path::Path, process::Command};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn build_marks_modified_and_untracked_code_dirty() {
    let temp = std::env::temp_dir().join(format!("rof-build-provenance-{}", std::process::id()));
    let repo = temp.join("repo");
    std::fs::create_dir_all(repo.join("crates/rof/src")).unwrap();
    std::fs::write(repo.join("crates/rof/src/main.rs"), "fn main() {}\n").unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "initial",
        ],
    );
    let sha = git(&repo, &["rev-parse", "--short", "HEAD"]);
    let exe = temp.join("build-script");
    let status = Command::new("rustc")
        .args([concat!(env!("CARGO_MANIFEST_DIR"), "/build.rs"), "-o"])
        .arg(&exe)
        .status()
        .unwrap();
    assert!(status.success());
    let run = || {
        let output = Command::new(&exe)
            .current_dir(repo.join("crates/rof"))
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    };
    assert!(run().contains(&format!("cargo:rustc-env=ROF_GIT_SHA={sha}\n")));
    std::fs::write(repo.join("crates/rof/src/main.rs"), "changed\n").unwrap();
    assert!(run().contains(&format!("cargo:rustc-env=ROF_GIT_SHA={sha}-dirty\n")));
    git(&repo, &["checkout", "--", "crates/rof/src/main.rs"]);
    std::fs::write(repo.join("crates/rof/src/new.rs"), "new\n").unwrap();
    assert!(run().contains(&format!("cargo:rustc-env=ROF_GIT_SHA={sha}-dirty\n")));
    std::fs::remove_dir_all(temp).unwrap();
}
