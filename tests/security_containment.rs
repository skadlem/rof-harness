//! Regression coverage for path containment at the file-tool/context boundary.
use rof::config::{PermissionPolicy, RetrievalConfig, SkillsConfig};
use rof::context::Retriever;
use rof::tools::ToolRegistry;
use std::path::{Path, PathBuf};

struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("rof-containment-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(path.join("work/.git")).unwrap();
        Self(path)
    }
    fn work(&self) -> PathBuf {
        self.0.join("work")
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn registry(work: &Path) -> ToolRegistry {
    ToolRegistry::with_defaults(
        work.to_path_buf(),
        PermissionPolicy::default(),
        SkillsConfig::default(),
    )
}

#[cfg(unix)]
#[tokio::test]
async fn write_cannot_reach_git_through_directory_alias() {
    let tree = Tree::new();
    let work = tree.work();
    std::fs::write(work.join(".git/HEAD"), "original").unwrap();
    std::os::unix::fs::symlink(".git", work.join("alias")).unwrap();
    let (result, _) = registry(&work)
        .call(
            "implementer",
            "fs.write",
            Some(&work.join("alias/HEAD")),
            serde_json::json!({"path":"alias/HEAD", "content":"modified"}),
        )
        .await;
    assert!(result.is_err(), "a resolved .git path must be denied");
    assert_eq!(
        std::fs::read_to_string(work.join(".git/HEAD")).unwrap(),
        "original"
    );
}

#[test]
fn retrieval_cannot_read_an_absolute_path_outside_workdir() {
    let tree = Tree::new();
    let outside = tree.0.join("private.txt");
    std::fs::write(&outside, "PRIVATE_SENTINEL").unwrap();
    let query = format!("inspect {}", outside.display());
    let snippets = Retriever::new(tree.work(), RetrievalConfig::default()).retrieve(&query, 12_000);
    assert!(
        snippets.is_empty(),
        "outside data must never enter model context"
    );
}

#[cfg(unix)]
#[test]
fn keyword_retrieval_cannot_follow_an_external_file_symlink() {
    let tree = Tree::new();
    let outside = tree.0.join("private.txt");
    std::fs::write(&outside, "PRIVATE_SENTINEL").unwrap();
    std::os::unix::fs::symlink(&outside, tree.work().join("leak.txt")).unwrap();
    let snippets = Retriever::new(tree.work(), RetrievalConfig::default())
        .retrieve("PRIVATE_SENTINEL", 12_000);
    assert!(
        snippets.is_empty(),
        "keyword retrieval must not follow an external symlink"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn git_alias_is_denied_for_read_list_and_new_file() {
    let tree = Tree::new();
    let work = tree.work();
    std::fs::write(work.join(".git/HEAD"), "original").unwrap();
    std::os::unix::fs::symlink(".git", work.join("alias")).unwrap();
    let reg = registry(&work);
    for (tool, input) in [
        ("fs.read", serde_json::json!({"path":"alias/HEAD"})),
        ("fs.list", serde_json::json!({"path":"alias"})),
        (
            "fs.write",
            serde_json::json!({"path":"alias/new", "content":"modified"}),
        ),
    ] {
        let (result, _) = reg.call("implementer", tool, Some(&work), input).await;
        assert!(result.is_err(), "{tool} must reject a resolved .git alias");
    }
    assert!(!work.join(".git/new").exists());
}

#[cfg(unix)]
#[test]
fn retrieval_and_file_map_hide_git_aliases() {
    let tree = Tree::new();
    let work = tree.work();
    std::fs::write(
        work.join(".git/private.rs"),
        "pub struct SecretSymbol;\nPRIVATE_SENTINEL",
    )
    .unwrap();
    std::os::unix::fs::symlink(".git/private.rs", work.join("alias.rs")).unwrap();
    let r = Retriever::new(work.clone(), RetrievalConfig::default());
    assert!(r.retrieve("inspect alias.rs", 12_000).is_empty());
    assert!(r.retrieve("PRIVATE_SENTINEL", 12_000).is_empty());
    assert!(!r.file_map().contains("alias.rs"));
    assert!(rof::context::symbols::index_workdir(&work, 500).is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn normal_creates_and_internal_symlinks_still_work() {
    let tree = Tree::new();
    let work = tree.work();
    std::fs::create_dir(work.join("sub")).unwrap();
    std::fs::write(work.join("safe.txt"), "PUBLIC_SENTINEL").unwrap();
    std::os::unix::fs::symlink("safe.txt", work.join("alias.txt")).unwrap();
    let reg = registry(&work);
    let (created, _) = reg
        .call(
            "implementer",
            "fs.write",
            Some(&work),
            serde_json::json!({"path":"sub/new.txt","content":"created"}),
        )
        .await;
    assert!(created.unwrap().ok);
    assert_eq!(
        std::fs::read_to_string(work.join("sub/new.txt")).unwrap(),
        "created"
    );
    let (read, _) = reg
        .call(
            "implementer",
            "fs.read",
            Some(&work),
            serde_json::json!({"path":"alias.txt"}),
        )
        .await;
    assert_eq!(read.unwrap().output, "PUBLIC_SENTINEL");
    let snippets =
        Retriever::new(work, RetrievalConfig::default()).retrieve("inspect alias.txt", 12_000);
    assert!(snippets.iter().any(|s| s.content == "PUBLIC_SENTINEL"));
}

#[cfg(unix)]
#[tokio::test]
async fn root_listing_hides_git_aliases() {
    let tree = Tree::new();
    let work = tree.work();
    std::os::unix::fs::symlink(".git", work.join("alias")).unwrap();
    let (listed, _) = registry(&work)
        .call(
            "implementer",
            "fs.list",
            Some(&work),
            serde_json::json!({"path":"."}),
        )
        .await;
    assert!(!listed.unwrap().output.contains("alias"));
}

#[cfg(unix)]
#[tokio::test]
async fn directory_alias_to_root_and_symlinked_root_still_list() {
    let tree = Tree::new();
    let work = tree.work();
    std::fs::write(work.join("safe.txt"), "PUBLIC_SENTINEL").unwrap();
    std::os::unix::fs::symlink(".", work.join("self")).unwrap();
    let (listed, _) = registry(&work)
        .call(
            "implementer",
            "fs.list",
            Some(&work),
            serde_json::json!({"path":"self"}),
        )
        .await;
    assert!(listed.unwrap().output.contains("safe.txt"));
    let linked_root = tree.0.join("linked");
    std::os::unix::fs::symlink(&work, &linked_root).unwrap();
    let (listed, _) = registry(&linked_root)
        .call(
            "implementer",
            "fs.list",
            Some(&linked_root),
            serde_json::json!({"path":"."}),
        )
        .await;
    assert!(listed.unwrap().output.contains("safe.txt"));
}
