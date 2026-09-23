// Per-call output caps + reviewer thinking shaping (bug02 next-action #1 enabler).
// Default-off: unset env leaves every wire byte-identical.
// Env-isolation invariant: these are the only tests in this binary that touch
// ROF_*_MAX_TOKENS / ROF_THINKING, and the two fns use disjoint var sets, so
// the harness's in-binary parallelism cannot race. Any future test in this
// file touching those vars must keep the sets disjoint or serialize.
#[test]
fn per_call_caps_follow_env_with_clamp() {
    // Unset → defaults (implementer 8192, reviewer 4096).
    std::env::remove_var("ROF_IMPLEMENTER_MAX_TOKENS");
    std::env::remove_var("ROF_REVIEWER_MAX_TOKENS");
    assert_eq!(
        rof::agents::max_tokens_from_env("ROF_IMPLEMENTER_MAX_TOKENS", 8192),
        8192
    );
    assert_eq!(
        rof::agents::max_tokens_from_env("ROF_REVIEWER_MAX_TOKENS", 4096),
        4096
    );
    // Set → honored.
    std::env::set_var("ROF_IMPLEMENTER_MAX_TOKENS", "4096");
    assert_eq!(
        rof::agents::max_tokens_from_env("ROF_IMPLEMENTER_MAX_TOKENS", 8192),
        4096
    );
    // Clamp both ends.
    std::env::set_var("ROF_IMPLEMENTER_MAX_TOKENS", "100");
    assert_eq!(
        rof::agents::max_tokens_from_env("ROF_IMPLEMENTER_MAX_TOKENS", 8192),
        1024
    );
    std::env::set_var("ROF_IMPLEMENTER_MAX_TOKENS", "99999");
    assert_eq!(
        rof::agents::max_tokens_from_env("ROF_IMPLEMENTER_MAX_TOKENS", 8192),
        32768
    );
    // Garbage → default.
    std::env::set_var("ROF_IMPLEMENTER_MAX_TOKENS", "banana");
    assert_eq!(
        rof::agents::max_tokens_from_env("ROF_IMPLEMENTER_MAX_TOKENS", 8192),
        8192
    );
    std::env::remove_var("ROF_IMPLEMENTER_MAX_TOKENS");
}

#[test]
fn reviewer_thinking_shaping_matches_implementer_modes() {
    // Same mapping the implementer uses: off starts thought-off, low bounded.
    std::env::set_var("ROF_THINKING", "off");
    assert_eq!(rof::agents::thinking_start_for_test(), (true, true, false));
    std::env::set_var("ROF_THINKING", "low");
    assert_eq!(rof::agents::thinking_start_for_test(), (false, false, true));
    std::env::remove_var("ROF_THINKING");
    assert_eq!(
        rof::agents::thinking_start_for_test(),
        (false, false, false)
    );
}
