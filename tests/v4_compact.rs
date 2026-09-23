// Transcript-only compaction (/compact): a prose-only round digest.
// Default-off (ROF_COMPACT unset): no digest collected, prompts byte-identical.
// The digest must NEVER carry file bytes — only verdict prose, answers,
// skill lines, path names, and condensed check output.
#[test]
fn digest_collects_prose_and_refuses_file_bytes() {
    let mut d = rof::engine::digest::RoundDigest::new();
    assert!(d.render_raw().is_empty());
    // An artifact carrying file bytes: current_content + patch bodies.
    let artifact = serde_json::json!({
        "file_state": [{"path": "src/x.rs", "current_content": "SECRET_FILE_BYTES_MARKER_123"}],
        "patches": [{"path": "src/x.rs", "search": "a", "replace": "SECRET_PATCH_BODY_MARKER_456"}],
        "result": {"artifact": "prose answer body here"},
        "skill_changes": []
    });
    d.note_round(
        1,
        false,
        "reviewer says: anchor wrong, re-read the file",
        &artifact,
        "src/x.rs",
        "test result: FAILED. 0 passed",
    );
    let raw = d.render_raw();
    // Prose in …
    assert!(
        raw.contains("reviewer says: anchor wrong"),
        "feedback kept: {raw}"
    );
    assert!(raw.contains("prose answer body here"), "answer kept: {raw}");
    assert!(raw.contains("src/x.rs"), "path name kept: {raw}");
    assert!(raw.contains("test result: FAILED"), "checks kept: {raw}");
    // … file bytes never.
    assert!(
        !raw.contains("SECRET_FILE_BYTES_MARKER_123"),
        "file bytes leaked: {raw}"
    );
    assert!(
        !raw.contains("SECRET_PATCH_BODY_MARKER_456"),
        "patch bodies leaked: {raw}"
    );
}

#[test]
fn digest_compaction_gate_and_fallback() {
    // Below threshold: raw, no compaction needed.
    let mut d = rof::engine::digest::RoundDigest::new();
    d.note_round(1, false, "short note", &serde_json::Value::Null, "", "");
    assert!(!d.needs_compaction());
    assert!(d.render_raw().contains("short note"));
    // Env gate: unset/false → off; yes/true/1 → on. Sequential in one fn.
    std::env::remove_var("ROF_COMPACT");
    assert!(!rof::engine::digest::compact_enabled());
    std::env::set_var("ROF_COMPACT", "no");
    assert!(!rof::engine::digest::compact_enabled());
    std::env::set_var("ROF_COMPACT", "yes");
    assert!(rof::engine::digest::compact_enabled());
    std::env::remove_var("ROF_COMPACT");
    // Fallback on summarize failure: head+tail with marker, never the middle.
    let big = "H".repeat(800) + &"M".repeat(2000) + &"T".repeat(800);
    let cut = rof::engine::digest::head_tail(&big, 1600);
    assert!(cut.contains("truncated"), "marker present: {cut}");
    assert!(cut.starts_with(&"H".repeat(10)), "head kept");
    assert!(cut.ends_with(&"T".repeat(10)), "tail kept");
    assert!(!cut.contains('M'), "middle dropped");
}
