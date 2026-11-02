#![cfg(unix)]
use rof::context::{
    memory,
    research::{Edit, Research, TreeState},
};
use std::path::PathBuf;

struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("rof-memory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("work/.git")).unwrap();
        Self(root)
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

#[test]
fn project_memory_cannot_follow_external_or_git_symlinks() {
    let tree = Tree::new();
    std::fs::write(tree.0.join("private"), "PRIVATE_SENTINEL").unwrap();
    std::fs::write(tree.work().join(".git/HEAD"), "GIT_SENTINEL").unwrap();
    std::os::unix::fs::symlink(tree.0.join("private"), tree.work().join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink(".git/HEAD", tree.work().join("CLAUDE.md")).unwrap();
    assert!(memory::load(&tree.work()).project.is_empty());
}

#[test]
fn project_memory_accepts_safe_internal_aliases() {
    let tree = Tree::new();
    std::fs::write(tree.work().join("conventions.md"), "Use Rust.").unwrap();
    std::os::unix::fs::symlink("conventions.md", tree.work().join("AGENTS.md")).unwrap();
    assert_eq!(memory::load(&tree.work()).project, "Use Rust.");
}

#[test]
fn research_index_cannot_follow_external_symlink() {
    let tree = Tree::new();
    std::fs::create_dir_all(tree.work().join(".rof/research")).unwrap();
    std::fs::write(tree.0.join("private"), "PRIVATE_SENTINEL").unwrap();
    std::os::unix::fs::symlink(
        tree.0.join("private"),
        tree.work().join(".rof/research/index.md"),
    )
    .unwrap();
    let store = Research::load(&tree.work());
    assert!(!format!("{store:?}").contains("PRIVATE_SENTINEL"));
}

#[test]
fn research_note_cannot_follow_external_symlink() {
    let tree = Tree::new();
    let state = TreeState::at("abc123");
    let mut store = Research::empty(&tree.work());
    store
        .apply(
            Edit::Write {
                topic: "retry".into(),
                body: "PRIVATE_SENTINEL".into(),
            },
            &state,
        )
        .unwrap();
    store.save().unwrap();
    let path = tree.work().join(".rof/research/retry.md");
    std::fs::rename(&path, tree.0.join("private")).unwrap();
    std::os::unix::fs::symlink(tree.0.join("private"), &path).unwrap();
    let loaded = Research::load(&tree.work());
    assert!(!loaded.needs_research("retry", &state).is_answered());
}

#[test]
fn research_save_cannot_create_directories_through_external_alias() {
    let tree = Tree::new();
    std::fs::create_dir_all(tree.0.join("outside")).unwrap();
    std::os::unix::fs::symlink(tree.0.join("outside"), tree.work().join(".rof")).unwrap();
    let mut store = Research::empty(&tree.work());
    store
        .apply(
            Edit::Write {
                topic: "retry".into(),
                body: "note".into(),
            },
            &TreeState::at("abc"),
        )
        .unwrap();
    assert!(store.save().is_err());
    assert!(!tree.0.join("outside/research").exists());
}

#[test]
fn research_atomic_write_does_not_follow_predictable_temp_alias() {
    let tree = Tree::new();
    std::fs::create_dir_all(tree.work().join(".rof/research")).unwrap();
    std::fs::write(tree.0.join("private"), "UNCHANGED").unwrap();
    std::os::unix::fs::symlink(
        tree.0.join("private"),
        tree.work().join(".rof/research/retry.md.tmp"),
    )
    .unwrap();
    let mut store = Research::empty(&tree.work());
    store
        .apply(
            Edit::Write {
                topic: "retry".into(),
                body: "note".into(),
            },
            &TreeState::at("abc"),
        )
        .unwrap();
    store.save().unwrap();
    assert_eq!(
        std::fs::read_to_string(tree.0.join("private")).unwrap(),
        "UNCHANGED"
    );
}

#[test]
fn repo_skill_root_cannot_alias_an_external_store() {
    let tree = Tree::new();
    std::fs::create_dir_all(tree.0.join("outside/secret")).unwrap();
    std::fs::write(
        tree.0.join("outside/secret/SKILL.md"),
        "---\nname: secret\ndescription: PRIVATE_SENTINEL\n---\nprivate\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(tree.0.join("outside"), tree.work().join("skills")).unwrap();
    let manager = rof::skills::SkillManager::new(
        tree.0.join("user-skills"),
        Some(tree.work().join("skills")),
        rof::skills::SkillPolicy::ReadOnly,
    );
    assert!(manager.list().is_empty());
}

#[test]
fn skill_markdown_and_support_reads_cannot_follow_external_aliases() {
    let tree = Tree::new();
    let skills = tree.work().join("skills");
    std::fs::create_dir_all(skills.join("secret")).unwrap();
    std::fs::write(
        tree.0.join("private"),
        "---\nname: secret\ndescription: PRIVATE_SENTINEL\n---\nprivate\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(tree.0.join("private"), skills.join("secret/SKILL.md")).unwrap();
    let manager = rof::skills::SkillManager::new(
        tree.0.join("user-skills"),
        Some(skills.clone()),
        rof::skills::SkillPolicy::ReadOnly,
    );
    assert!(
        manager.list().is_empty(),
        "SKILL.md must stay inside its skill directory"
    );
    std::fs::remove_file(skills.join("secret/SKILL.md")).unwrap();
    std::fs::write(
        skills.join("secret/SKILL.md"),
        "---\nname: secret\ndescription: safe\n---\nbody\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(tree.0.join("private"), skills.join("secret/support.md")).unwrap();
    assert!(manager.view("secret", Some("support.md")).is_err());
}
