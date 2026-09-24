// v4 explorer: read-only isolated exploration contract.
#[test]
fn explorer_report_shape_is_stable() {
    let r = rof::agents::explorer_report_for_test("goal", &["src/lib.rs"]);
    assert!(r.contains("summary") && r.contains("key_files"));
}

#[tokio::test]
async fn explorer_honours_the_configured_caps() {
    let root = std::env::temp_dir().join(format!("rof-explorer-caps-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("big.rs"), "fn big() {}\n".repeat(200)).unwrap();
    let cfg = rof::config::RetrievalConfig {
        max_total_chars: 50,
        ..Default::default()
    };
    let reg = rof::tools::ToolRegistry::new(rof::config::PermissionPolicy::default());
    let trace = rof::obs::TraceSink::new();
    let rep = rof::agents::ExplorerAgent::explore("big", &root, &reg, &trace, &cfg).await;
    assert!(
        rep.chars_seen <= 50,
        "retrieval must respect the configured cap: {}",
        rep.chars_seen
    );
    assert!(!rep.key_files.is_empty());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn explorer_block_names_files_with_stats() {
    let rep = rof::agents::ExplorerReport {
        summary: "g".to_string(),
        key_files: vec![
            rof::agents::KeyFile {
                path: "a.rs".to_string(),
                why: "t".to_string(),
            },
            rof::agents::KeyFile {
                path: "b.rs".to_string(),
                why: "t".to_string(),
            },
        ],
        quotes: Vec::new(),
        snippets_seen: 5,
        chars_seen: 8432,
    };
    let line = rof::agents::explorer_block(&rep);
    assert!(line.contains("a.rs") && line.contains("b.rs"), "{line}");
    assert!(line.contains("8432"), "stats ride the line: {line}");
}

#[test]
fn explorer_block_is_empty_when_nothing_found() {
    assert_eq!(
        rof::agents::explorer_block(&rof::agents::ExplorerReport::default()),
        ""
    );
}
