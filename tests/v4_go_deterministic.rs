// Go-session header (x-opencode-session) + ctx-follows-exec fallback.
#[test]
fn opencode_base_gets_session_header_others_do_not() {
    let go = rof::llm::session_header_for_test("https://opencode.ai/zen/go/v1", "s1");
    assert_eq!(
        go,
        Some(("x-opencode-session".to_string(), "s1".to_string()))
    );
    assert!(rof::llm::session_header_for_test("https://api.deepseek.com", "s1").is_none());
    assert!(rof::llm::session_header_for_test("https://openrouter.ai/api/v1", "s1").is_none());
}

#[test]
fn client_session_id_is_stable_and_nonempty() {
    let a = rof::llm::client_session_for_test();
    let b = rof::llm::client_session_for_test();
    assert!(!a.is_empty());
    assert_ne!(
        a, b,
        "each client mints its own session; stability is per-client"
    );
}

#[test]
fn unset_context_model_follows_the_executor() {
    let r = rof::engine::ModelRouter::new("".into(), "strong".into(), None, None);
    assert_eq!(r.resolve(rof::engine::Role::Context), ("strong", None));
}

#[test]
fn explicit_context_model_still_wins() {
    let r = rof::engine::ModelRouter::new("cheap".into(), "strong".into(), None, None);
    assert_eq!(r.resolve(rof::engine::Role::Context), ("cheap", None));
}

#[test]
fn summarizer_is_off_by_default_mid_layer_too() {
    let p = rof::context::ContextPolicy::default();
    assert_eq!(p.layer(rof::context::LayerKind::Mid).summarize_at, 0.0);
}
