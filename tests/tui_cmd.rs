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
    ] {
        assert!(h.contains(cmd), "help mentions {cmd}");
    }
}
