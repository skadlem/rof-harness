use rof::config::RetrievalConfig;
use rof::context::{render, Retriever};

fn setup(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("rof-retr-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("policy.md"), "tool permissions policy allowlist").unwrap();
    std::fs::write(root.join("sub").join("router.rs"), "fn route_model() {}").unwrap();
    std::fs::write(
        root.join("sub").join("notes.bin"),
        "tool permissions binary",
    )
    .unwrap();
    std::fs::write(
        root.join("target").join("skip.rs"),
        "tool permissions built artifact",
    )
    .unwrap();
    root
}

#[test]
fn ranks_relevant_first_and_skips_build_dirs() {
    let root = setup("rank");
    let r = Retriever::new(root.clone(), RetrievalConfig::default());
    let snips = r.retrieve("tool permissions policy", 50_000);
    assert!(!snips.is_empty());
    assert!(
        snips[0].path.ends_with("policy.md"),
        "top hit: {}",
        snips[0].path
    );
    assert!(
        !snips.iter().any(|s| s.path.contains("target")),
        "build dir must be skipped"
    );
    assert!(
        !snips.iter().any(|s| s.path.ends_with(".bin")),
        "extension filter must apply"
    );
    let rendered = render(&snips);
    assert!(rendered.contains("policy.md"));
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn budget_is_respected() {
    let root = setup("budget");
    let cfg = RetrievalConfig {
        max_total_chars: 40,
        ..RetrievalConfig::default()
    };
    let r = Retriever::new(root.clone(), cfg);
    let snips = r.retrieve("tool permissions policy", 40);
    let total: usize = snips.iter().map(|s| s.path.len() + s.content.len()).sum();
    assert!(total <= 200, "bounded output, got {total}");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn empty_query_returns_nothing() {
    let root = setup("empty");
    let r = Retriever::new(root.clone(), RetrievalConfig::default());
    assert!(r.retrieve("a", 1000).is_empty());
    assert!(r.retrieve("", 1000).is_empty());
    assert_eq!(render(&[]), "retrieved: (none)");
    std::fs::remove_dir_all(&root).ok();
}
