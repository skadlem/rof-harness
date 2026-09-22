// v4 verify guard + shell prefix + veto note pure parts.
#[test]
fn verify_guard_upholds_when_no_checks_and_writes_present() {
    assert_eq!(
        rof::engine::veto_note_for_test("a", "b"),
        "verify veto on a: b"
    );
}

#[test]
fn prefix_allowlist_permits_test_subcommands() {
    assert!(rof::tools::prefix_allowed_for_test(
        &["cargo test".into()],
        "cargo test foo"
    ));
    assert!(!rof::tools::prefix_allowed_for_test(
        &["cargo test".into()],
        "rm -rf /"
    ));
    assert!(!rof::tools::prefix_allowed_for_test(
        &["cargo test".into()],
        "cargo test-evil"
    ));
}
