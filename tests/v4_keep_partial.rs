// Keep-partial rounds: a failed round's landed patches survive to the next
// round of the same attempt instead of being rolled back (ROF_KEEP_PARTIAL).
// Off by default: every byte of the historical run must be unchanged without it.
#[test]
fn keep_partial_defaults_off() {
    assert!(!rof::config::AppConfig::default().keep_partial);
}

#[test]
fn keep_partial_survives_a_config_round_trip() {
    let cfg = rof::config::AppConfig {
        keep_partial: true,
        ..Default::default()
    };
    let back: rof::config::AppConfig =
        serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
    assert!(back.keep_partial);
}
