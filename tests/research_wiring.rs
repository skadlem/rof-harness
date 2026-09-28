//! §7's second half, wired: retrieval BEFORE research, in a real run
//! (design report §7, build item 5).
//!
//! The store landed first and nothing read it. This file is the run that does,
//! and the claims under test are the ones an implementation that consults the
//! folder *after* the spend, or that buys research unconditionally, or that
//! lets a note crowd out `AGENTS.md`, would quietly break:
//!
//! - a FRESH note reaches the implementer's FIRST prompt and costs no call;
//! - `NotAnswered` (absent OR stale) buys exactly one bounded call, and the
//!   note that call produces is WRITTEN BACK, so the next run retrieves it;
//! - a model that produces nothing usable writes nothing and says why;
//! - a moved pin re-buys instead of serving the stale body;
//! - the injected block is bounded and rides AFTER the project conventions;
//! - the knob off means no lookup, no call, and a byte-identical run;
//! - a research note alone cannot satisfy `expect_writes`.
//!
//! Hermetic: every fixture is a real temp git work root, the model is a canned
//! fake, and there is no PTY, no sleep and no network.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rof::agents::research::{self, Consulted};
use rof::config::{AppConfig, PermissionPolicy};
use rof::context::research as store;
use rof::context::research::{Edit, Research, TreeState, DIR};
use rof::engine::{Orchestrator, Session};
use rof::llm::{ContextService, ExecutorService, LlmClient, LlmError, LlmReq, LlmResp};
use rof::obs::{TraceEvent, TraceSink};
use rof::tools::{FsListTool, FsPatchTool, FsReadTool, ProcRunTool, ToolRegistry};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// A task-shaped goal, so the decomposition gate declines and this file never
/// pays for a plan: it is about research, not about the meta layer.
const GOAL: &str = "fix src/a.rs so the parser accepts an empty line";

