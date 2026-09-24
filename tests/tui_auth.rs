/// Serializes tests that point ROF_CREDENTIALS at scratch dirs: tests in
/// one binary share env, so two parallel overrides would cross-wire.
static CRED_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn creds_roundtrip_with_override_path() {
    let _g = CRED_ENV_LOCK.lock().unwrap();
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

#[test]
fn provider_prefix_splits_model_ids() {
    assert_eq!(
        rof::tui::auth::split_provider_model("acme/model-x"),
        ("acme", "model-x")
    );
    assert_eq!(rof::tui::auth::split_provider_model("model-x"), ("", "model-x"));
    assert_eq!(
        rof::tui::auth::split_provider_model("a/b/c"),
        ("a", "b/c")
    );
}

#[test]
fn provider_keys_resolve_store_then_env() {
    let _g = CRED_ENV_LOCK.lock().unwrap();
    let dir = std::env::temp_dir().join(format!("rof-keyres-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("ROF_CREDENTIALS", dir.join("credentials"));
    // Hermetic: stash any real OR_TOKEN for the duration.
    let saved = std::env::var("OR_TOKEN").ok();
    std::env::remove_var("OR_TOKEN");
    // Empty store, no env: nothing to resolve.
    assert_eq!(rof::tui::auth::key_for("openrouter"), None);
    // Env fallback works when the store is silent.
    std::env::set_var("OR_TOKEN", "env-key");
    assert_eq!(
        rof::tui::auth::key_for("openrouter"),
        Some("env-key".to_string())
    );
    // The store wins over env.
    rof::tui::auth::store().save("openrouter", "store-key").unwrap();
    assert_eq!(
        rof::tui::auth::key_for("openrouter"),
        Some("store-key".to_string())
    );
    // Registry providers resolve from the store only (no blanket env fill).
    assert_eq!(rof::tui::auth::key_for("acme"), None);
    if let Some(v) = saved {
        std::env::set_var("OR_TOKEN", v);
    } else {
        std::env::remove_var("OR_TOKEN");
    }
    std::env::remove_var("ROF_CREDENTIALS");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn custom_provider_registry_round_trips() {
    let dir = std::env::temp_dir().join(format!("rof-providers-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("ROF_PROVIDERS", dir.join("providers.json"));
    // Non-URLs are rejected before anything is stored.
    assert!(rof::tui::auth::save_provider("evil", "ftp://x").is_err());
    rof::tui::auth::save_provider("acme", "https://llm.acme.test/v1").unwrap();
    assert_eq!(
        rof::tui::auth::base_for_test("acme").unwrap(),
        "https://llm.acme.test/v1"
    );
    // Trailing slashes are trimmed so URL joining stays stable.
    rof::tui::auth::save_provider("acme2", "https://x.test/v1/").unwrap();
    assert_eq!(
        rof::tui::auth::base_for_test("acme2").unwrap(),
        "https://x.test/v1"
    );
    assert!(rof::tui::auth::remove_provider("acme").unwrap());
    assert!(rof::tui::auth::base_for_test("acme").is_err());
    // Unknown names error before any network.
    assert!(rof::tui::auth::base_for_test("nope").is_err());
    std::env::remove_var("ROF_PROVIDERS");
    std::fs::remove_dir_all(&dir).unwrap();
}
