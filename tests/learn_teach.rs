// Learn mode, slice B: teaching (`introduces` -> the goal's one lesson).
//
// The mechanism, not the console: this slice makes the harness NAME a
// concept, GATE it by set membership, compose the lesson out of the
// model's own `because`, and record the state transition. Nothing here
// renders a lesson to a terminal — that is the next slice, and until it
// lands this file is the only place the lesson is visible.
//
// Hermetic: `ROF_PROFILE` points at a scratch file, the workdir is a temp
// tree, the LLM is a fake. No PTY, no sleeps, no network. `ROF_PROFILE`
// is process-global, so the env-touching tests share one lock, the same
// idiom `tests/learn_profile.rs` uses.

use async_trait::async_trait;
use rof::agents::teach::{self, Gated, Introduced};
use rof::config::{AppConfig, PermissionPolicy};
use rof::context::profile::{self, State};
use rof::engine::{Orchestrator, Session};
use rof::llm::{ContextService, ExecutorService, LlmClient, LlmError, LlmReq, LlmResp};
use rof::obs::{TraceEvent, TraceSink};
use rof::tools::{FsListTool, FsPatchTool, FsReadTool, FsWriteTool, ProcRunTool, ToolRegistry};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

static ENV_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// A canned model. Every knob here is a teaching-path input: what the
/// implementer NAMES, and whether the reviewer passes first time.
struct Fake {
    /// The `introduces` value the implementer emits, verbatim. A string,
    /// so a test can hand it a deliberately malformed value.
    introduces: String,
    /// Fail the first reviewer verdict, so a second round runs. The
    /// multi-round delivery test needs this: the lesson is emitted in
    /// round 1 and the result carries the LAST round's artifact.
    fail_first_review: bool,
    reviews: AtomicUsize,
}

impl Fake {
    fn introducing(introduces: &str) -> Arc<Fake> {
        Arc::new(Fake {
            introduces: introduces.to_string(),
            fail_first_review: false,
            reviews: AtomicUsize::new(0),
        })
    }

    /// Two rounds: round 1 passes the review, round 2 does not, so the
    /// loop runs twice with the same goal.
    fn two_rounds(introduces: &str) -> Arc<Fake> {
        Arc::new(Fake {
            introduces: introduces.to_string(),
            fail_first_review: true,
            reviews: AtomicUsize::new(0),
        })
    }

    fn resp(text: String) -> Result<LlmResp, LlmError> {
        Ok(LlmResp {
            text,
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 1,
            cost_usd: None,
            cached_input_tokens: 0,
            attempts: 1,
        })
    }
}

#[async_trait]
impl LlmClient for Fake {
    async fn complete(&self, _model: &str, req: LlmReq) -> Result<LlmResp, LlmError> {
        if req.system.contains("Compress the input") {
            return Fake::resp("condensed".to_string());
        }
        if req.system.contains("reviewer") {
            let n = self.reviews.fetch_add(1, Ordering::SeqCst);
            if self.fail_first_review && n == 0 {
                return Fake::resp("{\"pass\": false, \"feedback\": \"not yet\"}".to_string());
            }
            return Fake::resp("{\"pass\": true, \"feedback\": \"looks good\"}".to_string());
        }
        if req.system.contains("implementer") || req.system.contains("direct coding agent") {
            return Fake::resp(format!(
                "{{\"artifact\": \"did the work\", \"notes\": \"ok\", \"introduces\": {}}}",
                self.introduces
            ));
        }
        Fake::resp("{\"artifact\": \"ok\"}".to_string())
    }
}

/// One scratch profile file plus one scratch workdir, removed on drop.
/// The lock is held for the whole test because `ROF_PROFILE` is
/// process-global and two tests sharing it would read each other's store.
struct Scratch {
    dir: PathBuf,
    profile: PathBuf,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "rof-teach-{tag}-{}-{:?}",
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

    /// Point the store somewhere that cannot be written: a path whose
    /// parent is a regular FILE, so `create_dir_all` fails for a reason
    /// no privilege level can grant its way around.
    fn make_unwritable(&self) {
        let blocker = self.dir.join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        std::env::set_var(profile::PATH_ENV, blocker.join("PROFILE.md"));
    }

