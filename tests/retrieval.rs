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

/// The measured failure mode: the goal names the file to edit, keyword
/// density picks a bigger file, and the implementer guesses a patch anchor it
/// never saw. A named path must win.
#[test]
fn named_path_beats_keyword_density() {
    let root = setup("named");
    // Big, keyword-dense decoy that would outscore the small real target.
    let noise = "tool permissions policy allowlist route model ids roles struct\n".repeat(200);
    std::fs::write(root.join("sub").join("big.rs"), noise).unwrap();
    let r = Retriever::new(root.clone(), RetrievalConfig::default());
    let goal = "In sub/router.rs, add exactly one new doc comment line \
                `/// Maps roles to model ids.` directly above `fn route_model`.";
    let snips = r.retrieve(goal, 50_000);
    assert!(
        snips[0].path.ends_with("router.rs"),
        "the named file must lead: {:?}",
        snips.iter().map(|s| &s.path).collect::<Vec<_>>()
    );
    assert!(
        snips[0].content.contains("fn route_model"),
        "and its text must be present, not truncated away: {}",
        snips[0].content
    );
    std::fs::remove_dir_all(&root).ok();
}

/// A goal names a symbol deep inside a file larger than the per-file cap: the
/// excerpt must contain that symbol, or the implementer patches an anchor it
/// never saw (measured: `ProcRunTool` sat at line 421 of 521 and the patch was
/// refused twice).
#[test]
fn excerpt_centres_on_the_named_symbol() {
    let root = setup("window");
    let mut body = "// filler line to push the anchor past the cap\n".repeat(200);
    body.push_str(
        "/// Runs allowlisted commands.\npub struct ProcRunTool {\n    root: PathBuf,\n}\n",
    );
    body.push_str(&"// trailer\n".repeat(200));
    std::fs::write(root.join("tools.rs"), body).unwrap();
    let r = Retriever::new(root.clone(), RetrievalConfig::default());
    let snips = r.retrieve(
        "In tools.rs, replace the doc comment on `struct ProcRunTool` with one \
         that states the allowlist rules. Change nothing else.",
        50_000,
    );
    let s = snips
        .iter()
        .find(|s| s.path.ends_with("tools.rs"))
        .unwrap_or_else(|| panic!("named file must be retrieved: {:?}", snips.len()));
    assert!(
        s.content.contains("pub struct ProcRunTool"),
        "the excerpt must contain the named symbol, got: {}",
        s.content.chars().take(160).collect::<String>()
    );
    std::fs::remove_dir_all(&root).ok();
}

/// The implementer names paths to read them and guessed wrong a quarter of the
/// time (src/obs.rs for src/obs/trace.rs); the map is what it gets instead.
#[test]
fn file_map_lists_sources_and_skips_build_dirs() {
    let root = setup("map");
    let r = Retriever::new(root.clone(), RetrievalConfig::default());
    let map = r.file_map();
    assert!(map.contains("policy.md"), "map: {map}");
    assert!(map.contains("sub/router.rs"), "map: {map}");
    assert!(!map.contains("target/"), "build dir must be skipped: {map}");
    assert!(
        !map.contains("notes.bin"),
        "extension filter must apply: {map}"
    );
    std::fs::remove_dir_all(&root).ok();
}
