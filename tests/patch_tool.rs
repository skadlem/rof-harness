use rof::tools::{FsPatchTool, ToolRegistry};
use std::path::PathBuf;

fn registry(dir: &std::path::Path) -> ToolRegistry {
    let mut cfg = rof::config::AppConfig::default();
    cfg.permissions.allowed_dirs = vec![dir.to_path_buf()];
    let mut r = ToolRegistry::new(cfg.permissions.clone());
    r.register(FsPatchTool::new(dir.to_path_buf()));
    r
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rof-patch-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn patch(
    r: &ToolRegistry,
    dir: &std::path::Path,
    input: serde_json::Value,
) -> Result<String, String> {
    let target = dir.join(input["path"].as_str().unwrap_or(""));
    let (res, _) = r
        .call("implementer", "fs.patch", Some(&target), input)
        .await;
    match res {
        Ok(o) => Ok(o.output),
        Err(e) => Err(e.to_string()),
    }
}

#[tokio::test]
async fn exact_unique_match_replaces() {
    let d = scratch("exact");
    std::fs::write(d.join("a.txt"), "line one\nline two\nline three\n").unwrap();
    let r = registry(&d);
    let out = patch(
        &r,
        &d,
        serde_json::json!({"path": "a.txt", "search": "line two\n", "replace": "LINE TWO\n"}),
    )
    .await
    .unwrap();
    assert!(out.contains("patched"), "{out}");
    assert_eq!(
        std::fs::read_to_string(d.join("a.txt")).unwrap(),
        "line one\nLINE TWO\nline three\n"
    );
}

#[tokio::test]
async fn tolerant_match_ignores_indentation() {
    let d = scratch("ws");
    std::fs::write(d.join("b.rs"), "fn f() {\n        let x = 1;\n}\n").unwrap();
    let r = registry(&d);
    // single-space search against 8-space indented file
    patch(
        &r,
        &d,
        serde_json::json!({"path": "b.rs", "search": "let x = 1;", "replace": "let x = 2;"}),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(d.join("b.rs")).unwrap(),
        "fn f() {\n        let x = 2;\n}\n"
    );
}

#[tokio::test]
async fn ambiguous_match_refused() {
    let d = scratch("ambig");
    std::fs::write(d.join("c.txt"), "dup\nmid\ndup\n").unwrap();
    let r = registry(&d);
    let err = patch(
        &r,
        &d,
        serde_json::json!({"path": "c.txt", "search": "dup\n", "replace": "DUP\n"}),
    )
    .await
    .unwrap_err();
    assert!(err.contains("2 spans"), "{err}");
    // no partial edit
    assert_eq!(
        std::fs::read_to_string(d.join("c.txt")).unwrap(),
        "dup\nmid\ndup\n"
    );
}

#[tokio::test]
async fn missing_search_refused() {
    let d = scratch("missing");
    std::fs::write(d.join("d.txt"), "hello\n").unwrap();
    let r = registry(&d);
    let err = patch(
        &r,
        &d,
        serde_json::json!({"path": "d.txt", "search": "goodbye", "replace": "hi"}),
    )
    .await
    .unwrap_err();
    assert!(err.contains("not found"), "{err}");
}

#[tokio::test]
async fn path_escape_and_grant_denied() {
    let d = scratch("deny");
    std::fs::write(d.join("e.txt"), "x\n").unwrap();
    let r = registry(&d);
    // .. escape
    let target = d.join("../escape.txt");
    let (res, _) = r
        .call(
            "implementer",
            "fs.patch",
            Some(&target),
            serde_json::json!({"path": "../escape.txt", "search": "x", "replace": "y"}),
        )
        .await;
    assert!(res.is_err());
    // reviewer has no fs.patch grant
    let target = d.join("e.txt");
    let (res, _) = r
        .call(
            "reviewer",
            "fs.patch",
            Some(&target),
            serde_json::json!({"path": "e.txt", "search": "x", "replace": "y"}),
        )
        .await;
    assert!(res.unwrap_err().to_string().contains("may not use"));
}