    fn read_profile(&self) -> rof::context::profile::Profile {
        profile::load()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::env::remove_var(profile::PATH_ENV);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn harness(
    client: Arc<Fake>,
    scratch: &Scratch,
    max_rounds: u32,
) -> (Orchestrator, ToolRegistry, Arc<TraceSink>) {
    let root = scratch.workdir();
    std::fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();
    // No allowed commands: the implementer's post-write test runner stays
    // silent, so nothing here depends on a Python install.
    let policy = PermissionPolicy {
        allowed_dirs: vec![root.clone()],
        allowed_commands: Vec::new(),
        ..Default::default()
    };
    let mut reg = ToolRegistry::new(policy);
    reg.register(FsListTool::new(root.clone()));
    reg.register(FsReadTool::new(root.clone()));
    reg.register(FsPatchTool::new(root.clone()));
    reg.register(FsWriteTool::new(root.clone()));
    reg.register(ProcRunTool::new(root.clone(), Vec::new()));

    let cfg = AppConfig {
        max_review_rounds: max_rounds,
        execution: "pipeline".to_string(),
        ..Default::default()
    };
    let trace = Arc::new(TraceSink::new());
    let context = ContextService::new(client.clone(), "fake-ctx".to_string());
    let executor = ExecutorService::new(client.clone(), "fake-exec".to_string(), None);
    let verify = ExecutorService::new(client, "fake-verify".to_string(), None);
    (
        Orchestrator::new(cfg, trace.clone(), context, executor, verify),
        reg,
        trace,
    )
}

/// A task-shaped goal, so the decomposition gate declines and the run
/// never buys a plan: this file is about teaching, not about meta.
const GOAL: &str = "fix src/a.rs so the parser accepts an empty line";

async fn run_goal(
    client: Arc<Fake>,
    scratch: &Scratch,
    max_rounds: u32,
) -> (serde_json::Value, Vec<TraceEvent>) {
    let (orch, reg, trace) = harness(client, scratch, max_rounds);
    let root = scratch.workdir();
    let out = orch
        .run_loop(
            &Session::new(GOAL.to_string()).expecting_writes(false),
            &reg,
            &root,
        )
        .await;
    (out, trace.events())
}

fn one(concept: &str, because: &str) -> String {
    serde_json::json!([{ "concept": concept, "because": because }]).to_string()
}

// ---------------------------------------------------------------------------
// 1. THE GATE ITSELF: set membership, not judgement.
// ---------------------------------------------------------------------------

/// The gate is a lookup in `assumed_known` and nothing else. Proven
/// here as a pure function over the store, before any run: a concept in
/// either derived-known state is excluded, a concept in the
/// assumed-unknown state is taught, and a concept the store has NEVER
/// heard of is taught too — the empty-start case, where on day one every
/// concept is new and the silence is only the default because nothing is
/// introduced twice.
#[test]
fn the_gate_is_membership_in_assumed_known_and_nothing_else() {
    let mut p = rof::context::profile::Profile::default();
    for c in ["explained-one", "understood-one", "unknown-one"] {
        p.apply(rof::context::profile::Edit::Add {
            concept: c.to_string(),
            scope: rof::context::profile::Scope::Global,
            evidence: "cited".into(),
        })
        .unwrap();
    }
    p.apply(rof::context::profile::Edit::AssumeKnown(
        "explained-one".into(),
    ))
    .unwrap();
    p.apply(rof::context::profile::Edit::AssumeUnderstood(
        "understood-one".into(),
    ))
    .unwrap();

    let introduces = |c: &str| vec![Introduced::new(c, "because")];

    // Assumed known -> excluded SILENTLY: no lesson, and nothing named
    // about it either, which is what "silently" has to mean for the
    // anti-nag property to be a property and not a mood.
    for known in ["explained-one", "understood-one"] {
        assert!(
            matches!(teach::gate(&p, &introduces(known)), Gated::Silent),
            "{known} is assumed_known and must produce no lesson"
        );
    }
    // Assumed unknown -> taught.
    assert!(
        matches!(
            teach::gate(&p, &introduces("unknown-one")),
            Gated::Lesson { .. }
        ),
        "a not_explained concept is exactly what this is for"
    );
    // Never heard of it -> taught. The store starts empty, so this is the
    // day-one path, not an edge case.
    assert!(
        matches!(
            teach::gate(&p, &introduces("never-heard-of")),
            Gated::Lesson { .. }
        ),
        "an unknown concept with no entry at all is treated as not_explained"
    );
    // Nothing introduced at all -> silent, and cheap.
    assert!(matches!(teach::gate(&p, &[]), Gated::Silent));
}

/// A store that cannot be READ is a store the gate must not consult:
/// `load` degrades a malformed file to an EMPTY one, so gating on it
/// would teach everything the user has already been taught. The read
/// failure has to be visible to the teaching path, separately.
#[test]
fn a_malformed_store_reads_as_empty_but_still_says_why() {
    let s = Scratch::new("gate-malformed");
    std::fs::write(&s.profile, "```json\n{ not json at all\n```\n").unwrap();
    let p = profile::load();
    assert!(p.entries.is_empty(), "a bad block yields no entries");
    assert!(
        p.warning.is_some(),
        "a degraded store must be distinguishable from an empty one: {p:?}"
    );
    // Which is exactly why the run path checks the warning before gating.
    assert!(
        matches!(
            teach::gate(&p, &[Introduced::new("x", "b")]),
            Gated::Lesson { .. }
        ),
        "the pure gate cannot know — the caller must"
    );
    assert_eq!(teach::unusable_store(&p), Some(p.warning.clone().unwrap()));
}

/// The deterministic pick when a goal introduces SEVERAL concepts: the
/// FIRST in the model's declared order that the gate admits. Named here
/// because "pick one" is only a rule if it is the same rule every time.
#[test]
fn several_concepts_yield_the_first_admitted_one_in_declared_order() {
    let p = rof::context::profile::Profile::default();
    let many = vec![
        Introduced::new("first", "b1"),
        Introduced::new("second", "b2"),
        Introduced::new("third", "b3"),
    ];
    match teach::gate(&p, &many) {
        Gated::Lesson {
            concept, dropped, ..
        } => {
            assert_eq!(concept, "first", "declared order decides");
            assert_eq!(dropped, vec!["second".to_string(), "third".to_string()]);
        }
        other => panic!("expected a lesson, got {other:?}"),
    }
    // An already-known concept does not consume the pick: the first
    // ADMITTED concept is chosen, not the first declared one.
    let mut p2 = p.clone();
    p2.apply(rof::context::profile::Edit::Add {
        concept: "first".into(),
        scope: rof::context::profile::Scope::Global,
        evidence: "cited".into(),
    })
    .unwrap();
    p2.apply(rof::context::profile::Edit::AssumeKnown("first".into()))
        .unwrap();
    match teach::gate(&p2, &many) {
        Gated::Lesson { concept, .. } => assert_eq!(concept, "second"),
        other => panic!("expected a lesson, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 2. THE PARSE: malformed `introduces` must not break a run.
// ---------------------------------------------------------------------------

/// The field is optional and the model is not to be trusted with it.
/// Every shape a model can plausibly emit is either a concept with a
/// reason or nothing at all — never an error, because a run that dies on
/// a malformed optional field is a worse harness than one that teaches
/// less.
#[test]
fn a_malformed_introduces_yields_nothing_rather_than_an_error() {
    for bad in [
        "",
        "null",
        "3",
        "\"retry backoff\"",
        "{}",
        "[]",
        "[1, 2, 3]",
        "[{}]",
        "[{\"concept\": \"\"}]",
        "[{\"concept\": \"   \"}]",
        "[{\"concept\": 7, \"because\": \"x\"}]",
        // A concept with no reason is not an introduction: the store
        // refuses an entry with no evidence, and the harness does not
        // invent prose to stand in for one.
        "[{\"concept\": \"retry backoff\"}]",
        "[{\"concept\": \"a\", \"because\": \"\"}]",
        "[{\"concept\": \"a\", \"because\": 9}]",
    ] {
        let shaped = if bad.is_empty() {
            "{\"introduces\": null}".to_string()
        } else {
            // Wrap in the OBJECT the parser actually reads. Passing the bare
            // value made every case here pass vacuously — `.get("introduces")`
            // on an array is always None — and the well-formed case failed.
            format!("{{\"introduces\": {bad}}}")
        };
        let v: serde_json::Value = serde_json::from_str(&shaped).unwrap();
        assert!(
            teach::parse_introduces(&v).is_empty(),
            "{bad} must parse to no concepts, not to an error"
        );
    }
    // And the well-formed shape parses.
    let good: serde_json::Value = serde_json::from_str(&format!(
        "{{\"introduces\": {}}}",
        one("retry backoff", "b")
    ))
    .unwrap();
    let parsed = teach::parse_introduces(&good);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].concept, "retry backoff");
    assert_eq!(parsed[0].because, "b");
}

/// Is this additive? The implementer's output is a `serde_json::Value`
/// throughout — `parse_lenient` produces one and every field is read with
/// `.get(..)` — so an unrecognised key is inert, never a parse error, and
/// is preserved verbatim under `result`. Asserted on the source, because
/// the answer decides whether a new field can be added at all.
#[test]
fn unknown_artifact_fields_are_inert_because_nothing_denies_them() {
    let src = include_str!("../src/agents/implementer.rs");
    assert!(
        !src.contains("deny_unknown_fields"),
        "the implementer must keep reading a Value, not a closed struct"
    );
    let lenient = include_str!("../src/llm/mod.rs");
    assert!(
        lenient.contains("pub fn parse_lenient"),
        "the model response is parsed leniently into a Value"
    );
    // And an unrecognised sibling key rides along without disturbing the
    // recognised one.
    let v: serde_json::Value =
        serde_json::from_str(r#"{"introduces":[{"concept":"a","because":"b"}],"who_knows":1}"#)
            .unwrap();
    assert_eq!(teach::parse_introduces(&v).len(), 1);
    assert_eq!(v["who_knows"], 1, "the unknown key is untouched");
}

// ---------------------------------------------------------------------------
// 3. THE RUN: the lesson reaches the RESULT, once, and only for an
//    unknown concept.
// ---------------------------------------------------------------------------

/// The red test for the whole feature: a goal that introduces an unknown
/// concept ends with a lesson on the run's RESULT, the concept recorded
/// `explained`, and nothing anywhere claiming the user understands it.
#[tokio::test]
async fn a_goal_introducing_an_unknown_concept_yields_a_lesson_and_records_it_explained() {
    let s = Scratch::new("unknown");
    let (out, _events) = run_goal(
        Fake::introducing(&one(
            "retry backoff",
            "the executor sleeps 200ms, doubles it, and gives up after 5 tries",
        )),
        &s,
        2,
    )
    .await;

    let lesson = &out["lesson"];
    assert!(
        !lesson.is_null(),
        "the run's RESULT must carry the lesson: {out}"
    );
    assert_eq!(lesson["concept"], "retry backoff");
    assert_eq!(
        lesson["because"], "the executor sleeps 200ms, doubles it, and gives up after 5 tries",
        "the harness does not rewrite the model's reason"
    );
    let text = lesson["text"].as_str().unwrap_or_default();
    assert!(text.contains("retry backoff"), "lesson text: {text}");
    assert!(
        text.contains("200ms"),
        "the lesson is the model's own words, not harness prose: {text}"
    );

    // Recorded, and recorded as an assumption — not a claim about a person.
    let p = s.read_profile();
    let entry = p
        .entries
        .iter()
        .find(|e| e.concept == "retry backoff")
        .expect("the concept was recorded");
    assert_eq!(entry.state, State::Explained);
    assert_eq!(
        entry.evidence, lesson["because"],
        "the persisted evidence is the model's because, not a synthesised one"
    );
}

/// THE ANTI-NAG PROPERTY, on a second run of the same goal. The gate is
/// set membership, so this is a lookup and not a judgement — and the
/// store is what makes it true across processes.
#[tokio::test]
async fn the_same_concept_introduced_again_yields_no_lesson() {
    let s = Scratch::new("antinag");
    let fake = Fake::introducing(&one("retry backoff", "the executor doubles the sleep"));
    let (first, _) = run_goal(fake.clone(), &s, 2).await;
    assert!(!first["lesson"].is_null(), "the first run teaches");

    let (second, _) = run_goal(fake, &s, 2).await;
    assert!(
        second["lesson"].is_null(),
        "a second run of a goal naming the same concept must say NOTHING: {second}"
    );
    // And saying nothing means nothing moved either.
    let p = s.read_profile();
    assert_eq!(p.entries.len(), 1, "no duplicate entry");
    assert_eq!(p.entries[0].state, State::Explained);
}

/// `understood` is the user's alone (spec §2). A concept already there
/// yields no lesson, and the harness leaves it exactly where the user's
/// own command put it.
#[tokio::test]
async fn a_concept_the_user_already_understands_yields_no_lesson_and_is_left_understood() {
    let s = Scratch::new("understood");
    let mut p = rof::context::profile::Profile::default();
    p.apply(rof::context::profile::Edit::Add {
        concept: "auth token layout".into(),
        scope: rof::context::profile::Scope::Global,
        evidence: "cited".into(),
    })
    .unwrap();
    // The ONLY route to `understood`, and it is a user command.
    p.apply(rof::context::profile::Edit::AssumeUnderstood(
        "auth token layout".into(),
    ))
    .unwrap();
    profile::save(&p).unwrap();

    let (out, _events) = run_goal(Fake::introducing(&one("auth token layout", "b")), &s, 2).await;
    assert!(out["lesson"].is_null(), "an understood concept: {out}");
    assert_eq!(
        s.read_profile().entries[0].state,
        State::Understood,
        "the harness must not move a confirmation"
    );
}

/// One concept per goal, and the ones that lost the pick are SAID to
/// have lost it. A lesson the user cannot act on is one they learn to
/// dismiss; a lesson that hides that three concepts are queued is a lie
/// by omission.
#[tokio::test]
async fn several_concepts_in_one_goal_yield_one_lesson_and_state_the_drop() {
    let s = Scratch::new("several");
    let introduces = serde_json::json!([
        {"concept": "retry backoff", "because": "the executor doubles the sleep"},
        {"concept": "auth token layout", "because": "the token is read in one place"},
        {"concept": "tree rollback", "because": "a failed attempt is rolled back to the baseline"},
    ])
    .to_string();
    let (out, _events) = run_goal(Fake::introducing(&introduces), &s, 2).await;

    let lesson = &out["lesson"];
    assert_eq!(lesson["concept"], "retry backoff", "first declared wins");
    let text = lesson["text"].as_str().unwrap_or_default();
    for dropped in ["auth token layout", "tree rollback"] {
        assert!(
            text.contains(dropped),
            "the drop must be stated, not silent: {text}"
        );
    }
    // Only the taught one is recorded. The dropped ones are still new:
    // recording them would assert we explained something the user never
    // saw, and the anti-nag gate would then hide them forever.
    let p = s.read_profile();
    assert_eq!(p.entries.len(), 1, "one concept per goal: {:?}", p.entries);
    assert_eq!(p.entries[0].concept, "retry backoff");
    assert_eq!(p.entries[0].state, State::Explained);
}

/// A malformed optional field degrades the TEACHING, never the run. The
/// goal still passes, the store is untouched, and no lesson appears.
#[tokio::test]
async fn a_malformed_introduces_does_not_fail_the_run() {
    let s = Scratch::new("malformed-field");
    for bad in ["3", "\"a string\"", "[1,2,3]", "[{\"concept\":\"\"}]", "[]"] {
        let (out, _events) = run_goal(Fake::introducing(bad), &s, 2).await;
        assert_eq!(out["passed"], true, "the run must not fail on {bad}");
        assert!(
            out["lesson"].is_null(),
            "a malformed introduces teaches nothing: {out}"
        );
    }
    assert!(
        s.read_profile().entries.is_empty(),
        "nothing was recorded from a malformed field"
    );
}

/// The lesson belongs to the RESULT, at the end of a goal. It must not
/// appear as a per-round trace event: the terminal is a work surface,
/// and an explanation between rounds is how you train someone to ignore
/// explanations. Proven on a TWO-round run, where a mid-round emission
/// would be visible in the middle of the event stream.
#[tokio::test]
async fn the_lesson_is_on_the_result_and_not_interleaved_into_the_round_events() {
    let s = Scratch::new("not-mid-round");
    let (out, events) = run_goal(
        Fake::two_rounds(&one("retry backoff", "the executor doubles the sleep")),
        &s,
        3,
    )
    .await;
    assert_eq!(out["rounds"], 2, "this run really did take two rounds");
    assert!(
        !out["lesson"].is_null(),
        "the lesson must SURVIVE to the result: {out}"
    );
    // Nothing in the durable event stream carries the lesson text. The
    // sink is the live view's source, so this is the property.
    for ev in &events {
        let rendered = serde_json::to_string(ev).unwrap_or_default();
        assert!(
            !rendered.contains("the executor doubles the sleep"),
            "a lesson leaked into the per-round event stream: {rendered}"
        );
    }
}

/// THE MULTI-ROUND DELIVERY REGRESSION, stated separately because it is
/// the failure mode the whole hoist exists for. The gate records the
/// concept on the round that teaches it, so every LATER round excludes
/// it silently — and the result carries the LAST round's artifact. A
/// lesson attached to the artifact alone is therefore dropped on the
/// default two-round configuration, and the user is told nothing.
#[tokio::test]
async fn a_lesson_taught_in_round_one_survives_to_a_result_built_from_the_last_round() {
    let s = Scratch::new("multi-round");
    let (out, _events) = run_goal(
        Fake::two_rounds(&one("retry backoff", "the executor doubles the sleep")),
        &s,
        3,
    )
    .await;
    assert_eq!(out["rounds"], 2, "the fixture must really run two rounds");
    assert_eq!(
        out["lesson"]["concept"], "retry backoff",
        "the first lesson of the goal must be the one the result carries: {out}"
    );
}

/// A concept that LOSES the one-per-goal pick stays `not_explained`, and
/// is taught on a later goal. The state write is tied to actually
/// EMITTING the lesson: recording a concept the user was never shown
/// would make the store assert we explained it, and the anti-nag gate
/// would then exclude it forever.
#[tokio::test]
async fn a_concept_suppressed_by_the_one_per_goal_pick_is_taught_on_a_later_goal() {
    let s = Scratch::new("suppressed");
    let both = serde_json::json!([
        {"concept": "retry backoff", "because": "the executor doubles the sleep"},
        {"concept": "auth token layout", "because": "the token is read in one place"},
    ])
    .to_string();
    let (out, _events) = run_goal(Fake::introducing(&both), &s, 2).await;
    assert_eq!(out["lesson"]["concept"], "retry backoff");
    assert_eq!(
        s.read_profile().entries.len(),
        1,
        "the suppressed concept must not be recorded as explained"
    );

    // A later goal that names only the suppressed concept teaches it.
    let (later, _events) = run_goal(
        Fake::introducing(&one("auth token layout", "the token is read in one place")),
        &s,
        2,
    )
    .await;
    assert_eq!(
        later["lesson"]["concept"], "auth token layout",
        "a suppressed concept is taught on a future goal: {later}"
    );
    let p = s.read_profile();
    assert_eq!(p.entries.len(), 2);
    assert_eq!(
        p.entries
            .iter()
            .find(|e| e.concept == "auth token layout")
            .unwrap()
            .state,
        State::Explained
    );
}

// ---------------------------------------------------------------------------
// 4. DEGRADE, NEVER FAIL.
// ---------------------------------------------------------------------------

/// A store that cannot be WRITTEN must not take a run down. The lesson
/// is skipped, the reason is in the trace, and the goal still finishes.
#[tokio::test]
async fn an_unwritable_store_completes_the_run_with_the_reason_in_the_trace_and_no_lesson() {
    let s = Scratch::new("unwritable");
    s.make_unwritable();
    let (orch, reg, trace) = harness(
        Fake::introducing(&one("retry backoff", "the executor doubles the sleep")),
        &s,
        2,
    );
    let root = s.workdir();
    let out = orch
        .run_loop(
            &Session::new(GOAL.to_string()).expecting_writes(false),
            &reg,
            &root,
        )
        .await;
    assert_eq!(out["passed"], true, "a store is not worth a failed goal");
    assert!(
        out["lesson"].is_null(),
        "a lesson we could not record must not be shown: {out}"
    );
    let events = trace.events();
    let skips: Vec<&TraceEvent> = events
        .iter()
        .filter(|e| matches!(e, TraceEvent::LessonSkipped { .. }))
        .collect();
    assert_eq!(
        skips.len(),
        1,
        "the degraded store must be surfaced once, not silently swallowed: {events:?}"
    );
    match skips[0] {
        TraceEvent::LessonSkipped { concept, reason } => {
            assert_eq!(concept, "retry backoff");
            assert!(!reason.is_empty(), "the reason must say something");
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// The READ side degrades the same way. A malformed store loads as an
/// EMPTY one, and an empty store is the day-one case that teaches
/// everything — so the run must check the warning before gating, or a
/// hand-edit typo re-teaches the user's whole history.
#[tokio::test]
async fn a_malformed_store_skips_the_lesson_and_says_why() {
    let s = Scratch::new("malformed-store");
    std::fs::write(&s.profile, "```json\n{ not json at all\n```\n").unwrap();
    let (out, events) = run_goal(
        Fake::introducing(&one("retry backoff", "the executor doubles the sleep")),
        &s,
        2,
    )
    .await;
    assert_eq!(out["passed"], true);
    assert!(out["lesson"].is_null(), "{out}");
    let skip = events
        .iter()
        .find_map(|e| match e {
            TraceEvent::LessonSkipped { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .expect("the degraded read must be surfaced");
    assert!(
        skip.contains("PROFILE") || skip.contains("json"),
        "the reason must name the file it could not read: {skip}"
    );
    // And the malformed file is left alone: a run never rewrites a store
    // it could not understand.
    assert_eq!(
        std::fs::read_to_string(&s.profile).unwrap(),
        "```json\n{ not json at all\n```\n",
        "the user's file must survive a run untouched"
    );
}

// ---------------------------------------------------------------------------
// 5. NEVER `understood`.
// ---------------------------------------------------------------------------

/// The slice-level structural claim: a run that introduces concepts ends
/// at `explained`. Nothing here infers competence, so there is no path
/// by which a run could produce a confirmation about a person.
#[tokio::test]
async fn a_run_that_introduces_concepts_leaves_the_state_at_explained_and_never_understood() {
    let s = Scratch::new("never-understood");
    let (out, _events) = run_goal(
        Fake::introducing(
            &serde_json::json!([
                {"concept": "a concept", "because": "because a"},
                {"concept": "another concept", "because": "because b"},
            ])
            .to_string(),
        ),
        &s,
        2,
    )
    .await;
    assert!(!out["lesson"].is_null(), "{out}");
    let p = s.read_profile();
    assert!(!p.entries.is_empty());
    for e in &p.entries {
        assert_eq!(
            e.state,
            State::Explained,
            "a run may only record an explanation: {:?}",
            e
        );
        assert_ne!(e.state, State::Understood);
    }
    // And the classification the anti-nag gate reads agrees.
    assert_eq!(p.assumed_known().len(), p.entries.len());
    assert!(p.assumed_unknown().is_empty());
}

/// The teaching path is the SECOND writer in the crate, so the one-entry
/// property from slice A has to be checked against it: the teaching step
/// records through `Profile::apply` like every other writer, and reaches
/// no other `&mut Profile` method.
#[test]
fn the_teaching_path_records_through_the_store_s_own_write_path() {
    let src = include_str!("../src/agents/teach.rs");
    assert!(
        src.contains(".apply("),
        "the teaching path must go through the one write path"
    );
    assert!(
        !src.contains("State::Understood"),
        "the teaching path may not name the user's state at all"
    );
    assert!(
        !src.contains("AssumeUnderstood"),
        "only /profile assume-understood may reach understood"
    );
}