/// One real git work root, removed on drop. The commit it carries is the one a
/// note pins, so freshness is decided against a real tree.
struct Scratch {
    dir: PathBuf,
    root: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "rof-research-wire-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("work");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn parse() {}\n").unwrap();
        let tree = rof::engine::tree::TreeService::new(root.clone());
        tree.ensure().unwrap();
        Scratch { dir, root }
    }

    fn head(&self) -> TreeState {
        let state = TreeState::read(&self.root);
        assert!(state.head().is_some(), "the fixture must have a HEAD");
        state
    }

    /// A NEW commit on top of the current one: the tree moved and nothing in
    /// the working tree is dirty. The only input the frozen staleness rule is
    /// allowed to call stale by movement.
    fn advance(&self) {
        std::fs::write(self.root.join("src/a.rs"), "fn parse() { let x = 1; }\n").unwrap();
        rof::engine::tree::TreeService::new(self.root.clone())
            .baseline()
            .unwrap();
    }

    /// Write a note through the store's own path, as `/research write` does.
    fn store_note(&self, topic: &str, body: &str) {
        let mut store = Research::load(&self.root);
        store
            .apply(
                Edit::Write {
                    topic: topic.to_string(),
                    body: body.to_string(),
                },
                &self.head(),
            )
            .unwrap();
        store.save().unwrap();
    }

    /// The topic the run derives for GOAL — the address the lookup uses.
    fn topic(&self) -> String {
        research::topic_for(GOAL).expect("GOAL yields a topic")
    }

    fn store(&self) -> Research {
        Research::load(&self.root)
    }

    fn note_exists(&self, topic: &str) -> bool {
        self.store().note(topic).is_some()
    }

    fn index_text(&self) -> Option<String> {
        std::fs::read_to_string(self.root.join(DIR).join("index.md")).ok()
    }

    fn workdir(&self) -> &Path {
        &self.root
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A canned model. The research call is dispatched on its own marker so a
/// test can count it exactly, which is how "bought once" and "bought nothing"
/// are told apart from each other.
struct Fake {
    /// The reply the research call gets. Deliberately un-shapeable in some
    /// tests: an unanswerable step must write nothing.
    research_reply: String,
    /// Research calls this fake saw, in order.
    research_calls: AtomicUsize,
    /// Every implementer prompt, in call order.
    impl_prompts: Mutex<Vec<String>>,
    /// Whether the implementer writes a file at all.
    writes: bool,
}

impl Fake {
    fn answering(body: &str) -> Arc<Fake> {
        Arc::new(Fake {
            research_reply: format!("{{\"body\": {}}}", serde_json::json!(body)),
            research_calls: AtomicUsize::new(0),
            impl_prompts: Mutex::new(Vec::new()),
            writes: false,
        })
    }

    /// A model whose research reply is whatever the test hands it — including
    /// replies that are not a note at all.
    fn replying(reply: &str) -> Arc<Fake> {
        Arc::new(Fake {
            research_reply: reply.to_string(),
            research_calls: AtomicUsize::new(0),
            impl_prompts: Mutex::new(Vec::new()),
            writes: false,
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

    fn first_impl_prompt(&self) -> String {
        self.impl_prompts
            .lock()
            .unwrap()
            .first()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl LlmClient for Fake {
    async fn complete(&self, _model: &str, req: LlmReq) -> Result<LlmResp, LlmError> {
        if req.system.contains("Compress the input") {
            return Fake::resp("condensed".to_string());
        }
        // The research step, and nothing else, carries this marker.
        if req.system.contains(research::SYSTEM_MARKER) {
            self.research_calls.fetch_add(1, Ordering::SeqCst);
            return Fake::resp(self.research_reply.clone());
        }
        if req.system.contains("reviewer") {
            return Fake::resp("{\"pass\": true, \"feedback\": \"looks good\"}".to_string());
        }
        if req.system.contains("implementer") || req.system.contains("direct coding agent") {
            self.impl_prompts.lock().unwrap().push(req.prompt.clone());
            if self.writes {
                return Fake::resp(
                    "{\"patches\":[{\"path\":\"src/a.rs\",\"search\":\"fn parse() {}\",\
                     \"replace\":\"fn parse() { let x = 1; }\"}],\"notes\":\"ok\"}"
                        .to_string(),
                );
            }
            return Fake::resp(
                "{\"artifact\": \"read the parser\", \"notes\": \"ok\"}".to_string(),
            );
        }
        Fake::resp("{\"artifact\": \"ok\"}".to_string())
    }
}

/// The harness. `knob` is the config flag under test; whether the write gate
/// has anything to say is the session's `expect_writes`, set in [`run`].
fn harness(
    client: Arc<Fake>,
    s: &Scratch,
    knob: bool,
) -> (Orchestrator, ToolRegistry, Arc<TraceSink>) {
    let root = s.workdir().to_path_buf();
    let mut reg = ToolRegistry::new(PermissionPolicy {
        allowed_dirs: vec![root.clone()],
        allowed_commands: Vec::new(),
        ..Default::default()
    });
    reg.register(FsListTool::new(root.clone()));
    reg.register(FsReadTool::new(root.clone()));
    reg.register(FsPatchTool::new(root.clone()));
    reg.register(ProcRunTool::new(root, Vec::new()));

    let cfg = AppConfig {
        max_review_rounds: 1,
        execution: "pipeline".to_string(),
        research_before_work: knob,
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

async fn run(
    client: Arc<Fake>,
    s: &Scratch,
    knob: bool,
    expect_writes: bool,
) -> (serde_json::Value, Vec<TraceEvent>, Arc<TraceSink>) {
    let session = Session::new(GOAL.to_string()).expecting_writes(expect_writes);
    run_session(client, s, knob, &session).await
}

/// The same run with a caller-supplied session, so a byte-identity test can
/// hold the session id constant — `Session::new` mints a fresh uuid and would
/// otherwise be the only difference between two otherwise identical runs.
async fn run_session(
    client: Arc<Fake>,
    s: &Scratch,
    knob: bool,
    session: &Session,
) -> (serde_json::Value, Vec<TraceEvent>, Arc<TraceSink>) {
    let (orch, reg, trace) = harness(client, s, knob);
    let out = orch.run_loop(session, &reg, s.workdir()).await;
    (out, trace.events(), trace)
}

/// The research trace events of a run, in order.
fn research_events(events: &[TraceEvent]) -> Vec<(&str, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::ResearchStep {
                topic,
                action,
                reason,
            } => Some((action.as_str(), format!("{topic}: {reason}"))),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 1. RETRIEVAL BEFORE RESEARCH: a fresh note is served, nothing is bought.
// ---------------------------------------------------------------------------

/// The whole property, on the run: a fresh note's body is in the implementer's
/// FIRST prompt, and the research step bought NOTHING.
#[tokio::test]
async fn a_fresh_note_reaches_the_first_prompt_and_buys_no_research() {
    let s = Scratch::new("fresh");
    let topic = s.topic();
    s.store_note(
        &topic,
        "the parser skips a blank line because trim runs first",
    );

    let client = Fake::answering("a note the run must never buy");
    let (out, events, _trace) = run(client.clone(), &s, true, false).await;

    let prompt = client.first_impl_prompt();
    assert!(
        prompt.contains("the parser skips a blank line because trim runs first"),
        "a fresh note must be in the implementer's FIRST prompt, which is the only \
         prompt that can prove the lookup ran BEFORE the spend"
    );
    assert!(
        !prompt.contains("a note the run must never buy"),
        "a fresh note must buy no research, and none may be injected"
    );
    assert_eq!(
        client.research_calls.load(Ordering::SeqCst),
        0,
        "retrieval before research: a fresh answer costs zero calls"
    );
    let steps = research_events(&events);
    assert_eq!(steps.len(), 1, "one decision, recorded: {steps:?}");
    assert_eq!(steps[0].0, "retrieved", "the verdict was Answered");
    assert_eq!(out["passed"], true, "{out}");
}

/// A stale note is NOT served. The tree moved off the pin, so the run buys
/// fresh research, and the trace says which happened.
#[tokio::test]
async fn a_moved_pin_forces_a_rebuy_rather_than_serving_the_stale_body() {
    let s = Scratch::new("stale");
    let topic = s.topic();
    s.store_note(&topic, "STALE-BODY-MUST-NOT-BE-SERVED");
    s.advance();

    let client = Fake::answering("the empty line is dropped by the lexer, not the parser");
    let (_out, events, _trace) = run(client.clone(), &s, true, false).await;

    assert_eq!(
        client.research_calls.load(Ordering::SeqCst),
        1,
        "a stale note is not usable, so exactly one call is bought"
    );
    let prompt = client.first_impl_prompt();
    assert!(
        !prompt.contains("STALE-BODY-MUST-NOT-BE-SERVED"),
        "a stale note must never reach a prompt: {prompt}"
    );
    let steps = research_events(&events);
    let bought = steps.iter().find(|(a, _)| *a == "bought");
    assert!(
        bought.is_some(),
        "the trace must name the buy, not a retrieval: {steps:?}"
    );
    assert!(
        bought.unwrap().1.contains("stale"),
        "the reason must say the note was stale, computed: {}",
        bought.unwrap().1
    );
}

// ---------------------------------------------------------------------------
// 2. THE ROUND TRIP: buy once, then retrieve.
// ---------------------------------------------------------------------------

/// The property the whole store was built for. An absent note buys exactly
/// one bounded call; the answer is written back as a note pinning the commit
/// the research ran against; the NEXT run finds it fresh, injects it, and buys
/// nothing.
#[tokio::test]
async fn an_absent_note_is_bought_once_and_the_next_run_retrieves_it() {
    let s = Scratch::new("roundtrip");
    let topic = s.topic();
    let client = Fake::answering("an empty line is skipped by trim() in parse()");

    // Run 1: nothing in the store, so one call is bought.
    let (_first, first_events, _t) = run(client.clone(), &s, true, false).await;
    assert_eq!(
        client.research_calls.load(Ordering::SeqCst),
        1,
        "the absent note buys exactly one call"
    );
    assert!(
        s.note_exists(&topic),
        "the bought note must be WRITTEN BACK, or the next run re-buys it. \
         index: {:?}",
        s.index_text()
    );
    let after_first = s.store();
    let note = after_first.note(&topic).expect("the note is in the index");
    assert_eq!(
        note.pinned.clone(),
        s.head().head().unwrap().to_string(),
        "the note must pin the commit the research ran against"
    );
    assert_eq!(
        research_events(&first_events)[0].0,
        "bought",
        "the first run bought"
    );

    // Run 2: same goal, same work root. The note is fresh, so no call.
    let prompt = {
        let (second, second_events, _t) = run(client.clone(), &s, true, false).await;
        assert_eq!(
            client.research_calls.load(Ordering::SeqCst),
            1,
            "the second run must retrieve the note it bought, not buy it again"
        );
        assert_eq!(research_events(&second_events)[0].0, "retrieved");
        assert_eq!(second["passed"], true, "{second}");
        client.first_impl_prompt()
    };
    assert!(
        prompt.contains("an empty line is skipped by trim() in parse()"),
        "run 2 must be handed the note run 1 bought"
    );
}

/// The write-back must not be able to satisfy the write gate on its own. A run
/// that bought research and shipped no code change must still fail
/// `expect_writes`: harness bookkeeping is not a deliverable.
#[tokio::test]
async fn a_research_note_alone_cannot_satisfy_the_write_gate() {
    let s = Scratch::new("gate");
    let client = Fake::answering("an empty line is skipped by trim() in parse()");
    let (out, _events, _trace) = run(client.clone(), &s, true, true).await;

    assert_eq!(
        client.research_calls.load(Ordering::SeqCst),
        1,
        "the note is still bought — this test is about the GATE, not the buy"
    );
    assert!(
        s.note_exists(&s.topic()),
        "the note must really be on disk, or the assertion below is vacuous"
    );
    assert_eq!(
        out["tasks"][0]["writes_made"], 0,
        "a `.rof/` entry the harness wrote is not the agent's deliverable: {out}"
    );
    assert_eq!(
        out["passed"], false,
        "a run that changed no source must not pass a write-gated task on the \
         strength of the harness's own note: {out}"
    );
}

// ---------------------------------------------------------------------------
// 3. NEVER FABRICATE: nothing usable means nothing written.
// ---------------------------------------------------------------------------

/// A model that cannot produce a note leaves the store EXACTLY as it was, says
/// why, and the run continues. A harness-authored body would be the failure
/// this design exists to prevent.
#[tokio::test]
async fn a_model_that_cannot_produce_a_note_writes_nothing_and_says_why() {
    for (n, reply) in [
        // No `body` key at all.
        "{\"summary\": \"nothing found\"}",
        // A body the store would have to fill in for itself.
        "{\"body\": \"   \"}",
        // Not the shape at all.
        "the parser skips blank lines",
        // An empty completion.
        "",
    ]
    .iter()
    .enumerate()
    {
        let s = Scratch::new(&format!("mute{n}"));
        let before = s.index_text();
        let client = Fake::replying(reply);
        let (out, events, _trace) = run(client.clone(), &s, true, false).await;
        assert_eq!(
            s.index_text(),
            before,
            "an unusable research reply must leave the store untouched (reply {reply:?})"
        );
        assert!(!s.note_exists(&s.topic()), "no note may be written");
        assert_eq!(out["passed"], true, "the run still finishes: {out}");
        let steps = research_events(&events);
        assert!(
            steps.iter().any(|(a, _)| *a == "declined"),
            "a refusal must be visible, never silent: {steps:?}"
        );
        assert!(
            steps.iter().any(|(_, r)| !r.is_empty()),
            "the refusal must say why: {steps:?}"
        );
    }
}

/// A store whose index cannot be read is not rewritten and not consulted: the
/// user���s file is the record, and an unreadable index must never read as "this
/// repo has no research".
#[tokio::test]
async fn a_degraded_index_is_neither_consulted_nor_overwritten() {
    let s = Scratch::new("degraded");
    std::fs::create_dir_all(s.workdir().join(DIR)).unwrap();
    let broken = "# Research index\n\nnot a machine block at all\n";
    std::fs::write(s.workdir().join(DIR).join("index.md"), broken).unwrap();
    let client = Fake::answering("a note that must not be bought from a broken store");

    let (out, events, _trace) = run(client.clone(), &s, true, false).await;

    assert_eq!(
        client.research_calls.load(Ordering::SeqCst),
        0,
        "a store that could not be read is not a store that says 'buy'"
    );
    assert_eq!(
        std::fs::read_to_string(s.workdir().join(DIR).join("index.md")).unwrap(),
        broken,
        "the user's file must survive a run untouched"
    );
    assert_eq!(out["passed"], true, "{out}");
    assert!(
        research_events(&events)
            .iter()
            .any(|(a, _)| *a == "declined"),
        "the degraded read must be surfaced: {:?}",
        research_events(&events)
    );
}

// ---------------------------------------------------------------------------
// 4. THE HEAD: bounded, and never crowding out AGENTS.md.
// ---------------------------------------------------------------------------

/// A retrieved note is knowledge about the project, so it rides the same head
/// as the project conventions — and it must not be able to push them out. A
/// 400 KB note against a 4 KB `AGENTS.md` still leaves the conventions at the
/// front of the delivered long-term layer.
#[tokio::test]
async fn the_injected_note_is_bounded_and_cannot_displace_agents_md() {
    let s = Scratch::new("bounded");
    let topic = s.topic();
    std::fs::write(
        s.workdir().join("AGENTS.md"),
        "CONVENTIONS-HEAD-MARKER: small diffs, cargo test must pass\n",
    )
    .unwrap();
    let huge = format!("RESEARCH-FILLER-MARKER {}", "x".repeat(400_000));
    s.store_note(&topic, &huge);

    let client = Fake::answering("unused");
    let (_out, _events, _trace) = run(client.clone(), &s, true, false).await;
    let prompt = client.first_impl_prompt();
    assert!(
        !prompt.is_empty(),
        "the run must have reached the implementer at all"
    );
    // The conventions are still at the front of the stable head.
    let conv = prompt
        .find("CONVENTIONS-HEAD-MARKER")
        .expect("a 400 KB research note must never evict AGENTS.md from the head");
    let note_at = prompt
        .find("RESEARCH-FILLER-MARKER")
        .expect("the note is in the head, just bounded");
    assert!(
        conv < note_at,
        "project conventions lead, so a user-appendable section cannot crowd \
         them out (conventions at {conv}, note at {note_at})"
    );
    // And the note is bounded: the delivered block is a fraction of the body.
    let delivered = &prompt[note_at..];
    assert!(
        delivered.len() < 100_000,
        "a 400 KB note must be head-capped, not delivered whole: {} chars",
        delivered.len()
    );
    // The cap itself needs no assertion — it is a constant, and asserting a
    // constant is a tautology. What matters is the behaviour asserted above:
    // a body far larger than any cap is delivered truncated.
    // Nothing was bought: the fixture's note was fresh.
    assert_eq!(client.research_calls.load(Ordering::SeqCst), 0);
}

/// The same head, read through the layer machinery itself: the section is
/// composed by the same bounded-head discipline the profile section uses, and
/// the block is empty when nothing answered.
#[test]
fn the_head_block_is_empty_unless_a_note_answered() {
    assert_eq!(Consulted::Off.head_block(), "");
    assert_eq!(
        Consulted::Declined {
            topic: "t".into(),
            reason: "r".into(),
        }
        .head_block(),
        "",
        "a declined lookup contributes nothing to the head"
    );
    let answer = store::Answer {
        topic: "retry backoff".into(),
        kind: store::Kind::Design,
        path: ".rof/research/retry backoff.md".into(),
        pinned: "abc123".into(),
        body: "the window resets after 60s idle".into(),
    };
    let block = Consulted::Answered {
        answer,
        bought: false,
    }
    .head_block();
    assert!(block.contains("retry backoff"), "the block names its topic");
    assert!(
        block.contains("verified against abc123"),
        "the answer travels with the commit it was verified against: {block}"
    );
    assert!(
        block.contains("the window resets after 60s idle"),
        "{block}"
    );
}

// ---------------------------------------------------------------------------
// 5. THE KNOB: off means off, byte for byte.
// ---------------------------------------------------------------------------

/// With the knob off the run must not even LOOK at the folder: a store full of
/// fresh notes, and one that does not exist, produce the same run — same
/// result bytes, same trace bytes, same prompt, no call.
#[tokio::test]
async fn the_knob_off_leaves_the_run_byte_identical() {
    let with_note = Scratch::new("knob-on-store");
    let topic = with_note.topic();
    with_note.store_note(&topic, "a note that must not be read at all");
    let without = Scratch::new("knob-off-store");

    // ONE session for both runs: `Session::new` mints a uuid, and a byte
    // comparison must not be decided by that.
    let session = Session::new(GOAL.to_string()).expecting_writes(false);
    let a = Fake::answering("unused");
    let (out_a, events_a, _t) = run_session(a.clone(), &with_note, false, &session).await;
    let b = Fake::answering("unused");
    let (out_b, events_b, _t) = run_session(b.clone(), &without, false, &session).await;

    assert_eq!(
        serde_json::to_string(&out_a).unwrap(),
        serde_json::to_string(&out_b).unwrap(),
        "the default run must not depend on the research folder's contents"
    );
    assert_eq!(
        serde_json::to_string(&events_a).unwrap(),
        serde_json::to_string(&events_b).unwrap(),
        "the default trace must not name the folder either"
    );
    assert_eq!(a.first_impl_prompt(), b.first_impl_prompt());
    assert_eq!(a.research_calls.load(Ordering::SeqCst), 0);
    assert_eq!(b.research_calls.load(Ordering::SeqCst), 0);
    assert!(
        research_events(&events_a).is_empty(),
        "a run that never consulted the store must not report a decision: {:?}",
        research_events(&events_a)
    );
    // And the knob is off by default, which is the whole cost argument.
    assert!(
        !AppConfig::default().research_before_work,
        "the knob defaults off: a run must be able to state the cost before \
         paying it"
    );
}

/// With the knob off, no research work happens even when the topic is one the
/// run would have bought: the flag gates the LOOKUP, so it gates the buy.
#[tokio::test]
async fn the_knob_off_buys_nothing_even_with_nothing_in_the_store() {
    let s = Scratch::new("knob-off-buy");
    let client = Fake::answering("must not be bought");
    let (_out, events, _trace) = run(client.clone(), &s, false, false).await;
    assert_eq!(client.research_calls.load(Ordering::SeqCst), 0);
    assert!(!s.note_exists(&s.topic()));
    assert!(research_events(&events).is_empty());
}

// ---------------------------------------------------------------------------
// 6. ORDER: the lookup precedes the spend.
// ---------------------------------------------------------------------------

/// The test a wiring placed AFTER the round loop fails. The research decision
/// is recorded in the durable stream, and the run's first expensive call is
/// recorded there too: the decision must come first, and the note must be in
/// the prompt that first call was built from.
#[tokio::test]
async fn the_lookup_precedes_the_first_expensive_call() {
    let s = Scratch::new("order");
    let topic = s.topic();
    s.store_note(&topic, "ORDERING-MARKER: read before the spend");

    let client = Fake::answering("unused");
    let (_out, events, _trace) = run(client, &s, true, false).await;

    let research_at = events
        .iter()
        .position(|e| matches!(e, TraceEvent::ResearchStep { .. }))
        .expect("the lookup is recorded");
    let first_model_call = events
        .iter()
        .position(|e| matches!(e, TraceEvent::ModelCall { .. }))
        .expect("the run made a model call");
    assert!(
        research_at < first_model_call,
        "the folder is consulted BEFORE the first model call, not after it: \
         research at {research_at}, first call at {first_model_call}"
    );
}

// ---------------------------------------------------------------------------
// 7. THE ADDRESS: a topic is derived, never guessed.
// ---------------------------------------------------------------------------

/// Retrieval is a path lookup, so the run has to name the path. It derives one
/// from the goal, and the derivation is total: no separators, no `..`, no
/// leading dot, never a `tests/` topic (a design run must never reach the
/// tests folder), and never longer than the store accepts.
#[test]
fn the_topic_is_derived_from_the_goal_and_is_always_addressable() {
    for goal in [
        GOAL,
        "g",
        "tests/retention flaky",
        "../../etc/passwd",
        ".hidden",
        "   ",
        "***",
        "a goal that is much much longer than sixty four characters so the \
         readable part of its address has to be cut somewhere",
        "юникод и символы",
    ] {
        match research::topic_for(goal) {
            Some(topic) => {
                assert!(!topic.is_empty(), "{goal:?}");
                assert!(!topic.contains('/'), "a topic is ONE segment: {topic:?}");
                assert!(!topic.contains('\\'), "{topic:?}");
                assert!(!topic.contains(".."), "{topic:?}");
                assert!(!topic.starts_with('.'), "{topic:?}");
                assert!(
                    topic.chars().count() <= store::MAX_TOPIC_CHARS,
                    "{topic:?} is longer than the store accepts"
                );
                assert!(
                    !topic.starts_with("tests/"),
                    "a design run must never address the tests folder: {topic:?}"
                );
            }
            None => {
                // The only refusals are topics the store could not address.
                assert!(
                    goal.trim().chars().all(|c| !c.is_alphanumeric()),
                    "a goal with usable text must yield a topic: {goal:?}"
                );
            }
        }
    }
    // Distinct goals, distinct addresses: a truncated slug alone would collide,
    // and a note served for the wrong goal is the failure the store exists to
    // prevent.
    let a = research::topic_for("fix the parser so it accepts an empty line at all").unwrap();
    let b = research::topic_for("fix the parser so it accepts an empty line in a.rs").unwrap();
    assert_ne!(a, b, "two goals must never share one address");
    assert_eq!(
        research::topic_for(GOAL),
        research::topic_for(GOAL),
        "the address is a function of the goal, so a second run finds it"
    );
}
