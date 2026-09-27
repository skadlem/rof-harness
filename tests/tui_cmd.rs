// tests/tui_cmd.rs
use rof::tui::cmd::{parse, Action};

#[test]
fn commands_parse_at_message_start_only() {
    assert!(matches!(parse("/quit"), Some(Action::Quit)));
    assert!(matches!(parse("/attempts 3"), Some(Action::Attempts(3))));
    assert!(matches!(
        parse("/model go/deepseek-flash"),
        Some(Action::Model(_))
    ));
    assert!(parse("please /quit the session").is_none());
    assert!(parse("fix it").is_none());
    assert!(matches!(parse("/nope"), Some(Action::Unknown(_))));
    assert!(matches!(parse("/attempts 9"), Some(Action::Unknown(_))));
}

#[test]
fn provider_forms_parse() {
    assert!(matches!(
        parse("/provider add acme https://llm.acme.test/v1"),
        Some(Action::ProviderAdd(_))
    ));
    assert!(matches!(
        parse("/provider list"),
        Some(Action::ProviderList)
    ));
    assert!(matches!(
        parse("/provider rm acme"),
        Some(Action::ProviderRm(_))
    ));
    assert!(matches!(
        parse("/provider add acme"),
        Some(Action::Unknown(_))
    ));
    assert!(matches!(parse("/provider"), Some(Action::Unknown(_))));
}

/// `/providers` is its own display command and `/provider list` keeps the
/// mutation-grammar arm it always was: the two are not synonyms, and a
/// near-miss on either is still `Unknown` rather than a guess.
#[test]
fn providers_parses_to_its_own_action_and_neighbours_still_parse() {
    assert!(matches!(parse("/providers"), Some(Action::Providers)));
    assert!(
        matches!(parse("/provider list"), Some(Action::ProviderList)),
        "/provider list must keep parsing to the existing arm"
    );
    assert!(matches!(parse("/providers all"), Some(Action::Providers)));
    for unknown in ["/providersx", "/providerx list", "/provider lists"] {
        assert!(
            matches!(parse(unknown), Some(Action::Unknown(_))),
            "{unknown} should be Unknown"
        );
    }
}

/// `/theme` is the one command whose argument is optional: with no name it
/// LISTS, with a name it switches. Both forms parse to the same action, so
/// the listing and the switch cannot be two different commands.
#[test]
fn theme_parses_as_a_listing_without_a_name_and_a_switch_with_one() {
    assert!(matches!(parse("/theme"), Some(Action::Theme(None))));
    for name in ["default", "dark", "light"] {
        assert!(
            matches!(parse(&format!("/theme {name}")), Some(Action::Theme(Some(ref got))) if got == name),
            "/theme {name} must parse to its own name"
        );
    }
}

/// An unknown name is REFUSED by the closed parser and never resolved
/// later: nothing downstream can turn it into a quiet fallback to the
/// default theme. The refusal names the valid names, so the user can fix
/// the line without reading `/help` first.
#[test]
fn an_unknown_theme_name_is_refused_with_the_valid_names() {
    let action = parse("/theme neon").expect("/theme neon must parse");
    let message = match action {
        Action::Unknown(message) => message,
        other => panic!("/theme neon must be refused, got {other:?}"),
    };
    for name in ["default", "dark", "light"] {
        assert!(
            message.contains(name),
            "refusal omits the valid name {name}: {message}"
        );
    }
    assert!(
        !message.contains("neon dark") && !message.contains("silently"),
        "refusal does not report what it got: {message}"
    );
    // `/theme` alone still lists: a typo in the argument must not cost the
    // user the listing.
    assert!(matches!(parse("/theme"), Some(Action::Theme(None))));
}

#[test]
fn help_lists_everything_with_current_values_placeholder() {
    let h = rof::tui::cmd::help_text();
    for cmd in [
        "/quit",
        "/model",
        "/login",
        "/attempts",
        "/retry",
        "/undo",
        "/diff",
        "/providers",
        "/theme",
    ] {
        assert!(h.contains(cmd), "help mentions {cmd}");
    }
}

/// `/busy` is the one command whose copy has to describe behavior, so its
/// three modes are spelled out rather than listed: steer is what Enter
/// does by default, queue stores exactly one next goal, and interrupt is
/// the stop path.
#[test]
fn help_names_the_three_busy_modes_and_what_they_do() {
    let h = rof::tui::cmd::help_text();
    assert!(h.contains("/busy"), "help omits /busy: {h}");
    assert!(h.contains("steer"), "help omits steer: {h}");
    assert!(
        h.contains("default"),
        "help does not say steer is the default: {h}"
    );
    // The /busy line is the copy under test; "/hotkeys" above it is a
    // command name, not a credential.
    let busy = h
        .lines()
        .find(|line| line.starts_with("/busy:"))
        .unwrap_or_else(|| panic!("help has no /busy explanation: {h}"));
    assert!(
        busy.contains("queue stores exactly one next goal"),
        "help does not say what queue stores: {busy}"
    );
    assert!(busy.contains("interrupt"), "help omits interrupt: {busy}");
    assert!(
        busy.contains("stop path"),
        "help does not tie interrupt to the stop path: {busy}"
    );
    let lowered = busy.to_lowercase();
    assert!(
        !lowered.contains("key") && !lowered.contains("token"),
        "busy copy mentions a credential: {busy}"
    );
    assert!(
        !h.contains("P1a has no queue") && !h.contains("one goal at a time"),
        "help still carries the pre-queue wording: {h}"
    );
}
