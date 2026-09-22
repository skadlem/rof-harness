// ROF_THINKING mapping: start thought-off/low instead of escalating after failure.
#[test]
fn thinking_modes_map_to_request_flags() {
    // off: answer directly, no reasoning budget to burn.
    assert_eq!(
        rof::agents::thinking_flags_for_test("off"),
        (true, true, false)
    );
    // low: bounded reasoning effort.
    assert_eq!(
        rof::agents::thinking_flags_for_test("low"),
        (false, false, true)
    );
    // on (default) and anything unknown: current behavior, all flags false.
    assert_eq!(
        rof::agents::thinking_flags_for_test("on"),
        (false, false, false)
    );
    assert_eq!(
        rof::agents::thinking_flags_for_test(""),
        (false, false, false)
    );
    assert_eq!(
        rof::agents::thinking_flags_for_test("banana"),
        (false, false, false)
    );
}
