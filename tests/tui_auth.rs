#[test]
fn creds_roundtrip_with_override_path() {
    let dir = std::env::temp_dir().join(format!("rof-creds-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("ROF_CREDENTIALS", dir.join("credentials"));
    let s = rof::tui::auth::store();
    s.save("probe", "k123").unwrap();
    assert!(s.providers().contains(&"probe".to_string()));
    assert!(s.remove("probe").unwrap());
    assert!(!s.providers().contains(&"probe".to_string()));
    std::env::remove_var("ROF_CREDENTIALS");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn verify_rejects_garbage_without_network_panic() {
    let r = rof::tui::auth::verify("bogus-provider", "k").await;
    assert!(r.is_err());
}
