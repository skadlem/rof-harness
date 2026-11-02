use rof::config::AppConfig;
use rof::eval::{compare, EvalSuite, RunLabel, SuiteReport};
use std::path::{Path, PathBuf};
use std::process::Command;

struct ScratchRepo(PathBuf);
impl Drop for ScratchRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@local")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@local")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fixture() -> ScratchRepo {
    let repo =
        ScratchRepo(std::env::temp_dir().join(format!("rof-provenance-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&repo.0).unwrap();
    std::fs::write(repo.0.join("input.txt"), "original").unwrap();
    std::fs::write(repo.0.join(".gitignore"), "target/\n").unwrap();
    git(&repo.0, &["init", "--quiet"]);
    git(&repo.0, &["add", "."]);
    git(&repo.0, &["commit", "--quiet", "-m", "fixture"]);
    repo
}

fn label(root: &Path) -> RunLabel {
    RunLabel::build(
        &AppConfig::default(),
        &EvalSuite {
            name: "fixture".into(),
            tasks: Vec::new(),
        },
        root,
    )
}

#[test]
fn labels_distinguish_dirty_and_untracked_inputs_at_the_same_head() {
    let repo = fixture();
    let clean = serde_json::to_value(label(&repo.0)).unwrap();
    std::fs::write(repo.0.join("input.txt"), "modified").unwrap();
    let modified = serde_json::to_value(label(&repo.0)).unwrap();
    assert_eq!(clean["git_head"], modified["git_head"]);
    assert_ne!(clean["target_tree_hash"], modified["target_tree_hash"]);
    assert_eq!(clean["target_dirty"], false);
    assert_eq!(modified["target_dirty"], true);
    std::fs::write(repo.0.join("extra.txt"), "untracked").unwrap();
    let extra = serde_json::to_value(label(&repo.0)).unwrap();
    assert_ne!(modified["target_tree_hash"], extra["target_tree_hash"]);
    std::fs::remove_file(repo.0.join("input.txt")).unwrap();
    let deleted = serde_json::to_value(label(&repo.0)).unwrap();
    assert_ne!(extra["target_tree_hash"], deleted["target_tree_hash"]);
}

#[test]
fn ignored_build_outputs_do_not_change_the_source_fingerprint() {
    let repo = fixture();
    let before = serde_json::to_value(label(&repo.0)).unwrap();
    std::fs::create_dir(repo.0.join("target")).unwrap();
    std::fs::write(repo.0.join("target/artifact"), "irrelevant").unwrap();
    let after = serde_json::to_value(label(&repo.0)).unwrap();
    assert!(before["target_tree_hash"]
        .as_str()
        .is_some_and(|hash| !hash.is_empty()));
    assert_eq!(before["target_tree_hash"], after["target_tree_hash"]);
}

#[test]
fn comparison_exposes_a_changed_judge_and_harness_build() {
    let repo = fixture();
    let before = SuiteReport {
        label: label(&repo.0),
        ..SuiteReport::default()
    };
    let mut after = before.clone();
    after.label.verify_model = "independent-judge".into();
    let comparison = compare(&before, &after);
    assert!(comparison
        .labels
        .iter()
        .any(|delta| delta.field == "verify_model" && delta.changed()));
    let serialized = serde_json::to_value(&before.label).unwrap();
    assert!(serialized["harness_revision"]
        .as_str()
        .is_some_and(|revision| revision != "unknown"));
}

#[cfg(unix)]
#[test]
fn source_fingerprints_hash_symlinks_without_reading_their_targets() {
    let repo = fixture();
    let outside = ScratchRepo(
        std::env::temp_dir().join(format!("rof-provenance-outside-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir_all(&outside.0).unwrap();
    let external_file = outside.0.join("private.txt");
    std::fs::write(&external_file, "first").unwrap();
    std::os::unix::fs::symlink(&external_file, repo.0.join("link.txt")).unwrap();
    let first = serde_json::to_value(label(&repo.0)).unwrap();
    std::fs::write(&external_file, "second").unwrap();
    let second = serde_json::to_value(label(&repo.0)).unwrap();
    assert_eq!(first["target_tree_hash"], second["target_tree_hash"]);
    std::fs::remove_file(repo.0.join("link.txt")).unwrap();
    std::os::unix::fs::symlink("another-target", repo.0.join("link.txt")).unwrap();
    let replaced = serde_json::to_value(label(&repo.0)).unwrap();
    assert_ne!(second["target_tree_hash"], replaced["target_tree_hash"]);
}

async fn provider_report(root: &Path, ctx: &str, exec: &str, verify: &str) -> SuiteReport {
    use rof::llm::{ContextService, ExecutorService, OpenRouterClient};
    use std::sync::Arc;
    let client = |base: &str| {
        Arc::new(OpenRouterClient::from_parts(
            base.into(),
            "bearer-secret".into(),
        ))
    };
    rof::eval::EvaluationRunner::new(
        Arc::new(rof::obs::TraceSink::new()),
        AppConfig::default(),
        ContextService::new(client(ctx), "same-model".into()),
        ExecutorService::new(client(exec), "same-model".into(), None),
        ExecutorService::new(client(verify), "same-model".into(), None),
        root.into(),
    )
    .run_suite(&EvalSuite {
        name: "providers".into(),
        tasks: Vec::new(),
    })
    .await
}

#[tokio::test]
async fn labels_distinguish_actual_role_providers_without_credentials() {
    let repo = fixture();
    let first = provider_report(
        &repo.0,
        "https://Context.Example/v1/",
        "https://alice:password@Provider.Example:443/v1/?api_key=private#hidden",
        "https://judge.example/v2",
    )
    .await;
    let second = provider_report(
        &repo.0,
        "https://context.example/v1",
        "https://another-provider.example/v1",
        "https://independent-judge.example/v2",
    )
    .await;
    assert_eq!(first.label.config_hash, second.label.config_hash);
    assert_eq!(first.label.exec_model, second.label.exec_model);
    let first_json = serde_json::to_value(&first.label).unwrap();
    let second_json = serde_json::to_value(&second.label).unwrap();
    assert_ne!(first_json["exec_endpoint"], second_json["exec_endpoint"]);
    assert_eq!(first_json["ctx_endpoint"], "https://context.example/v1");
    assert_eq!(first_json["exec_endpoint"], "https://provider.example/v1");
    assert_eq!(first_json["verify_endpoint"], "https://judge.example/v2");
    let rendered = first.label.render();
    for secret in [
        "alice",
        "password",
        "api_key",
        "private",
        "hidden",
        "bearer-secret",
    ] {
        assert!(!rendered.contains(secret));
        assert!(!first_json.to_string().contains(secret));
    }
    let comparison = compare(&first, &second);
    for field in ["exec_endpoint", "verify_endpoint"] {
        assert!(comparison
            .labels
            .iter()
            .any(|delta| delta.field == field && delta.changed()));
    }
}
