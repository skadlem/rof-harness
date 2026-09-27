// Learn mode, slice A: the user-knowledge store (`~/.rof/PROFILE.md`).
//
// Hermetic by construction: every test points `ROF_PROFILE` at its own
// scratch file and builds its own temp workdir, so no test can read or
// write a real `~/.rof/PROFILE.md`. `ROF_PROFILE` is process-global, so
// the env-touching tests share one lock, the same idiom `tests/tui_live.rs`
// uses for the credentials override.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rof::context::memory;
use rof::context::profile::{self, Edit, Entry, Profile, Scope, State};
use rof::obs::TraceSink;
use rof::tui::app::App;
use rof::tui::cmd::{parse, Action, ProfileCmd};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// One scratch profile file plus one scratch workdir, removed on drop.
struct Scratch {
    dir: PathBuf,
    profile: PathBuf,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "rof-learn-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("workdir")).unwrap();
        let profile = dir.join("PROFILE.md");
        std::env::set_var(profile::PATH_ENV, &profile);
        Scratch {
            dir,
            profile,
            _lock: lock,
        }
    }

    fn workdir(&self) -> PathBuf {
        self.dir.join("workdir")
    }

    fn write(&self, body: &str) {
        std::fs::write(&self.profile, body).unwrap();
    }

    fn read(&self) -> String {
        std::fs::read_to_string(&self.profile).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::env::remove_var(profile::PATH_ENV);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn write_profile(s: &Scratch, entries: &[&str], notes: &str) {
    s.write(&format!(
        "```json\n[\n{}\n]\n```\n{notes}\n",
        entries.join(",\n")
    ));
}

/// Drive the `/profile` command exactly as the console's between-goals path
/// does: `parse` first, then `apply_action` on the raw line.
fn run_profile_command(raw: &str) -> Vec<String> {
    let action = parse(raw).unwrap_or_else(|| panic!("{raw} must parse as a command"));
    assert!(
        !matches!(action, Action::Unknown(_)),
        "{raw} must not be Unknown"
    );
    let mut app = App::new();
    let trace = TraceSink::new();
    let mut awaiting_key: Option<String> = None;
    let quit = rof::tui::run::apply_action(&mut app, &trace, action, raw, &mut awaiting_key);
    assert!(!quit, "{raw} must not end the console");
    app.transcript
}

// ---------------------------------------------------------------------------
// 1. A missing profile is an empty store, and a run still works.
// ---------------------------------------------------------------------------

/// Day one of learn mode is a file that does not exist, and it is not an
/// error: the store is empty, no warning is raised, and the context path
/// that reads it still produces a head.
#[test]
fn a_missing_profile_is_an_empty_store_and_a_run_still_builds_its_head() {
    let s = Scratch::new("missing");
    assert!(!s.profile.exists(), "the fixture must start with no file");

    let p = profile::load();
    assert!(p.entries.is_empty(), "a missing file is not an empty store");
    assert_eq!(p.warning, None, "an absent file is not a problem: {p:?}");

    // The run's own path: memory::load + memory::render, in that order,
    // with project conventions present. Neither step can fail on the
    // profile, so the head is exactly the conventions.
    std::fs::write(s.workdir().join("AGENTS.md"), "conventions: tabs").unwrap();
    let mem = memory::load(&s.workdir());
    let head = memory::render(&mem);
    assert!(head.contains("PROJECT MEMORY"), "head: {head}");
    assert!(head.contains("tabs"), "head: {head}");
    assert!(
        !head.contains("USER PROFILE"),
        "an empty store must add no section: {head}"
    );
}

// ---------------------------------------------------------------------------
// 2. Round-trip: state, scope and evidence survive a write and a read.
// ---------------------------------------------------------------------------

/// The file is machine-readable AND hand-editable in one place, so a write
/// followed by a read must be lossless for the three fields the two derived
/// sets and the scope filter actually depend on.
#[test]
fn entries_round_trip_with_state_scope_and_evidence_intact() {
    let s = Scratch::new("roundtrip");
    let mut p = Profile::default();
    p.apply(Edit::Add {
        concept: "retry backoff".into(),
        scope: Scope::Global,
        evidence: "asked what happens on a 429".into(),
    })
    .unwrap();
    p.apply(Edit::Add {
        concept: "auth token layout".into(),
        scope: Scope::Repo("rof-harness".into()),
        evidence: "asked where the token is read".into(),
    })
    .unwrap();
    p.apply(Edit::AssumeKnown("retry backoff".into())).unwrap();
    p.notes = "hand-written note: I prefer small PRs.".into();
    profile::save(&p).unwrap();

    // Human-editable: the notes below the block survive a machine write.
    assert!(
        s.read().contains("I prefer small PRs."),
        "the free-form notes were lost: {}",
        s.read()
    );

    let back = profile::load();
    assert_eq!(back.warning, None, "{:?}", back.warning);
    assert_eq!(
        back.notes.trim(),
        "hand-written note: I prefer small PRs.",
        "notes lost"
    );
    let backoff = back
        .entries
        .iter()
        .find(|e| e.concept == "retry backoff")
        .expect("entry survived");
    assert_eq!(backoff.state, State::Explained);
    assert_eq!(backoff.scope, Scope::Global);
    assert_eq!(backoff.evidence, "asked what happens on a 429");
    let auth = back
        .entries
        .iter()
        .find(|e| e.concept == "auth token layout")
        .expect("entry survived");
    assert_eq!(auth.state, State::NotExplained);
    assert_eq!(auth.scope, Scope::Repo("rof-harness".into()));
    assert!(!auth.first_mentioned.trim().is_empty(), "timestamp missing");
}

// ---------------------------------------------------------------------------
// 3. A malformed block degrades to an empty store with a reason, and never
//    takes a run down.
// ---------------------------------------------------------------------------

/// The store is a file a human edits, so it WILL be malformed at some
/// point. The failure mode is a wrong answer, never a dead session: the
/// entries degrade to empty, the reason is surfaced, and the context path
/// still builds a head.
#[test]
fn a_malformed_block_degrades_to_an_empty_store_with_a_reason_and_never_fails_a_run() {
    let s = Scratch::new("malformed");
    s.write("```json\n{ not json at all\n```\n");
    let p = profile::load();
    assert!(p.entries.is_empty(), "a bad block must not yield entries");
    let warning = p
        .warning
        .as_deref()
        .unwrap_or_else(|| panic!("a degraded store must say why: {p:?}"));
    assert!(!warning.is_empty(), "the reason must not be empty");

    // The run path: no error type anywhere, just a head.
    let mem = memory::load(&s.workdir());
    let head = memory::render(&mem);
    assert!(
        !head.contains("USER PROFILE"),
        "a bad block adds nothing: {head}"
    );

    // A file with no block at all is the same degradation, not a panic.
    s.write("just some prose, no machine block\n");
    let prose = profile::load();
    assert!(prose.entries.is_empty());
    assert!(prose.warning.is_some(), "a missing block is a degradation");
    let _ = memory::render(&memory::load(&s.workdir()));
}

// ---------------------------------------------------------------------------
// 4. Evidence is mandatory: an unevidenced entry is rejected, not stored.
// ---------------------------------------------------------------------------

/// The whole point of the file is that it is a set of assumptions ABOUT A
/// PERSON. An assumption with no cited prompt is unreviewable, so `add`
/// refuses it and stores nothing.
#[test]
fn evidence_is_mandatory_and_a_blank_add_is_refused() {
    let _s = Scratch::new("evidence");
    let mut p = Profile::default();
    for evidence in ["", "   ", "\t\n"] {
        let err = p
            .apply(Edit::Add {
                concept: "retry backoff".into(),
                scope: Scope::Global,
                evidence: evidence.into(),
            })
            .unwrap_err();
        assert!(
            err.contains("evidence"),
            "the refusal must name the missing field: {err}"
        );
    }
    assert!(
        p.entries.is_empty(),
        "a refused add must not be stored: {:?}",
        p.entries
    );
    profile::save(&p).unwrap();
    assert!(
        profile::load().entries.is_empty(),
        "nothing reached the file"
    );

    // The console path refuses it the same way, and says why.
    let out = run_profile_command("/profile add retry backoff global");
    assert!(
        out.iter().any(|l| l.contains("evidence")),
        "the console must refuse and explain: {out:?}"
    );
}

// ---------------------------------------------------------------------------
// 5. One entry list, two DERIVED sets. They cannot drift apart.
// ---------------------------------------------------------------------------

/// `assumed_known` is `explained | understood` and `assumed_unknown` is
/// `not_explained`, both computed from the single stored list. A test
/// cannot prove drift is impossible by inspecting two snapshots; it proves
/// it by checking the partition for EVERY state, including the one only a
/// user command may create.
#[test]
fn the_two_sets_partition_the_entries_and_cannot_disagree_with_the_states() {
    let mut p = Profile::default();
    for (concept, scope) in [
        ("alpha", Scope::Global),
        ("beta", Scope::Repo("other".into())),
    ] {
        p.apply(Edit::Add {
            concept: concept.into(),
            scope,
            evidence: "cited".into(),
        })
        .unwrap();
    }
    p.apply(Edit::AssumeKnown("alpha".into())).unwrap();
    p.apply(Edit::AssumeUnderstood("beta".into())).unwrap();

    let known: Vec<&str> = p
        .assumed_known()
        .iter()
        .map(|e| e.concept.as_str())
        .collect();
    let unknown: Vec<&str> = p
        .assumed_unknown()
        .iter()
        .map(|e| e.concept.as_str())
        .collect();
    assert_eq!(known, vec!["alpha", "beta"], "understood is assumed_known");
    assert!(
        unknown.is_empty(),
        "nothing is assumed unknown: {unknown:?}"
    );

    // The partition: every entry is in exactly one set, for every state.
    p.apply(Edit::Add {
        concept: "gamma".into(),
        scope: Scope::Global,
        evidence: "cited".into(),
    })
    .unwrap();
    p.apply(Edit::AssumeUnknown("alpha".into())).unwrap();
    for entry in &p.entries {
        let in_known = p.assumed_known().iter().any(|e| e.concept == entry.concept);
        let in_unknown = p
            .assumed_unknown()
            .iter()
            .any(|e| e.concept == entry.concept);
        assert!(
            in_known ^ in_unknown,
            "{} is in {in_known} known and {in_unknown} unknown",
            entry.concept
        );
    }
    let known: Vec<&str> = p
        .assumed_known()
        .iter()
        .map(|e| e.concept.as_str())
        .collect();
    assert_eq!(known, vec!["beta"], "moving alpha back moved it out");

    // The membership rule itself, read off the states rather than a list.
    for entry in &p.entries {
        let expected = matches!(entry.state, State::Explained | State::Understood);
        assert_eq!(
            expected,
            p.assumed_known().iter().any(|e| e.concept == entry.concept),
            "{} disagrees with its own state {:?}",
            entry.concept,
            entry.state
        );
    }
}

// ---------------------------------------------------------------------------
// 6. Scopes: global always, repo:<name> only on a matching workdir.
// ---------------------------------------------------------------------------

/// "Cross-repo" cannot mean "load everything everywhere": a profile that
/// accumulates one project's auth layout and is then loaded into an
/// unrelated repo leaks that project's internals into another's context.
#[test]
fn repo_scoped_entries_load_only_for_the_matching_workdir() {
    let s = Scratch::new("scopes");
    s.write(&format!(
        "```json\n[\n{}\n]\n```\n",
        [
            r#"{"concept":"style","state":"explained","scope":"global","evidence":"asked for tabs","first_mentioned":"2026-09-27"}"#,
            r#"{"concept":"auth","state":"explained","scope":"repo:{}","evidence":"asked where the token is read","first_mentioned":"2026-09-27"}"#,
            r#"{"concept":"retry","state":"not_explained","scope":"repo:other-repo","evidence":"asked what a 429 does","first_mentioned":"2026-09-27"}"#,
        ]
        .join(",\n")
            .replace("{}", &profile::repo_name(&s.workdir()))
    ));

    let p = profile::load();
    let visible: Vec<&str> = p
        .in_scope(&s.workdir())
        .iter()
        .map(|e| e.concept.as_str())
        .collect();
    assert_eq!(
        visible,
        vec!["style", "auth"],
        "repo:other-repo leaked into an unrelated workdir"
    );
    // Not hidden from the store, only from this workdir's head.
    assert_eq!(p.entries.len(), 3, "the store still holds all three");
    assert!(
        p.assumed_unknown().iter().any(|e| e.concept == "retry"),
        "the other repo's entry is still there"
    );

    // The head is what the loop actually consumes, through the one path
    // memory already had.
    let head = memory::render(&memory::load(&s.workdir()));
    assert!(head.contains("USER PROFILE"), "head: {head}");
    assert!(
        head.contains("style") && head.contains("auth"),
        "head: {head}"
    );
    assert!(
        !head.contains("other-repo"),
        "an unrelated repo's entry reached the head: {head}"
    );
}

/// A different workdir gets a different in-scope view of the SAME file:
/// the filter is on the workdir, not on the load.
#[test]
fn a_different_workdir_sees_only_the_global_entries_of_the_same_file() {
    let s = Scratch::new("scopes-other");
    write_profile(
        &s,
        &[
            r#"{"concept":"style","state":"explained","scope":"global","evidence":"cited","first_mentioned":"2026-09-27"}"#,
            r#"{"concept":"auth","state":"explained","scope":"repo:rof-harness","evidence":"cited","first_mentioned":"2026-09-27"}"#,
        ],
        "",
    );
    let elsewhere = s.dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let p = profile::load();
    let visible: Vec<&str> = p
        .in_scope(&elsewhere)
        .iter()
        .map(|e| e.concept.as_str())
        .collect();
    assert_eq!(visible, vec!["style"], "repo entry leaked elsewhere");
}

/// The repo name is the workdir's basename, decided once and documented:
/// the store is hand-edited and shared across checkouts, so a name that
/// changes when the repo moves or is cloned elsewhere would silently stop
/// matching, while a basename is what the user calls the project.
#[test]
fn the_repo_name_is_the_workdir_basename() {
    assert_eq!(
        profile::repo_name(Path::new("/home/madiyar/code/rof-harness")),
        "rof-harness"
    );
    // A trailing separator is the same directory, not a nameless one.
    assert_eq!(
        profile::repo_name(Path::new("/home/madiyar/code/rof-harness/")),
        "rof-harness"
    );
    let s = Scratch::new("repo-name");
    assert_eq!(profile::repo_name(&s.workdir()), "workdir");
}

// ---------------------------------------------------------------------------
// 7. The profile cannot crowd out project conventions.
// ---------------------------------------------------------------------------

/// The head is a fixed budget. An entry list is appendable by hand with no
/// cap of its own, so the bound is enforced where the head is built, and
/// asserted: a profile far larger than the whole budget still leaves the
/// project conventions intact.
#[test]
fn a_huge_profile_cannot_evict_project_conventions_from_the_head() {
    let s = Scratch::new("bound");
    let conventions = "conventions: tabs, no panics, tests before commit";
    std::fs::write(s.workdir().join("AGENTS.md"), conventions).unwrap();

    // 40k chars of evidence: twenty times the whole head budget.
    let mut entries: Vec<String> = Vec::new();
    for i in 0..400 {
        entries.push(format!(
            r#"{{"concept":"concept-{i}","state":"explained","scope":"global","evidence":"{}","first_mentioned":"2026-09-27"}}"#,
            "e".repeat(100)
        ));
    }
    s.write(&format!("```json\n[\n{}\n]\n```\n", entries.join(",\n")));

    let head = memory::render(&memory::load(&s.workdir()));
    assert!(head.contains(conventions), "AGENTS.md was crowded out");
    let profile_section = head
        .split("[USER PROFILE]")
        .nth(1)
        .unwrap_or_else(|| panic!("no profile section: {head}"))
        .trim();
    assert!(
        profile_section.chars().count() <= profile::HEAD_CAP,
        "the profile took {} chars of a {}-char budget",
        profile_section.chars().count(),
        profile::HEAD_CAP
    );
    // And the profile never comes before the conventions: the project
    // section is the one the repo owns, so it leads.
    let project_at = head.find("[PROJECT MEMORY]").expect("project section");
    let profile_at = head.find("[USER PROFILE]").expect("profile section");
    assert!(
        project_at < profile_at,
        "profile displaced the project head"
    );
}

/// The profile reaches the head in BOTH loops, and through the one path
/// memory already had. This is asserted at the call sites rather than by
/// running a goal: the two loops each build `mem_text` from
/// `memory::load` + `memory::render`, and a third section that lived in
/// either loop directly would drift out of the other. If a future slice
/// adds a loop, or bypasses `render`, this fails.
#[test]
fn both_loops_reach_the_profile_through_the_one_memory_render_path() {
    let src = include_str!("../src/engine/orchestrator.rs");
    let loads = src.matches("crate::context::memory::load(").count();
    let renders = src.matches("crate::context::memory::render(").count();
    assert_eq!(
        loads, 2,
        "expected the pipeline and the direct loop, found {loads} memory::load call sites"
    );
    assert_eq!(
        renders, 2,
        "one render per loop, or a loop has its own path: found {renders}"
    );
    // And the profile is not smuggled into a prompt anywhere else.
    let other: Vec<&str> = src.lines().filter(|l| l.contains("profile")).collect();
    assert!(
        other.is_empty(),
        "the orchestrator references the profile directly instead of leaving it to memory::render: {other:?}"
    );
}

/// A run with a profile in scope: the head carries it, and the section is
/// the rendered rows, not the whole file (notes are not context).
#[test]
fn the_head_carries_the_rows_and_not_the_human_notes() {
    let s = Scratch::new("head-rows");
    std::fs::write(s.workdir().join("AGENTS.md"), "tabs").unwrap();
    s.write(&format!(
        "```json\n[{}]\n```\nPRIVATE NOTE: do not read this aloud\n",
        r#"{"concept":"retry backoff","state":"explained","scope":"global","evidence":"asked about 429s","first_mentioned":"2026-09-27"}"#
    ));
    let head = memory::render(&memory::load(&s.workdir()));
    assert!(head.contains("retry backoff"), "head: {head}");
    assert!(
        head.contains("explained") && head.contains("global"),
        "head: {head}"
    );
    assert!(
        !head.contains("PRIVATE NOTE"),
        "the free-form notes are not context: {head}"
    );
}

/// `assumed_known` / `assumed_unknown` are the spec's two sets and nothing
/// more: there is no third bucket of "observed facts", because nothing in
/// the harness observes competence.
#[test]
fn there_is_no_third_bucket_of_observed_facts() {
    let mut p = Profile::default();
    p.apply(Edit::Add {
        concept: "alpha".into(),
        scope: Scope::Global,
        evidence: "cited".into(),
    })
    .unwrap();
    let total = p.assumed_known().len() + p.assumed_unknown().len();
    assert_eq!(
        total,
        p.entries.len(),
        "an entry is in neither derived set: {} entries, {total} classified",
        p.entries.len()
    );
}

// ---------------------------------------------------------------------------
// 8. `/profile` is display plus explicit manual edit — the only writers.
// ---------------------------------------------------------------------------

/// The store is inspectable in full: a row that hides the state, the scope
/// or the evidence hides the thing a wrong assumption is corrected with.
#[test]
fn profile_list_shows_state_scope_and_evidence() {
    let s = Scratch::new("list");
    write_profile(
        &s,
        &[
            r#"{"concept":"retry backoff","state":"explained","scope":"global","evidence":"asked on 2026-09-27","first_mentioned":"2026-09-27"}"#,
        ],
        "",
    );
    let out = run_profile_command("/profile list");
    let row = out
        .iter()
        .find(|l| l.contains("retry backoff"))
        .unwrap_or_else(|| panic!("no row for the entry: {out:?}"));
    for field in ["explained", "global", "asked on 2026-09-27"] {
        assert!(row.contains(field), "the row hides {field}: {row}");
    }
}

/// The two derived sets are their own subcommands, and what they print is
/// the derived view — the same partition, read straight off the states.
#[test]
fn profile_known_and_unknown_print_the_two_derived_sets() {
    let _s = Scratch::new("known-unknown");
    let mut p = Profile::default();
    p.apply(Edit::Add {
        concept: "alpha".into(),
        scope: Scope::Global,
        evidence: "cited".into(),
    })
    .unwrap();
    p.apply(Edit::Add {
        concept: "beta".into(),
        scope: Scope::Global,
        evidence: "cited".into(),
    })
    .unwrap();
    p.apply(Edit::AssumeKnown("alpha".into())).unwrap();
    profile::save(&p).unwrap();

    let known = run_profile_command("/profile known");
    assert!(known.iter().any(|l| l.contains("alpha")), "{known:?}");
    assert!(!known.iter().any(|l| l.contains("beta")), "{known:?}");
    let unknown = run_profile_command("/profile unknown");
    assert!(unknown.iter().any(|l| l.contains("beta")), "{unknown:?}");
    assert!(!unknown.iter().any(|l| l.contains("alpha")), "{unknown:?}");
}

/// The three mutations, end to end through the console, and each one is a
/// user command: there is no fourth way to change the file.
#[test]
fn add_assume_and_forget_work_through_the_console() {
    let _s = Scratch::new("mutate");
    let out = run_profile_command("/profile add retry backoff global asked what a 429 does");
    assert!(out.iter().any(|l| l.contains("retry backoff")), "{out:?}");
    assert_eq!(profile::load().entries.len(), 1, "add wrote one entry");
    assert_eq!(profile::load().entries[0].state, State::NotExplained);

    let out = run_profile_command("/profile assume-known retry backoff");
    assert!(out.iter().any(|l| l.contains("explained")), "{out:?}");
    assert_eq!(profile::load().entries[0].state, State::Explained);

    let out = run_profile_command("/profile assume-unknown retry backoff");
    assert!(out.iter().any(|l| l.contains("not_explained")), "{out:?}");
    assert_eq!(profile::load().entries[0].state, State::NotExplained);

    let out = run_profile_command("/profile forget retry backoff");
    assert!(out.iter().any(|l| l.contains("forgot")), "{out:?}");
    assert!(profile::load().entries.is_empty(), "the entry is gone");
}

/// A repo-scoped entry is spelled `repo:<name>` and nothing else; the
/// closed parser refuses the rest by name rather than storing a scope that
/// can never match.
#[test]
fn the_scope_grammar_is_refused_at_the_parser() {
    assert!(matches!(
        parse("/profile add retry backoff repo:rof-harness asked about 429s"),
        Some(Action::Profile(ProfileCmd::Add { .. }))
    ));
    for bad in [
        "/profile add retry backoff local cited",
        "/profile add retry backoff repo: cited",
        "/profile add retry backoff cited",
        "/profile add retry backoff",
    ] {
        assert!(
            matches!(parse(bad), Some(Action::Unknown(_))),
            "{bad} must be refused, not guessed at"
        );
    }
    for (sub, line) in [
        ("list", "/profile list"),
        ("known", "/profile known"),
        ("unknown", "/profile unknown"),
        ("assume-known", "/profile assume-known retry backoff"),
        ("assume-unknown", "/profile assume-unknown retry backoff"),
        (
            "assume-understood",
            "/profile assume-understood retry backoff",
        ),
        ("forget", "/profile forget retry backoff"),
    ] {
        assert!(
            matches!(parse(line), Some(Action::Profile(_))),
            "/profile {sub} must parse, got {:?}",
            parse(line)
        );
    }
    assert!(matches!(parse("/profile"), Some(Action::Unknown(_))));
    assert!(matches!(parse("/profiles"), Some(Action::Unknown(_))));
}

/// `understood` is a claim about a person, so the harness may not create
/// one. This is asserted two ways, because the second is the one that
/// catches a future slice adding a writer by accident:
///
/// 1. Behaviourally: no edit other than the user's own command can produce
///    it, and reading/rendering the store never changes a state.
/// 2. Structurally: `State::Understood` is constructed in exactly one
///    place in the whole crate — the arm of the single write path that the
///    `/profile assume-understood` command reaches.
#[test]
fn no_path_reaches_understood_except_the_user_command() {
    let s = Scratch::new("understood");
    let mut p = Profile::default();
    p.apply(Edit::Add {
        concept: "alpha".into(),
        scope: Scope::Global,
        evidence: "cited".into(),
    })
    .unwrap();

    // Every other edit, in every direction, leaves the state alone.
    p.apply(Edit::Add {
        concept: "beta".into(),
        scope: Scope::Global,
        evidence: "cited".into(),
    })
    .unwrap();
    p.apply(Edit::AssumeKnown("beta".into())).unwrap();
    p.apply(Edit::AssumeUnknown("beta".into())).unwrap();
    p.apply(Edit::Forget("alpha".into())).unwrap();
    assert!(
        p.entries.iter().all(|e| e.state != State::Understood),
        "a non-command edit produced `understood`: {:?}",
        p.entries
    );

    // Reads do not move anything: the derived view and the head are pure.
    profile::save(&p).unwrap();
    let before = profile::load();
    let _ = before.assumed_known();
    let _ = before.assumed_unknown();
    let _ = memory::render(&memory::load(&s.workdir()));
    let after = profile::load();
    assert_eq!(before.entries, after.entries, "a read changed the store");

    // The only writer of `understood` is a user command, and it works.
    run_profile_command("/profile add gamma global cited a prompt");
    let out = run_profile_command("/profile assume-understood gamma");
    assert!(out.iter().any(|l| l.contains("understood")), "{out:?}");
    assert_eq!(
        profile::load()
            .entries
            .iter()
            .find(|e| e.concept == "gamma")
            .expect("gamma is stored")
            .state,
        State::Understood
    );
}

/// The structural half: `State::Understood` is built in exactly one place
/// in the entire crate, and that place is inside the one public function
/// that takes `&mut Profile`, reached only by an `Edit`.
#[test]
fn understood_is_constructed_once_in_the_crate_inside_the_write_path() {
    let src = include_str!("../src/context/profile.rs");
    let apply_at = src.find("pub fn apply(").expect("profile.rs has no apply");
    let arm_at = src
        .find("Edit::AssumeUnderstood")
        .expect("profile.rs has no AssumeUnderstood arm");
    let constructions: Vec<(String, usize)> = all_sources()
        .into_iter()
        .flat_map(|(path, text)| {
            text.lines()
                .enumerate()
                .filter(|(_, line)| line.contains("State::Understood"))
                .filter(|(_i, line)| {
                    // Definitions, doc comments and match patterns are not
                    // constructions; `=> State::Understood` and a `let` are.
                    let t = line.trim();
                    !t.starts_with("//")
                        && !t.starts_with("///")
                        && !t.starts_with("Understood")
                        && (t.contains("=> State::Understood")
                            || t.starts_with("let ")
                            || t.contains("= State::Understood"))
                })
                .map(|(i, line)| {
                    let at = text.lines().take(i).map(|l| l.len() + 1).sum::<usize>()
                        + line.find("State::Understood").unwrap_or(0);
                    (format!("{path}:{}", i + 1), at)
                })
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(
        constructions.len(),
        1,
        "State::Understood is constructed more than once: {constructions:?}"
    );
    let (site, at) = &constructions[0];
    assert!(
        site.contains("context/profile.rs"),
        "the construction is outside the store: {site}"
    );
    assert!(
        *at > apply_at && *at > arm_at,
        "the construction at {site} is not inside the apply() AssumeUnderstood arm"
    );
}

/// The write surface is one function. A second `&mut self` / `&mut Profile`
/// entry point is how a future slice would add a writer by accident.
#[test]
fn the_store_has_exactly_one_mutating_entry_point() {
    let src = include_str!("../src/context/profile.rs");
    let mutators: Vec<&str> = src
        .lines()
        .filter(|l| l.trim_start().starts_with("pub fn "))
        .filter(|l| l.contains("&mut self") || l.contains("&mut Profile"))
        .collect();
    assert_eq!(
        mutators.len(),
        1,
        "the store grew a second writer: {mutators:?}"
    );
    assert!(
        mutators[0].contains("apply"),
        "the writer is not `apply`: {}",
        mutators[0]
    );
}

/// Every source file under `src/`, for the structural tests above. Read
/// with `std::fs` so the test has no dependency the crate does not have.
fn all_sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push((path.display().to_string(), text));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
    out
}

/// The entry type is the whole contract: the five stored fields, so a
/// hand-edited row that drops one is a degradation, not a silent default.
#[test]
fn an_entry_keeps_its_five_fields_across_a_hand_edit() {
    let s = Scratch::new("handedit");
    write_profile(
        &s,
        &[
            r#"{"concept":"retry backoff","state":"understood","scope":"repo:rof-harness","evidence":"answered the probe","first_mentioned":"2026-09-27"}"#,
        ],
        "notes below",
    );
    let p = profile::load();
    assert_eq!(p.entries.len(), 1);
    let e: &Entry = &p.entries[0];
    assert_eq!(e.concept, "retry backoff");
    assert_eq!(e.state, State::Understood);
    assert_eq!(e.scope, Scope::Repo("rof-harness".into()));
    assert_eq!(e.evidence, "answered the probe");
    assert_eq!(e.first_mentioned, "2026-09-27");
    assert!(p.assumed_known().iter().any(|k| k.concept == e.concept));
}
