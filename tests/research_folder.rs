//! Per-repo research folder (`.rof/research/`), design spec §7, build item 5.
//!
//! Hermetic by construction: every test builds its own temp work root with
//! real git in it, and the one test that drives the console points
//! `ROF_RESEARCH_ROOT` at that root, so no test can read or write a real
//! repository's research folder. No PTY, no sleeps, no network.
//!
//! The claims under test are the ones a permissive implementation would
//! quietly break: a note is fresh only while its PINNED COMMIT still matches
//! the tree, "do I need new research?" is a function of that computation and
//! not a judgement, a note cannot leave the work root, a note about tests is
//! never served for a design question, and a hand-edited body survives every
//! machine write.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rof::context::research::{
    self, Edit, Freshness, Kind, Research, StaleReason, TreeState, DIR, INDEX_FILE, ROOT_ENV,
};
use rof::engine::session::is_test_shaped;
use rof::engine::tree::TreeService;
use rof::obs::TraceSink;
use rof::tui::app::App;
use rof::tui::cmd::{parse, Action, ResearchCmd};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// One real git work root plus the research folder inside it, removed on drop.
///
/// It holds no lock: a test may build several, and the global env override
/// below is taken only by the one test that drives the console, which is the
/// only thing in this file that reads `ROF_RESEARCH_ROOT`.
struct Scratch {
    dir: PathBuf,
    root: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("rof-research-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("work");
        std::fs::create_dir_all(&root).unwrap();
        // A committed one-file tree, so HEAD names a commit and the pinned
        // commit in a note is a real one.
        std::fs::write(root.join("a.txt"), "before\n").unwrap();
        let tree = TreeService::new(root.clone());
        tree.ensure().unwrap();
        tree.baseline().unwrap();
        Scratch { dir, root }
    }

    /// The commit a note pins when written now.
    fn head(&self) -> TreeState {
        let state = TreeState::read(&self.root);
        assert!(state.head().is_some(), "the fixture must have a HEAD");
        state
    }

    /// A NEW commit on top of the pinned one: the tree moved, nothing in the
    /// working tree is dirty, and no harness ran. This is the only input
    /// freshness is allowed to call stale-by-movement on.
    fn advance(&self) {
        std::fs::write(self.root.join("a.txt"), "after\n").unwrap();
        TreeService::new(self.root.clone()).baseline().unwrap();
    }

    fn research(&self) -> Research {
        Research::load(&self.root)
    }

    fn store(&self) -> Research {
        let mut r = self.research();
        r.apply(
            Edit::Write {
                topic: "retry backoff".into(),
                body: "the window resets after 60s idle".into(),
            },
            &self.head(),
        )
        .unwrap();
        r.save().unwrap();
        r
    }

    fn note_path(&self, topic: &str) -> PathBuf {
        self.root.join(DIR).join(format!("{topic}.md"))
    }

