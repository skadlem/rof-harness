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

// ---- P3 F2: composer completion off the registry -------------------------

use rof::tui::cmd::{
    command_candidates, complete, provider_candidates, Completion, BUILTIN_PROVIDERS, COMMANDS,
};

/// The registry is the candidate list, so nothing can be missing from
/// completion: every registered name is offered, and completion offers
/// nothing the registry does not name.
#[test]
fn every_registered_command_is_a_completion_candidate() {
    let candidates = command_candidates();
    assert_eq!(
        candidates.len(),
        COMMANDS.len(),
        "completion and the registry disagree in size: {candidates:?} vs {COMMANDS:?}"
    );
    for name in COMMANDS {
        let want = format!("/{name}");
        assert!(
            candidates.contains(&want),
            "the registry names {want} but completion does not offer it: {candidates:?}"
        );
    }
}

/// The drift guard: a command added to the parser without being registered
/// is offered by nobody, and this fails. The arms of `parse` are read out
/// of the source, because a name the registry has never heard of cannot be
/// discovered any other way.
#[test]
fn a_command_added_to_the_parser_but_not_to_the_registry_fails() {
    let source = include_str!("../src/tui/cmd.rs");
    // Only `parse`'s own body: `busy_line` below it matches the same
    // indentation with posture names, which are not commands.
    let body = source
        .split_once("pub fn parse(")
        .expect("cmd.rs has no parse")
        .1
        .split_once("pub fn help_text(")
        .expect("cmd.rs has no help_text")
        .0;
    let mut seen = 0;
    for line in body.lines() {
        // The top-level arms of `match name {` are the only lines indented
        // eight spaces that start with a quoted name and dispatch on it.
        let Some(rest) = line.strip_prefix("        \"") else {
            continue;
        };
        let Some(name) = rest.split("\" =>").next() else {
            continue;
        };
        seen += 1;
        assert!(
            COMMANDS.contains(&name),
            "/{name} parses but is not registered, so completion never offers it"
        );
    }
    assert!(seen >= 20, "the arm scan found nothing to check: {seen}");
}

/// A prefix with exactly one match completes on the first press, and says
/// nothing else: no candidate list, because there is no choice to make.
#[test]
fn one_match_completes_without_a_second_keystroke() {
    assert_eq!(
        complete("/att"),
        Completion::Completed {
            text: "/attempts".to_string(),
            candidates: vec![],
        }
    );
    // Pressing again once the line is already the full command changes
    // nothing and reports no choice: there is nothing left to decide.
    assert_eq!(
        complete("/attempts"),
        Completion::Completed {
            text: "/attempts".to_string(),
            candidates: vec![],
        }
    );
    // A trailing space moves past the name into the argument, which for
    // `/attempts` is a number this process has no metadata for.
    assert!(
        matches!(complete("/att "), Completion::NoMatch { .. }),
        "a completed name plus a space is the argument position"
    );
    // A bare `/` matches everything: the whole list is reported and
    // nothing is inserted, because every command shares the `/`.
    let all = complete("/");
    assert_eq!(all.text(), "/", "the bare slash must not grow a name");
    assert_eq!(
        all.candidates().len(),
        COMMANDS.len(),
        "the bare slash must offer the whole registry: {:?}",
        all.candidates()
    );
}

/// Several matches complete to their longest common prefix and REPORT the
/// candidates: the completion never picks one silently. Pressing it again
/// at the common prefix adds nothing and repeats the same list, so the
/// behaviour is deterministic rather than a slow auto-pick.
#[test]
fn several_matches_complete_to_the_common_prefix_and_list_the_rest() {
    let first = complete("/pro");
    let candidates = first.candidates();
    assert_eq!(
        candidates,
        vec![
            "/profile".to_string(),
            "/provider".to_string(),
            "/providers".to_string()
        ],
        "the ambiguous set is not what the registry says"
    );
    assert_eq!(first.text(), "/pro", "the common prefix was not inserted");
    // Deterministic: the second press is the same answer, not a pick.
    assert_eq!(
        complete(first.text()),
        first,
        "completion is not idempotent"
    );
    // An ambiguous answer always carries its candidates, and a unique one
    // never does: that is the difference between "here is your line" and
    // "here are your options".
    assert!(
        !first.candidates().is_empty(),
        "ambiguous, so it must report"
    );
    assert_eq!(
        complete("/att").candidates(),
        Vec::<String>::new(),
        "unique, so it must not report"
    );
}

/// No match changes nothing and says so: the draft is returned exactly as
/// it was typed, and the reason names the prefix that failed.
#[test]
fn no_match_changes_nothing_and_says_so() {
    for draft in ["/zzz", "/zzz ", "fix the composer"] {
        match complete(draft) {
            Completion::NoMatch { text, reason } => {
                assert_eq!(text, draft, "a no-match rewrote the draft");
                assert!(!reason.is_empty(), "a no-match said nothing");
            }
            other => panic!("{draft} should not complete, got {other:?}"),
        }
    }
    assert!(
        complete("/zzz").reason().contains("/zzz"),
        "the reason does not name the prefix: {}",
        complete("/zzz").reason()
    );
}

/// Provider candidates are ids, and every id is a provider `auth` can
/// resolve a base for. Built-ins are the same set `/providers` prints.
#[test]
fn provider_candidates_are_resolvable_ids_never_a_base_or_a_key() {
    let candidates = provider_candidates();
    for builtin in BUILTIN_PROVIDERS {
        assert!(
            candidates.contains(&builtin.to_string()),
            "the built-in provider {builtin} is not a candidate: {candidates:?}"
        );
    }
    let mut sorted = candidates.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted, candidates, "candidates must be sorted and unique");
    for candidate in &candidates {
        assert!(
            !candidate.contains("://"),
            "a base URL reached the candidates: {candidate}"
        );
        assert!(
            !candidate.contains("sk-"),
            "credential-shaped text reached the candidates: {candidate}"
        );
    }
    // `custom` is env-gated, so the three fixed-base built-ins are what can
    // be checked offline: a name here that auth cannot resolve would be a
    // candidate that no `/login` could ever use.
    for builtin in BUILTIN_PROVIDERS.iter().filter(|b| **b != "custom") {
        assert!(
            rof::tui::auth::base_for_test(builtin).is_ok(),
            "{builtin} is a candidate but has no base"
        );
    }
}

/// The provider-taking commands complete their argument, and the other
/// commands say there is nothing there rather than guessing.
#[test]
fn provider_commands_complete_their_argument_and_others_do_not() {
    assert_eq!(
        complete("/login openr"),
        Completion::Completed {
            text: "/login openrouter".to_string(),
            candidates: vec![],
        }
    );
    assert_eq!(complete("/logout at").text(), "/logout atria");
    // A model id is `provider/model`, so the provider half completes to
    // the separator and the model half is left to the user.
    assert_eq!(complete("/model openr").text(), "/model openrouter/");
    for draft in ["/attempts 3", "/retry note", "/busy queue"] {
        assert!(
            matches!(complete(draft), Completion::NoMatch { .. }),
            "{draft} has no completable argument"
        );
    }
}
