// v4 explorer: read-only isolated exploration contract.
#[test]
fn explorer_report_shape_is_stable() {
    let r = rof::agents::explorer_report_for_test("goal", &["src/lib.rs"]);
    assert!(r.contains("summary") && r.contains("key_files"));
}