    fn index_text(&self) -> String {
        std::fs::read_to_string(self.root.join(DIR).join(INDEX_FILE)).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Run `body` with the console pointed at `root`. The override is
/// process-global, so this is the only place in this file that takes it.
fn with_work_root<T>(root: &Path, body: impl FnOnce() -> T) -> T {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var(ROOT_ENV, root);
    let out = body();
    std::env::remove_var(ROOT_ENV);
    out
}

/// Drive the `/research` line exactly as the console does: parse first, then
/// `apply_action` on the parsed action.
fn run_research_command(raw: &str) -> Vec<String> {
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
// 1. A fresh note is fresh; a note pinned behind the tree is stale and the
//    REASON is computable.
// ---------------------------------------------------------------------------

#[test]
fn a_note_pinned_at_head_is_fresh_and_one_pinned_behind_is_stale_with_a_reason() {
    let s = Scratch::new("fresh-stale");
    let pinned_at = s.head();
    s.store();

    let fresh = s.research().freshness("retry backoff", &pinned_at);
    assert!(
        matches!(&fresh, Freshness::Fresh { pinned } if *pinned == pinned_at.head().unwrap()),
        "a note pinned at the current commit is fresh: {fresh:?}"
    );

    s.advance();
    let moved = s.head();
    assert_ne!(
        moved.head(),
        pinned_at.head(),
        "the fixture must have moved"
    );

    let stale = s.research().freshness("retry backoff", &moved);
    assert_eq!(
        stale,
        Freshness::Stale(StaleReason::HeadMoved {
            pinned: pinned_at.head().unwrap().to_string(),
            head: moved.head().unwrap().to_string(),
        }),
        "staleness must be computed from the pin, not judged: {stale:?}"
    );
    let line = stale.reason_line();
    assert!(
        line.contains(pinned_at.head().unwrap()) && line.contains("re-verify"),
        "the reason must name the pin and what to do: {line}"
    );
}

// ---------------------------------------------------------------------------
// 2. `needs_research` is a function: answered with the note that answered it,
//    or not answered — and the not-answered case is the only state in which
//    new research is bought.
// ---------------------------------------------------------------------------

#[test]
fn needs_research_answers_from_a_fresh_note_and_says_not_answered_otherwise() {
    let s = Scratch::new("needs");
    s.store();
    let at_pin = s.head();

    let answered = s.research().needs_research("retry backoff", &at_pin);
    let note = answered
        .answering_note()
        .unwrap_or_else(|| panic!("a fresh covering note must answer: {answered:?}"));
    assert_eq!(note.topic, "retry backoff");
    assert_eq!(note.pinned, at_pin.head().unwrap());
    assert!(
        note.body.contains("60s idle"),
        "the answer carries the note's own body: {}",
        note.body
    );

    // No note for the topic at all.
    let uncovered = s.research().needs_research("token refresh", &at_pin);
    assert!(
        uncovered.answering_note().is_none(),
        "a topic with no note is not answered: {uncovered:?}"
    );
    assert!(
        uncovered.reason().contains("no note"),
        "the reason must say none exists: {}",
        uncovered.reason()
    );

    // A covering note that is STALE is not an answer either: this is the
    // state in which new research is bought.
    s.advance();
    let stale = s.research().needs_research("retry backoff", &s.head());
    assert!(
        stale.answering_note().is_none(),
        "a stale note must not answer: {stale:?}"
    );
    assert!(
        stale.reason().contains("stale"),
        "the reason must say why: {}",
        stale.reason()
    );
}

/// Requirement 4: retrieval is the only path, and it is what the retrieval
/// decision is made of. The verdict is a sum type with no third state and no
/// confidence field — a note answers, or it does not.
#[test]
fn the_verdict_is_mechanical_and_cites_the_note_that_answered() {
    let s = Scratch::new("verdict");
    s.store();
    let v = s.research().needs_research("retry backoff", &s.head());
    assert!(v.is_answered(), "{v:?}");
    let missed = s.research().needs_research("nothing here", &s.head());
    assert!(!missed.is_answered(), "{missed:?}");
    // No similarity, no confidence, no partial credit: an uncovered topic is
    // reported in the same words whatever the store happens to contain.
    assert!(!missed.reason().contains("retry"), "{}", missed.reason());
}

// ---------------------------------------------------------------------------
// 3. A note cannot leave the work root.
// ---------------------------------------------------------------------------

#[test]
fn a_topic_that_escapes_the_work_root_is_refused() {
    let s = Scratch::new("escape");
    for topic in [
        "../../etc/passwd",
        "../outside",
        "..",
        "sub/dir",
        "notes\\windows",
        ".hidden",
        "   ",
    ] {
        let mut r = s.research();
        let err = r
            .apply(
                Edit::Write {
                    topic: topic.to_string(),
                    body: "content".into(),
                },
                &s.head(),
            )
            .expect_err(&format!("{topic:?} must be refused"));
        assert!(
            err.contains("refused"),
            "the refusal must be explicit: {err}"
        );
        assert!(
            !s.dir.join("etc").exists() && !s.dir.join("outside").exists(),
            "{topic:?} wrote outside the work root"
        );
    }
    assert!(!s.root.join("passwd.md").exists(), "nothing was written");
}

// ---------------------------------------------------------------------------
// 4. Research ABOUT TESTS lives under its own path and is never served for a
//    design question (principle 9).
// ---------------------------------------------------------------------------

#[test]
fn a_test_note_is_never_returned_for_a_design_topic() {
    let s = Scratch::new("test-notes");
    let mut r = s.research();
    r.apply(
        Edit::Write {
            topic: "tests/retry backoff".into(),
            body: "the suite passes at HEAD".into(),
        },
        &s.head(),
    )
    .unwrap();
    r.apply(
        Edit::Write {
            topic: "retry backoff".into(),
            body: "the window resets after 60s idle".into(),
        },
        &s.head(),
    )
    .unwrap();
    r.save().unwrap();

    assert!(
        s.root.join(DIR).join("tests/retry backoff.md").exists(),
        "a note about tests lives under the tests path"
    );

    let store = s.research();
    let design = store.needs_research("retry backoff", &s.head());
    let body = design.answering_note().expect("the design note answers");
    assert_eq!(body.kind, Kind::Design);
    assert!(
        !body.body.contains("the suite passes"),
        "a passing suite is not evidence about the design: {}",
        body.body
    );

    // The same spelling, asked as a question ABOUT tests, reaches the test
    // note and only that one.
    let about_tests = store.needs_research("tests/retry backoff", &s.head());
    let suite = about_tests.answering_note().expect("the test note answers");
    assert_eq!(suite.kind, Kind::Tests);
    assert!(suite.path.contains("/tests/"), "{}", suite.path);

    // A design-only store must not answer a tests question either.
    let only_design = Scratch::new("test-notes-empty");
    let mut d = only_design.research();
    d.apply(
        Edit::Write {
            topic: "retry backoff".into(),
            body: "the window resets after 60s idle".into(),
        },
        &only_design.head(),
    )
    .unwrap();
    d.save().unwrap();
    assert!(d
        .needs_research("tests/retry backoff", &only_design.head())
        .answering_note()
        .is_none());
}

// ---------------------------------------------------------------------------
// 5. The index round-trips.
// ---------------------------------------------------------------------------

#[test]
fn the_index_round_trips_topics_paths_and_commits() {
    let s = Scratch::new("roundtrip");
    let head = s.head();
    let mut r = s.research();
    for topic in ["retry backoff", "tests/oracle integrity"] {
        r.apply(
            Edit::Write {
                topic: topic.into(),
                body: format!("body for {topic}"),
            },
            &head,
        )
        .unwrap();
    }
    r.notes_text = "hand-written: check the backoff before trusting it.\n".into();
    r.save().unwrap();

    let back = s.research();
    assert_eq!(back.warning, None, "{:?}", back.warning);
    assert_eq!(back.notes.len(), 2, "{:?}", back.notes);
    let a = back.note("retry backoff").expect("design note");
    let b = back.note("tests/oracle integrity").expect("test note");
    assert_eq!(a.pinned, head.head().unwrap());
    assert_eq!(a.path, format!("{DIR}/retry backoff.md"));
    assert_eq!(b.pinned, head.head().unwrap());
    assert_eq!(b.path, format!("{DIR}/tests/oracle integrity.md"));
    assert_eq!(b.kind, Kind::Tests);
    assert!(
        back.notes_text
            .contains("check the backoff before trusting it"),
        "the hand-written notes below the block were lost: {}",
        back.notes_text
    );

    // One line per note, so a hand edit is a one-line edit.
    let index = s.index_text();
    let block = index
        .split("```json")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("the index has a machine block");
    assert_eq!(
        block.lines().filter(|l| !l.trim().is_empty()).count(),
        2,
        "one line per note: {block}"
    );

    // And a save that changed nothing is byte-identical: an idempotent
    // writer cannot accumulate header text across a user's own edits.
    r.save().unwrap();
    assert_eq!(s.index_text(), index, "a second save changed the file");
}

// ---------------------------------------------------------------------------
// 6. Malformed input degrades with a reason and never takes a run down.
// ---------------------------------------------------------------------------

#[test]
fn a_malformed_index_or_note_degrades_with_a_reason() {
    let s = Scratch::new("malformed");
    let head = s.head();
    // One unreadable line in the index: the OTHER notes still load.
    let mut r = s.research();
    r.apply(
        Edit::Write {
            topic: "retry backoff".into(),
            body: "body".into(),
        },
        &head,
    )
    .unwrap();
    r.apply(
        Edit::Write {
            topic: "token refresh".into(),
            body: "body".into(),
        },
        &head,
    )
    .unwrap();
    r.save().unwrap();
    let index_path = s.root.join(DIR).join(INDEX_FILE);
    let text = std::fs::read_to_string(&index_path).unwrap();
    std::fs::write(
        &index_path,
        text.replacen("{\"topic\"", "not json at all {\"topic\"", 1),
    )
    .unwrap();

    let degraded = s.research();
    assert_eq!(
        degraded.notes.len(),
        1,
        "the good line survived: {:?}",
        degraded.notes
    );
    let warning = degraded
        .warning
        .as_deref()
        .expect("a malformed index must say so");
    assert!(warning.contains("index.md"), "{warning}");
    // A broken index cannot silently read as "you have no research", so the
    // retrieval verdict carries the reason with it.
    let v = degraded.needs_research("retry backoff", &head);
    assert!(v.answering_note().is_none(), "{v:?}");
    assert!(v.reason().contains("index.md"), "{}", v.reason());

    // An index with no block at all: empty store, warning, no panic.
    std::fs::write(&index_path, "# notes\nnothing machine-readable here\n").unwrap();
    let empty = s.research();
    assert!(empty.notes.is_empty());
    assert!(empty.warning.is_some(), "{}", empty.notes_text);

    // A note file whose header is gone is not fresh, and not a crash.
    let good = Scratch::new("malformed-note");
    good.store();
    std::fs::write(
        good.note_path("retry backoff"),
        "# retry backoff\n\nno header here\n",
    )
    .unwrap();
    let f = good.research().freshness("retry backoff", &good.head());
    assert!(
        matches!(f, Freshness::Stale(StaleReason::NoteUnreadable { .. })),
        "a note with no pin is never fresh: {f:?}"
    );
    // And a write over it is a refusal, not a silent overwrite of a file
    // whose content the harness never understood.
    let mut r = good.research();
    let err = r
        .apply(
            Edit::Verify {
                topic: "retry backoff".into(),
            },
            &good.head(),
        )
        .expect_err("a headerless note cannot be re-verified");
    assert!(err.contains("refused"), "{err}");
}

// ---------------------------------------------------------------------------
// 7. A hand-edited note survives every machine write, byte for byte.
// ---------------------------------------------------------------------------

#[test]
fn a_hand_edited_note_body_survives_a_machine_write_verbatim() {
    let s = Scratch::new("hand-edited");
    s.store();
    let mut body = std::fs::read_to_string(s.note_path("retry backoff")).unwrap();
    body.push_str("\nhand-written: measured, 60s is what the log shows.\n");
    std::fs::write(s.note_path("retry backoff"), &body).unwrap();

    // A re-verification rewrites the pin; the user's prose must be untouched.
    let mut r = s.research();
    s.advance();
    r.apply(
        Edit::Verify {
            topic: "retry backoff".into(),
        },
        &s.head(),
    )
    .unwrap();
    r.save().unwrap();

    let after = std::fs::read_to_string(s.note_path("retry backoff")).unwrap();
    assert!(
        after.contains("hand-written: measured, 60s is what the log shows."),
        "the hand edit was lost: {after}"
    );
    assert!(
        after.contains(s.head().head().unwrap()),
        "the pin was not updated: {after}"
    );
    assert!(
        s.research()
            .freshness("retry backoff", &s.head())
            .is_fresh(),
        "a re-verified note is fresh again"
    );
}

// ---------------------------------------------------------------------------
// 8. The permissive failure: every way freshness could be rubber-stamped must
//    be caught here, so "always fresh" cannot pass this file.
// ---------------------------------------------------------------------------

#[test]
fn staleness_is_never_answered_always_fresh() {
    // (a) the tree moved on a commit of its own
    let s = Scratch::new("never-fresh-moved");
    s.store();
    let pinned_of = |s: &Scratch| s.research().note("retry backoff").unwrap().pinned.clone();
    s.advance();
    let moved = s.research().freshness("retry backoff", &s.head());
    assert!(
        !moved.is_fresh(),
        "a commit on top of the pin is staleness: {moved:?}"
    );

    // (b) no commit could be read here at all — the question cannot be
    //     answered, so it must not be answered YES
    let unknown = TreeState::unknown();
    let blind = s.research().freshness("retry backoff", &unknown);
    assert!(
        !blind.is_fresh(),
        "an unreadable tree is not a fresh note: {blind:?}"
    );
    assert_eq!(
        blind,
        Freshness::Stale(StaleReason::HeadUnknown {
            pinned: pinned_of(&s)
        }),
        "{blind:?}"
    );

    // (c) the body is gone, the pin is meaningless
    let g = Scratch::new("never-fresh-missing");
    g.store();
    std::fs::remove_file(g.note_path("retry backoff")).unwrap();
    let gone = g.research().freshness("retry backoff", &g.head());
    assert!(!gone.is_fresh(), "a deleted note is not fresh: {gone:?}");

    // (d) a NON-repo work root: there is nothing to verify against
    let bare = std::env::temp_dir().join(format!("rof-research-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&bare);
    std::fs::create_dir_all(bare.join(DIR)).unwrap();
    let mut r = Research::load(&bare);
    assert!(
        r.apply(
            Edit::Write {
                topic: "x".into(),
                body: "b".into()
            },
            &TreeState::read(&bare),
        )
        .is_err(),
        "a note cannot be pinned where there is no commit to pin"
    );
    assert!(TreeState::read(&bare).head().is_none());
    let _ = std::fs::remove_dir_all(&bare);
}

// ---------------------------------------------------------------------------
// 9. Commands: closed parser, registry, help, and the console's own arm.
// ---------------------------------------------------------------------------

#[test]
fn the_research_commands_parse_and_the_console_runs_them() {
    assert_eq!(
        parse("/research"),
        Some(Action::Research(ResearchCmd::List))
    );
    assert_eq!(
        parse("/research list"),
        Some(Action::Research(ResearchCmd::List))
    );
    assert_eq!(
        parse("/research read retry backoff"),
        Some(Action::Research(ResearchCmd::Read("retry backoff".into())))
    );
    assert_eq!(
        parse("/research verify retry backoff"),
        Some(Action::Research(ResearchCmd::Verify(
            "retry backoff".into()
        )))
    );
    assert_eq!(
        parse("/research forget retry backoff"),
        Some(Action::Research(ResearchCmd::Forget(
            "retry backoff".into()
        )))
    );
    assert_eq!(
        parse("/research write retry backoff -- the window resets after 60s idle"),
        Some(Action::Research(ResearchCmd::Write {
            topic: "retry backoff".into(),
            body: "the window resets after 60s idle".into(),
        }))
    );
    // A line with no body is not a note: the harness never invents one.
    for raw in ["/research write retry backoff", "/research write -- body"] {
        assert!(
            matches!(parse(raw), Some(Action::Unknown(_))),
            "{raw} must be refused by the parser"
        );
    }
    for raw in ["/research nope", "/research read"] {
        assert!(
            matches!(parse(raw), Some(Action::Unknown(_))),
            "{raw} must be refused by the parser"
        );
    }
    assert!(
        rof::tui::cmd::COMMANDS.contains(&"research"),
        "a command nobody completes is a command nobody finds"
    );
    assert!(rof::tui::cmd::help_text().contains("/research"));

    // The console's own arm, against a real work root.
    let s = Scratch::new("console");
    let root = s.root.clone();
    with_work_root(&root, || {
        let tree = s.head();

        let wrote = run_research_command(
            "/research write retry backoff -- the window resets after 60s idle",
        );
        assert!(
            wrote.iter().any(|l| l.contains("retry backoff")),
            "the write is not reported: {wrote:?}"
        );

        let listed = run_research_command("/research list");
        assert!(
            listed.iter().any(|l| l.contains("retry backoff")
                && l.contains(tree.head().unwrap())
                && l.contains("fresh")),
            "list must show topic, pinned commit and freshness: {listed:?}"
        );

        let read = run_research_command("/research read retry backoff");
        assert!(
            read.iter().any(|l| l.contains("60s idle")),
            "read must show the body: {read:?}"
        );

        // The tree moves: the same read now reports the note as stale and
        // refuses to serve it as an answer.
        s.advance();
        let stale_list = run_research_command("/research list");
        assert!(
            stale_list.iter().any(|l| l.contains("stale")),
            "a note behind the tree must read as stale: {stale_list:?}"
        );
        let stale_read = run_research_command("/research read retry backoff");
        assert!(
            !stale_read.iter().any(|l| l.contains("60s idle")),
            "a stale note must not be served as an answer: {stale_read:?}"
        );

        // verify re-pins, and the note is servable again — a
        // re-verification, not a new claim.
        run_research_command("/research verify retry backoff");
        let again = run_research_command("/research read retry backoff");
        assert!(
            again.iter().any(|l| l.contains("60s idle")),
            "a verified note answers again: {again:?}"
        );

        run_research_command("/research forget retry backoff");
        assert!(
            !s.note_path("retry backoff").exists(),
            "forget must remove the body file"
        );
        let gone = run_research_command("/research read retry backoff");
        assert!(
            gone.iter().any(|l| l.contains("no note")),
            "a forgotten topic is un-answered: {gone:?}"
        );
    });
}

// ---------------------------------------------------------------------------
// 10. The write gate: a note is a working-tree change, and a note ABOUT TESTS
//     lands on a path the oracle reads as test-shaped. Both are asserted here
//     rather than only described.
// ---------------------------------------------------------------------------

#[test]
fn a_note_is_a_working_tree_change_and_a_test_note_looks_test_shaped() {
    let s = Scratch::new("gate");
    let mut r = s.research();
    r.apply(
        Edit::Write {
            topic: "tests/oracle integrity".into(),
            body: "the suite passes".into(),
        },
        &s.head(),
    )
    .unwrap();
    r.save().unwrap();

    let service = TreeService::new(s.root.clone());
    // A folder that is entirely NEW is ONE untracked name in git's porcelain
    // output, not one per note. Recorded here so nobody later claims the
    // write gate sees each note.
    assert_eq!(
        service.diff().unwrap().names,
        vec![".rof/".to_string()],
        "a new folder is one name in the change set"
    );

    service.baseline().unwrap();
    s.store();
    let diff = service.diff().unwrap();
    let index_name = format!("{DIR}/index.md");
    let note_name = format!("{DIR}/retry backoff.md");
    assert!(
        diff.names.contains(&index_name) && diff.names.contains(&note_name),
        "a note is a working-tree change: {:?}",
        diff.names
    );
    assert!(
        diff.untracked.contains(&note_name),
        "a new note is untracked, which is what the oracle filter keys on: {:?}",
        diff.untracked
    );
    // The one that matters: a path under a `tests` segment is test-shaped to
    // `is_test_shaped`, so a note about a suite would LOOK like tampering to
    // the oracle once committed and re-verified. The fix is the `.rof/`
    // exclusion in `protected_oracle`: harness bookkeeping is not the suite.
    // Asserted as the FIXED behaviour, because an earlier version of this
    // test asserted the hazard and the hazard was real.
    let suite = format!("{DIR}/tests/oracle integrity.md");
    let suite_body = std::fs::read_to_string(s.root.join(&suite)).unwrap();
    assert!(is_test_shaped(&suite), "{suite} must be test-shaped");
    std::fs::write(s.root.join(&suite), "re-verified\n").unwrap();
    let d2 = service.diff().unwrap();
    assert!(
        d2.names.contains(&suite) && !d2.untracked.contains(&suite),
        "the fixture must produce a TRACKED change to the test-shaped path: {:?} / {:?}",
        d2.names,
        d2.untracked
    );
    assert!(
        d2.protected_oracle().is_empty(),
        "a re-verified research note must NOT read as oracle tampering: {:?}",
        d2.protected_oracle()
    );
    // The exclusion is scoped to `.rof/`: a real suite edit is still
    // protected, which is the whole point of the gate. It has to be a
    // TRACKED change — a brand-new test file is legitimately allowed — so
    // commit the fixture and then dirty the suite.
    std::fs::create_dir_all(s.root.join("tests")).unwrap();
    std::fs::write(s.root.join("tests/real_suite.rs"), "original\n").unwrap();
    service.baseline().unwrap();
    std::fs::write(s.root.join("tests/real_suite.rs"), "tampered\n").unwrap();
    let d_real = service.diff().unwrap();
    assert!(
        d_real
            .protected_oracle()
            .iter()
            .any(|p| p == "tests/real_suite.rs"),
        "excluding .rof/ must not unprotect the actual suite: {:?}",
        d_real.protected_oracle()
    );
    // Restore the suite's content rather than removing the file: it is
    // committed now, so deleting it would be a tracked DELETION and still a
    // protected change.
    std::fs::write(s.root.join("tests/real_suite.rs"), "original\n").unwrap();
    // A DESIGN note never does either, which is why the two kinds are kept
    // apart — with the suite note put back byte for byte, so only the design
    // note is dirty in the change set.
    std::fs::write(s.root.join(&suite), &suite_body).unwrap();
    std::fs::write(s.note_path("retry backoff"), "re-verified\n").unwrap();
    let d3 = service.diff().unwrap();
    assert!(
        d3.protected_oracle().is_empty(),
        "a design note must not trip the oracle: {:?}",
        d3.protected_oracle()
    );
}

// ---------------------------------------------------------------------------
// 11. The layout is deterministic: a note is addressed by its path, and the
//     path is a function of the topic alone.
// ---------------------------------------------------------------------------

#[test]
fn addressing_is_by_path_and_is_a_function_of_the_topic() {
    let s = Scratch::new("addressing");
    s.store();
    let path = research::note_rel_path(Kind::Design, "retry backoff").unwrap();
    assert_eq!(path, format!("{DIR}/retry backoff.md"));
    assert!(
        s.root.join(&path).exists(),
        "{path} must be where the note is"
    );
    let suite = research::note_rel_path(Kind::Tests, "oracle integrity").unwrap();
    assert_eq!(suite, format!("{DIR}/tests/oracle integrity.md"));
    // A relative path with no `..` and no root, or it is refused.
    for (kind, topic) in [
        (Kind::Design, "../x"),
        (Kind::Tests, "../../etc/passwd"),
        (Kind::Design, ""),
    ] {
        assert!(
            research::note_rel_path(kind, topic).is_err(),
            "{topic:?} must not be addressable"
        );
    }
    assert!(!Path::new(&path).is_absolute());
}
