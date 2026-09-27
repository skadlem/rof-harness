//! §6 / P3 build item 3: the prompt cache prefix, measured and defended.
//!
//! Prompt caching cut cost 41-80% and TTFT 13-31% across 500+ sessions — but
//! "strategic prompt cache block control, such as placing dynamic content at the
//! end of the system prompt, provides more consistent benefits than naive
//! full-context caching, which can paradoxically increase latency". A harness
//! that appends per-turn content into the MIDDLE of the request breaks its own
//! prefix, so a growing conversation can cost more than a short one.
//!
//! So this file does not take the order on faith. It measures it. One task runs
//! over two rounds (the first verdict fails), which gives four implementer
//! requests and two reviewer requests, and for each pair it computes how many
//! leading chars are byte-identical. The invariant is the ORDER of those
//! numbers, not their magnitude: what does not change between rounds must sit
//! ABOVE the per-round block, or every round re-sends it for nothing.
//!
//! Headless throughout: a fake LLM client, a temp tree, no network, no PTY,
//! no sleeps.

use async_trait::async_trait;
use rof::config::{AppConfig, PermissionPolicy};
use rof::engine::{Orchestrator, Session};
use rof::eval::metrics::EvalReport;
use rof::llm::{ContextService, ExecutorService, LlmClient, LlmError, LlmReq, LlmResp};
use rof::obs::{TraceEvent, TraceSink};
use rof::tools::{FsListTool, FsPatchTool, FsReadTool, ProcRunTool, ToolRegistry};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// The per-round block. Everything the harness wrote about THIS round — the
/// round counter, the reviewer feedback, the check log, the round's evidence —
/// lives in and below it, and it is the one place a per-round change is allowed
/// to start.
const VOLATILE_MARKER: &str = "[SHORT-TERM]";

/// A request as the fake client saw it. `prompt` is the ground truth for "what
/// the model actually received", so every number here is measured on the string
/// that went out rather than on a re-derivation of it.
#[derive(Debug, Clone)]
struct Call {
    agent: String,
    system: String,
    prompt: String,
}

/// One failing verdict then one passing one, so the same task gets a second
/// implementer round over the same goal, plan, task line and file map. The two
/// rounds differ only in what is per-round, which is exactly the comparison
/// that shows where the cache prefix breaks.
struct Fake {
    calls: Mutex<Vec<Call>>,
    reviews: AtomicUsize,
}

impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            reviews: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn resp(text: &str) -> Result<LlmResp, LlmError> {
        Ok(LlmResp {
            text: text.to_string(),
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
        let agent = if req.system.contains("You are a reviewer") {
            "reviewer"
        } else {
            "implementer"
        }
        .to_string();
        self.calls.lock().unwrap().push(Call {
            agent: agent.clone(),
            system: req.system.clone(),
            prompt: req.prompt.clone(),
        });
        if agent == "reviewer" {
            let n = self.reviews.fetch_add(1, Ordering::SeqCst) + 1;
            return if n == 1 {
                Self::resp("{\"pass\": false, \"feedback\": \"the field list is not right\"}")
            } else {
                Self::resp("{\"pass\": true, \"feedback\": \"matches the plan\"}")
            };
        }
        // The read-before-write reflex, once per round: a model that has not
        // seen the file asks for it and is asked again with it.
        if !req.prompt.contains("--- schema.txt") {
            return Self::resp("{\"reads\":[\"schema.txt\"],\"artifact\":\"need the field list\"}");
        }
        // Round 2 lands a real edit, so the reviewer's second prompt carries
        // evidence the first one did not: the two reviewer rounds differ
        // exactly where they are supposed to, and nowhere else.
        if req.prompt.contains("round 2/2") {
            return Self::resp(
                "{\"patches\":[{\"path\":\"a.txt\",\"search\":\"beta_real_line\",\
                 \"replace\":\"beta_fixed\"}],\"notes\":\"ok\"}",
            );
        }
        Self::resp("{\"artifact\":\"matched spec.txt\",\"notes\":\"ok\"}")
    }
}

/// One temp tree, registry and orchestrator. The goal names `spec.txt`, so its
/// bytes ride the implementer's tail every round; the model asks for
/// `schema.txt`, which only arrives on the re-ask. The skills root points
/// inside the temp dir so a populated `~/.rof/skills` cannot change a prompt.
fn harness(client: Arc<Fake>, tag: &str) -> (Orchestrator, ToolRegistry, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("rof-pcp-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), "alpha\nbeta_real_line\ngamma\n").unwrap();
    // Named by the goal, never patched, and big enough that re-sending it
    // every round is the thing worth measuring: the same bytes in round 1 and
    // round 2, sitting in the implementer's tail.
    let mut spec = String::from("struct Thing { field_a: u8, field_b: u8 }\n");
    while spec.chars().count() < 4_000 {
        spec.push_str("// a stable line of the file the goal names\n");
    }
    std::fs::write(root.join("spec.txt"), &spec).unwrap();
    // Asked for with `reads`, so it arrives only on the re-ask.
    std::fs::write(
        root.join("schema.txt"),
        "pub fn load() -> Thing { todo!() }\n",
    )
    .unwrap();
    // A few more paths, so the map is a map and not three lines.
    for name in ["mod_a.rs", "mod_b.rs", "mod_c.rs", "notes.md"] {
        std::fs::write(root.join(name), format!("// {name}\n")).unwrap();
    }

    let policy = PermissionPolicy {
        allowed_dirs: vec![root.clone()],
        allowed_commands: vec!["echo checked".to_string()],
        ..Default::default()
    };
    let mut reg = ToolRegistry::new(policy);
    reg.register(FsListTool::new(root.clone()));
    reg.register(FsReadTool::new(root.clone()));
    reg.register(FsPatchTool::new(root.clone()));
    reg.register(ProcRunTool::new(
        root.clone(),
        vec!["echo checked".to_string()],
    ));

    let cfg = AppConfig {
        max_review_rounds: 2,
        skills: rof::config::SkillsConfig {
            root: Some(root.join("no-skills")),
            ..Default::default()
        },
        ..Default::default()
    };
    let context = ContextService::new(client.clone(), "fake-ctx".to_string());
    let executor = ExecutorService::new(client.clone(), "fake-exec".to_string(), None);
    let verify = ExecutorService::new(client, "fake-verify".to_string(), None);
    (
        Orchestrator::new(cfg, Arc::new(TraceSink::new()), context, executor, verify),
        reg,
        root,
    )
}

/// Leading chars two requests share — the unit a provider's cache prefix works
/// in, measured on the strings that were sent.
fn common_prefix_chars(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// The invariant, as a function: a request is cache-ordered when every part
/// that does not change between rounds sits ABOVE the per-round block. Returns
/// the offending marker when it does not, so a failure names what moved.
fn misplaced_dynamic_content(prompt: &str, stable: &[&str]) -> Option<String> {
    let boundary = prompt.find(VOLATILE_MARKER)?;
    let stable_part = &prompt[..boundary];
    let misplaced: Vec<&str> = stable
        .iter()
        .copied()
        .filter(|marker| stable_part.find(marker).is_none() && prompt.contains(marker))
        .collect();
    if misplaced.is_empty() {
        None
    } else {
        Some(format!(
            "stable content delivered BELOW the per-round block {VOLATILE_MARKER}: {}",
            misplaced.join(", ")
        ))
    }
}

/// The fixture's six requests, in the order the client received them.
struct Turns {
    /// Round 1's first ask, and the `reads` re-ask that follows it.
    ask_1: String,
    reask_1: String,
    /// Round 2 over the same task after a failing verdict, and ITS re-ask.
    ask_2: String,
    reask_2: String,
    reviewer_1: String,
    reviewer_2: String,
}

async fn run(tag: &str) -> (Turns, Vec<Call>, Vec<TraceEvent>, std::path::PathBuf) {
    let client = Fake::new();
    let (orch, reg, root) = harness(client.clone(), tag);
    let out = orch
        .run_loop(&Session::new("follow spec.txt".into()), &reg, &root)
        .await;
    assert_eq!(
        out["rounds"].as_u64(),
        Some(2),
        "the fixture must run two rounds: {out:?}"
    );
    let events = orch.trace().events();
    let calls = client.calls();
    let impls: Vec<String> = calls
        .iter()
        .filter(|c| c.agent == "implementer")
        .map(|c| c.prompt.clone())
        .collect();
    let revs: Vec<String> = calls
        .iter()
        .filter(|c| c.agent == "reviewer")
        .map(|c| c.prompt.clone())
        .collect();
    assert_eq!(impls.len(), 4, "two rounds, one re-ask each: {impls:?}");
    assert_eq!(revs.len(), 2, "one verdict per round");
    (
        Turns {
            ask_1: impls[0].clone(),
            reask_1: impls[1].clone(),
            ask_2: impls[2].clone(),
            reask_2: impls[3].clone(),
            reviewer_1: revs[0].clone(),
            reviewer_2: revs[1].clone(),
        },
        calls,
        events,
        root,
    )
}

// ---------------------------------------------------------------------------
// 1. The measurement itself, with the real numbers for a known fixture.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn prefix_stability_is_measured_across_the_turns_of_a_round() {
    let (t, calls, _events, root) = run("measure").await;

    // Round 1 vs the re-ask: the re-ask re-sends the whole assembled context
    // plus the requested file, so the ask is a prefix of it and the shared
    // prefix is the whole ask. Nothing may rewrite what came before.
    let ask_reask = common_prefix_chars(&t.ask_1, &t.reask_1);
    // Round 1 vs round 2 over one task: the pair that decides whether a growing
    // conversation keeps its cache.
    let round_prefix = common_prefix_chars(&t.ask_1, &t.ask_2);
    // The reviewer appends nothing below its layers, so its per-round block is
    // already last and its two rounds share exactly their stable head.
    let reviewer_prefix = common_prefix_chars(&t.reviewer_1, &t.reviewer_2);
    eprintln!(
        "prefix chars: ask1={} reask1={} ask2={} reask2={} rev1={} rev2={} | \
         ask1~reask1={ask_reask} ask1~ask2={round_prefix} rev1~rev2={reviewer_prefix}",
        t.ask_1.chars().count(),
        t.reask_1.chars().count(),
        t.ask_2.chars().count(),
        t.reask_2.chars().count(),
        t.reviewer_1.chars().count(),
        t.reviewer_2.chars().count(),
    );
    // The fixture's numbers, asserted: the shared prefix in chars for the three
    // pairs the harness produces. Before this build item moved the per-round
    // block, ask1~ask2 was 239 of 4,351 — the goal, the plan and the task line
    // and nothing else, because the round counter sat above the file map and
    // the goal-named file. The reviewer's pair is unchanged at 190: it appends
    // nothing below its layers, so its per-round block was already last.
    assert_eq!(ask_reask, 4_351, "the re-ask shares the whole ask");
    assert_eq!(round_prefix, 4_333, "the two rounds share the stable tail");
    assert_eq!(reviewer_prefix, 190, "the reviewer was already ordered");

    // The per-round block is the ONE place the two rounds of a task may
    // differ: nothing per-round may appear above it, or that content is
    // re-sent on every round for nothing. The shared prefix must therefore
    // reach AT LEAST the block's own first byte.
    let block_at = t.ask_1.find(VOLATILE_MARKER).expect("a per-round block");
    let rev_block_at = t
        .reviewer_1
        .find(VOLATILE_MARKER)
        .expect("a per-round block");
    assert!(
        round_prefix >= block_at,
        "round 1 and round 2 diverge at {round_prefix}, before the per-round \
         block at {block_at}: per-round content is riding above the stable \
         prefix"
    );
    assert!(
        reviewer_prefix >= rev_block_at,
        "the reviewer's two rounds diverge at {reviewer_prefix}, before its \
         per-round block at {rev_block_at}"
    );
    // ... and they must share a real prefix, not zero: the goal, the plan and
    // the task line are the same on both rounds and must not be re-sent.
    assert!(
        round_prefix > 0 && reviewer_prefix > 0,
        "a zero prefix means the per-round block was hoisted over the stable \
         head: {round_prefix} / {reviewer_prefix}"
    );
    // The system prompt is its own request field, so it is cacheable on its own
    // and must be byte-identical across every call the agent makes.
    let systems: Vec<&str> = calls.iter().map(|c| c.system.as_str()).collect();
    let implementer_system = systems
        .iter()
        .find(|s| s.contains("You are an implementer"))
        .expect("the implementer ran")
        .to_string();
    assert!(
        systems.iter().filter(|s| **s == implementer_system).count() == 4,
        "the implementer's system prompt must be byte-identical on all four \
         of its calls: {systems:?}"
    );
    std::fs::remove_dir_all(&root).ok();
}

// ---------------------------------------------------------------------------
// 2. The order: the per-round block goes last. This is the red test.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_per_round_block_is_delivered_after_the_stable_material() {
    // Before the fix the implementer's request read
    //   [LONG-TERM] [MID-TERM] [SHORT-TERM: round 1/2, feedback] [REPO FILES] [--- spec.txt]
    // so a 20-char round counter sat ABOVE the file map and the goal-named
    // file, and every round re-sent both of them from scratch. Neither changes
    // between rounds; the counter does.
    let (t, _calls, _events, root) = run("order").await;
    for (name, prompt) in [
        ("round-1 ask", &t.ask_1),
        ("re-ask", &t.reask_1),
        ("round-2 ask", &t.ask_2),
    ] {
        assert!(
            prompt.contains(VOLATILE_MARKER),
            "{name}: the fixture must have a per-round block to order"
        );
        for stable in [
            "[LONG-TERM]",
            "goal:",
            "CURRENT TASK",
            "[REPO FILES]",
            "--- spec.txt",
        ] {
            assert_eq!(
                misplaced_dynamic_content(prompt, &[stable]),
                None,
                "{name}: {stable} must sit above {VOLATILE_MARKER}"
            );
        }
    }
    // The reviewer's whole turn is its layers, so its per-round evidence is
    // already last; the same guard is the statement of that.
    assert_eq!(
        misplaced_dynamic_content(&t.reviewer_1, &["PLAN:", "CURRENT TASK"]),
        None
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn the_stable_prefix_is_never_smaller_than_it_was() {
    // The floor, measured on this harness BEFORE the reorder: the two rounds of
    // one task shared the long-term head, the goal, the plan and the task line
    // and nothing else, because the round counter and everything the assembler
    // placed under it all came later. 239 chars of a 4,351-char request.
    const PRE_REORDER_FLOOR: usize = 239;
    let (t, _calls, _events, root) = run("floor").await;
    let round_prefix = common_prefix_chars(&t.ask_1, &t.ask_2);
    assert!(
        round_prefix >= PRE_REORDER_FLOOR,
        "the round-over-round stable prefix regressed from {PRE_REORDER_FLOOR} \
         to {round_prefix}"
    );
    // ... and the prefix must now reach past the per-round block to the end of
    // the whole stable tail: the file map AND the goal-named file, which is
    // where the hundreds of chars the reorder buys live.
    let map_at = t.ask_1.find("[REPO FILES]").expect("the map is delivered");
    let map_end = map_at + "[REPO FILES]".chars().count();
    let file_at = t
        .ask_1
        .find("--- spec.txt")
        .expect("the goal-named file is delivered");
    let file_end = file_at + "struct Thing { field_a: u8, field_b: u8 }".chars().count();
    assert!(
        round_prefix > map_end,
        "the stable prefix ({round_prefix}) stops inside the file map ({map_end})"
    );
    assert!(
        round_prefix > file_end,
        "the stable prefix ({round_prefix}) stops before the goal-named file \
         ends ({file_end}): the per-round block is still above it"
    );
    std::fs::remove_dir_all(&root).ok();
}

// ---------------------------------------------------------------------------
// 3. Same content, reordered only — and the totals unchanged.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_reorder_moves_content_without_adding_or_removing_any() {
    let (t, _calls, _events, root) = run("content").await;
    // Every block the request carried, each exactly once: membership, not a
    // substring of one particular order.
    for block in [
        "[LONG-TERM]",
        "conventions: small diffs",
        "[MID-TERM]",
        "goal: follow spec.txt",
        "CURRENT TASK (1/1)",
        "[SHORT-TERM]",
        "WRITES REQUIRED: yes",
        "round 1/2",
        "[REPO FILES]",
        "--- spec.txt",
        "struct Thing { field_a: u8, field_b: u8 }",
    ] {
        assert_eq!(
            t.ask_1.matches(block).count(),
            1,
            "block {block:?} must survive the reorder exactly once"
        );
    }
    // The re-ask carries everything the ask carried, plus the requested file
    // and the marker naming it.
    for block in [
        "[LONG-TERM]",
        "goal: follow spec.txt",
        "CURRENT TASK (1/1)",
        "[SHORT-TERM]",
        "round 1/2",
        "[REPO FILES]",
        "--- spec.txt",
    ] {
        assert!(
            t.reask_1.contains(block),
            "the re-ask lost {block:?} when the order changed"
        );
    }
    assert!(
        t.reask_1.contains("--- schema.txt") && t.reask_1.contains("[RE-ASK]"),
        "the re-ask must still deliver the requested file and name it"
    );
    // Reordering is not reduction. These are the char totals this fixture sent
    // BEFORE the reorder; the two re-asks gained the single "\n\n" the new
    // order needs between the per-round block and the items that belong to one
    // turn only, and nothing else moved in or out.
    const ASK_CHARS_BEFORE: usize = 4_351;
    const REASK_CHARS_BEFORE: usize = 4_640;
    assert_eq!(
        t.ask_1.chars().count(),
        ASK_CHARS_BEFORE,
        "the per-turn context total changed: this build item moves content, \
         it does not cut it"
    );
    assert_eq!(
        t.ask_2.chars().count(),
        4_419,
        "round 2's ask total changed"
    );
    assert_eq!(
        t.reask_1.chars().count(),
        REASK_CHARS_BEFORE + 2,
        "the re-ask may only gain the separator its new position needs"
    );
    assert_eq!(
        t.reviewer_1.chars().count(),
        469,
        "the reviewer's turn is untouched by the implementer's reorder"
    );
    assert_eq!(t.reviewer_2.chars().count(), 593);
    assert_eq!(t.reask_2.chars().count(), 4_710, "round 2's re-ask total");
    std::fs::remove_dir_all(&root).ok();
}

// ---------------------------------------------------------------------------
// 4. The guard is not vacuous: a mis-ordered prompt is rejected.
// ---------------------------------------------------------------------------

#[test]
fn the_ordering_guard_rejects_a_prompt_with_dynamic_content_first() {
    // The shape the harness sent before the fix, byte for byte in structure:
    // the per-round counter and the feedback ABOVE the map and the file.
    let mis_ordered = "[LONG-TERM]\nmem\n\n[MID-TERM]\ngoal: g\nCURRENT TASK (1/1) : t\n\n\
                       [SHORT-TERM]\nWRITES REQUIRED: yes\nround 2/2: reviewer feedback: no\n\n\
                       [REPO FILES]\na.txt\n--- spec.txt\nstruct Thing {}";
    let found = misplaced_dynamic_content(mis_ordered, &["[REPO FILES]"])
        .expect("the guard must reject dynamic content above the stable prefix");
    assert!(
        found.contains("[REPO FILES]"),
        "and it must name what was misplaced: {found}"
    );
    assert!(
        misplaced_dynamic_content(mis_ordered, &["--- spec.txt"]).is_some(),
        "the goal-named file is misplaced for the same reason"
    );

    // The shape the harness sends now passes: the guard tests the ORDER, not
    // the content.
    let ordered = "[LONG-TERM]\nmem\n\n[MID-TERM]\ngoal: g\nCURRENT TASK (1/1) : t\n\n\
                   [REPO FILES]\na.txt\n--- spec.txt\nstruct Thing {}\n\n\
                   [SHORT-TERM]\nWRITES REQUIRED: yes\nround 2/2: reviewer feedback: no";
    assert_eq!(misplaced_dynamic_content(ordered, &["[REPO FILES]"]), None);
    assert_eq!(misplaced_dynamic_content(ordered, &["--- spec.txt"]), None);
    assert_eq!(misplaced_dynamic_content(ordered, &["goal:"]), None);
    // A prompt with no per-round block has nothing to be misplaced against.
    assert_eq!(
        misplaced_dynamic_content("[LONG-TERM]\nonly stable", &["x"]),
        None
    );
}

// ---------------------------------------------------------------------------
// 5. Build item 2's measurement still works, unchanged in shape.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn context_measured_still_matches_the_request_and_still_folds() {
    let (t, calls, events, root) = run("fold").await;
    let measured: Vec<(String, String, u64)> = events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::ContextMeasured {
                agent, turn, chars, ..
            } => Some((agent.clone(), turn.clone(), *chars)),
            _ => None,
        })
        .collect();
    assert_eq!(measured.len(), calls.len(), "one event per agent call");
    for (call, (agent, _, chars)) in calls.iter().zip(measured.iter()) {
        assert_eq!(&call.agent, agent);
        assert_eq!(
            *chars as usize,
            call.prompt.chars().count(),
            "the measurement is of the string that was sent, before and after \
             the reorder alike"
        );
    }
    let mut report = EvalReport::default();
    for e in &events {
        report.fold(e);
    }
    assert_eq!(report.context.turns, calls.len() as u64);
    assert_eq!(
        report.context.total_chars,
        t.ask_1.chars().count() as u64
            + t.reask_1.chars().count() as u64
            + t.ask_2.chars().count() as u64
            + t.reask_2.chars().count() as u64
            + t.reviewer_1.chars().count() as u64
            + t.reviewer_2.chars().count() as u64
    );
    assert!(report.context.chars_by_agent.contains_key("implementer"));
    assert!(report.context.chars_by_agent.contains_key("reviewer"));
    std::fs::remove_dir_all(&root).ok();
}
