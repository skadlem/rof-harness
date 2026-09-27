//! §6 / P3 build item 2: context-per-turn, the measured competitive axis.
//!
//! Two harnesses ran the same model at the same thinking effort and tied on
//! quality while one spent >2x the cost per task. The cause was how much
//! context each harness fed the model on each turn. Until this number exists
//! for rof, the cut list is guesswork — so everything here is about measuring,
//! not cutting.
//!
//! What the number has to be: what the model ACTUALLY received for one call.
//! For the reviewer that is the `CtxView` the orchestrator built. For the
//! implementer it is the assembler's `parts.full()` — the layered head PLUS
//! the volatile tail the §4.1 assembler places below it — and there are TWO
//! such turns per round, because a model that answers `{"reads": [...]}` is
//! re-asked with the same context plus the files it asked for. A measurement
//! taken at the orchestrator would understate the implementer by exactly the
//! part that grows, so it is taken at each ask instead.
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
use std::sync::{Arc, Mutex};

/// One model call as the fake client saw it. The prompt recorded here IS the
/// ground truth for "what the model actually received" — the measurement is
/// asserted against it rather than against a re-derivation.
#[derive(Debug, Clone)]
struct Call {
    agent: String,
    system: String,
    prompt: String,
}

/// Canned answers by role keyword in the system prompt. `read_then_write`
/// adds the `reads` re-ask turn, so one round measures three turns instead of
/// two.
struct Fake {
    read_then_write: bool,
    calls: Mutex<Vec<Call>>,
}

impl Fake {
    fn pass() -> Arc<Self> {
        Arc::new(Self {
            read_then_write: false,
            calls: Mutex::new(Vec::new()),
        })
    }

    /// Round 1 asks for `schema.txt`; the re-ask turn then writes. The
    /// re-ask is a real turn the model pays for, so it is measured separately.
    fn read_then_write() -> Arc<Self> {
        Arc::new(Self {
            read_then_write: true,
            calls: Mutex::new(Vec::new()),
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
        } else if req.system.contains("direct coding agent") {
            "executor"
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
            return Self::resp("{\"pass\": true, \"feedback\": \"looks good\"}");
        }
        if self.read_then_write && !req.prompt.contains("--- schema.txt") {
            return Self::resp("{\"reads\":[\"schema.txt\"],\"artifact\":\"need the body\"}");
        }
        Self::resp(
            "{\"patches\":[{\"path\":\"a.txt\",\"search\":\"beta_real_line\",\"replace\":\"beta_fixed\"}],\"notes\":\"ok\"}",
        )
    }
}

/// One temp tree + registry + orchestrator, with the config open for tweaks.
/// The skills root points inside the temp dir so a machine with a populated
/// `~/.rof/skills` cannot change what a prompt contains.
fn harness(
    client: Arc<Fake>,
    tag: &str,
    tweak: impl FnOnce(&mut AppConfig),
) -> (Orchestrator, ToolRegistry, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("rof-cpt-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), "alpha\nbeta_real_line\ngamma\n").unwrap();
    // ~9.4k chars, under the 12k emission ceiling, with a marker in the middle
    // so "arrived whole" and "was windowed" are distinguishable.
    let mut big = String::new();
    for _ in 0..230 {
        big.push_str("padding line that fills the window budget\n");
    }
    let mid = big.len() / 2;
    big.insert_str(mid, "MIDDLE_MARKER_LINE\n");
    std::fs::write(root.join("schema.txt"), big).unwrap();

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

    let mut cfg = AppConfig {
        max_review_rounds: 2,
        skills: rof::config::SkillsConfig {
            root: Some(root.join("no-skills")),
            ..Default::default()
        },
        ..Default::default()
    };
    tweak(&mut cfg);
    let context = ContextService::new(client.clone(), "fake-ctx".to_string());
    let executor = ExecutorService::new(client.clone(), "fake-exec".to_string(), None);
    let verify = ExecutorService::new(client, "fake-verify".to_string(), None);
    (
        Orchestrator::new(cfg, Arc::new(TraceSink::new()), context, executor, verify),
        reg,
        root,
    )
}

/// One `ContextMeasured` event per measured turn, in emission order.
fn measurements(events: &[TraceEvent]) -> Vec<(String, String, u64)> {
    events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::ContextMeasured {
                agent, turn, chars, ..
            } => Some((agent.clone(), turn.clone(), *chars)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_run_measures_the_exact_context_each_turn_received() {
    // The measured number must equal the prompt the client actually received,
    // to the character. Not "greater than zero": an estimate that happened to
    // be positive would pass such a test, and an estimate is the thing this
    // metric exists to replace.
    let client = Fake::pass();
    let (orch, reg, root) = harness(client.clone(), "exact", |_| {});
    let out = orch.run_loop(&Session::new("g".into()), &reg, &root).await;
    assert_eq!(out["passed"], true, "the fixture must be a passing run");

    let measured = measurements(&orch.trace().events());
    assert_eq!(measured.len(), 2, "one per agent call: {measured:?}");

    let calls = client.calls();
    assert_eq!(calls.len(), 2, "the fixture makes one call per agent");
    for (call, (agent, _, chars)) in calls.iter().zip(measured.iter()) {
        assert_eq!(&call.agent, agent);
        assert_eq!(
            *chars as usize,
            call.prompt.chars().count(),
            "the {agent} measurement is not the prompt that was sent"
        );
        // The implementer's number is measured on the ASSEMBLED prompt, head
        // plus volatile tail. An orchestrator-side number would miss the tail
        // below, which is the part that grows; this asserts the tail is in.
        assert_eq!(
            call.prompt.contains("[REPO FILES]"),
            call.agent == "implementer",
            "the volatile tail belongs to the implementer's turn only"
        );
    }
    // The two agents are not the same number: one figure for the run would be
    // the averaging this metric must not do.
    assert_ne!(measured[0].2, measured[1].2, "{measured:?}");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn the_reads_reask_is_measured_as_its_own_turn() {
    // A re-ask re-sends the WHOLE assembled context plus the files the model
    // asked for. That is the turn "3x less context per turn" is about, so it
    // is a separate measurement and not folded into the first ask.
    let client = Fake::read_then_write();
    let (orch, reg, root) = harness(client.clone(), "reask", |_| {});
    orch.run_loop(&Session::new("g".into()), &reg, &root).await;

    let measured = measurements(&orch.trace().events());
    assert_eq!(measured.len(), 3, "ask + re-ask + reviewer: {measured:?}");
    let impl_turns: Vec<_> = measured
        .iter()
        .filter(|(agent, _, _)| agent == "implementer")
        .collect();
    assert_eq!(impl_turns.len(), 2, "both implementer turns are measured");
    assert_ne!(impl_turns[0].1, impl_turns[1].1, "two labelled turns");
    assert!(
        impl_turns[1].2 > impl_turns[0].2,
        "the re-ask carries the context PLUS the requested file: {:?}",
        impl_turns
    );

    // Ground truth for the re-ask, and the finding: the goal text and the
    // file map are re-sent in full, so the turn is not incremental.
    let reask = client
        .calls()
        .into_iter()
        .find(|c| c.prompt.contains("--- schema.txt"))
        .expect("the re-ask turn");
    assert_eq!(
        impl_turns[1].2 as usize,
        reask.prompt.chars().count(),
        "the re-ask measurement is not the prompt that was sent"
    );
    assert!(
        reask.prompt.contains("[REPO FILES]") && reask.prompt.contains("goal: g"),
        "the re-ask re-sends the whole context, it is not incremental"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn implementer_and_reviewer_are_reported_separately() {
    let client = Fake::pass();
    let (orch, reg, root) = harness(client.clone(), "per-agent", |_| {});
    orch.run_loop(&Session::new("g".into()), &reg, &root).await;
    let events = orch.trace().events();

    let mut report = EvalReport::default();
    for ev in &events {
        report.fold(ev);
    }
    let ctx = &report.context;
    assert_eq!(ctx.turns, 2, "one measured turn per agent call");
    let impl_chars = *ctx.chars_by_agent.get("implementer").expect("implementer");
    let rev_chars = *ctx.chars_by_agent.get("reviewer").expect("reviewer");
    assert_ne!(impl_chars, rev_chars, "the agents are not one number");
    assert_eq!(ctx.chars_by_agent.len(), 2, "only the two agents ran");
    assert_eq!(
        ctx.total_chars,
        impl_chars + rev_chars,
        "the total is the sum of the per-agent numbers, not a re-measurement"
    );
    // The per-agent figures must survive the fold, so a report can attribute
    // the bulk of the context to the agent that actually received it.
    let calls = client.calls();
    for (call, chars) in [("implementer", impl_chars), ("reviewer", rev_chars)] {
        let prompt = calls
            .iter()
            .find(|c| c.agent == call)
            .expect("the agent ran")
            .prompt
            .chars()
            .count() as u64;
        assert_eq!(chars, prompt, "{call} attribution is wrong");
    }
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn the_eval_fold_totals_context_per_turn_next_to_tokens() {
    // The whole point of the metric: a future arm must be able to read
    // context-per-turn and tokens-per-task off the same report and compare
    // them (the ratified cost-adjusted rule). Folding only one of the two
    // would leave nothing to compare.
    let mut r = EvalReport::default();
    r.fold(&TraceEvent::ModelCall {
        agent: "implementer".into(),
        model: "m".into(),
        input_tokens: 1_000,
        output_tokens: 200,
        latency_ms: 10,
        cost_usd: None,
        cached_input_tokens: 0,
        attempts: 1,
    });
    r.fold(&TraceEvent::ContextMeasured {
        agent: "implementer".into(),
        turn: "ask".into(),
        chars: 4_000,
        est_tokens: 1_000,
    });
    r.fold(&TraceEvent::ContextMeasured {
        agent: "implementer".into(),
        turn: "re-ask".into(),
        chars: 2_000,
        est_tokens: 500,
    });
    r.fold(&TraceEvent::ContextMeasured {
        agent: "reviewer".into(),
        turn: "call".into(),
        chars: 1_000,
        est_tokens: 250,
    });

    assert_eq!(r.est_input_tokens, 1_000, "tokens are untouched");
    assert_eq!(r.context.turns, 3);
    assert_eq!(r.context.total_chars, 7_000);
    assert!((r.context.mean_chars() - 7_000.0 / 3.0).abs() < 1e-9);
    assert_eq!(r.context.chars_by_agent.get("implementer"), Some(&6_000));
    assert_eq!(r.context.chars_by_agent.get("reviewer"), Some(&1_000));
    assert_eq!(r.context.turns_by_agent.get("implementer"), Some(&2));
    // Nothing measured, nothing claimed.
    assert_eq!(EvalReport::default().context.mean_chars(), 0.0);
}

#[tokio::test]
async fn the_default_cap_leaves_the_run_byte_identical() {
    // This task is measurement only. The knob is off by default, and a run
    // with it off must produce exactly the bytes a run produced before the
    // knob existed: same prompts, same trace apart from the new event.
    let default_client = Fake::pass();
    let (orch, reg, root) = harness(default_client.clone(), "cap-off", |_| {});
    let out = orch.run_loop(&Session::new("g".into()), &reg, &root).await;
    let default_events = orch.trace().events();
    let default_calls = default_client.calls();

    // A cap set far above anything a turn can reach must also change nothing:
    // the knob is inert unless it binds, so a "default" that merely happened
    // to match a small prompt would not be evidence of anything.
    let capped_client = Fake::pass();
    let (orch2, reg2, root2) = harness(capped_client.clone(), "cap-high", |cfg| {
        cfg.per_turn_context_cap = 1_000_000;
    });
    let out2 = orch2
        .run_loop(&Session::new("g".into()), &reg2, &root2)
        .await;
    let capped_calls = capped_client.calls();

    assert_eq!(out, out2, "the run result changed");
    assert_eq!(
        default_calls.iter().map(|c| &c.prompt).collect::<Vec<_>>(),
        capped_calls.iter().map(|c| &c.prompt).collect::<Vec<_>>(),
        "a cap that does not bind changed what the model was told"
    );
    let strip = |evs: &[TraceEvent]| -> Vec<String> {
        evs.iter()
            .filter(|e| !matches!(e, TraceEvent::ContextMeasured { .. }))
            .map(|e| {
                // The session id is a fresh uuid per run by construction; it
                // is the one field that cannot match across two runs and says
                // nothing about the trace's content.
                if matches!(e, TraceEvent::SessionStart { .. }) {
                    "session-start".to_string()
                } else {
                    serde_json::to_string(e).unwrap()
                }
            })
            .collect()
    };
    assert_eq!(
        strip(&default_events),
        strip(&orch2.trace().events()),
        "the instrumentation changed the trace beyond the new event"
    );

    // The knob itself: off by default, and absent from an old config file
    // without changing what that file means.
    assert_eq!(AppConfig::default().per_turn_context_cap, 0);
    let old: AppConfig = serde_json::from_str(
        "{\"budgets\":{\"long_term\":2000,\"mid_term\":4000,\"short_term\":6000},\
          \"max_review_rounds\":2,\"max_tokens_per_task\":50000,\"cost_lambda\":0.0}",
    )
    .expect("a pre-knob config must still load");
    assert_eq!(old.per_turn_context_cap, 0);
    assert_eq!(old.budgets.short_term, 6_000);

    // The default must be inert in the arithmetic too, not only in this
    // fixture: 0 means "no cap", and any positive cap larger than the
    // endpoint's own ceiling leaves the shipped budget exactly where it was.
    let profile = rof::llm::profile::EndpointProfile::default();
    let shipped = rof::engine::session::volatile_budget_for_profile(6_000, &profile, 0);
    assert_eq!(shipped, 12_000, "the default budget is unchanged");
    assert_eq!(
        rof::engine::session::volatile_budget_for_profile(6_000, &profile, 1_000_000),
        shipped
    );
    // A cap below the assembler's own narrowing floor is refused rather than
    // obeyed: at that width it could not deliver a thinner window, only stop
    // delivering the item.
    assert_eq!(
        rof::engine::session::volatile_budget_for_profile(6_000, &profile, 1),
        rof::context::assembler::MIN_WINDOW
    );
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&root2).ok();
}

#[tokio::test]
async fn the_event_round_trips_and_renders_one_sane_line() {
    // A new event kind that is silently swallowed by the transcript is a
    // measurement nobody can read, so the round trip and the one-line render
    // are part of the contract, not cosmetics.
    let client = Fake::pass();
    let (orch, reg, root) = harness(client, "render", |_| {});
    orch.run_loop(&Session::new("g".into()), &reg, &root).await;

    let event = orch
        .trace()
        .events()
        .into_iter()
        .find(|e| matches!(e, TraceEvent::ContextMeasured { .. }))
        .expect("a measured turn");
    let line = serde_json::to_string(&event).unwrap();
    let back: TraceEvent = serde_json::from_str(&line).expect("round trip");
    assert_eq!(
        serde_json::to_string(&back).unwrap(),
        line,
        "a replayed trace must read the same"
    );

    let rendered = rof::tui::render::render_line(&event);
    assert!(!rendered.contains('\n'), "one line: {rendered:?}");
    assert!(
        rendered.contains("implementer"),
        "the line must name the agent: {rendered:?}"
    );
    let chars = match &event {
        TraceEvent::ContextMeasured { chars, .. } => *chars,
        other => panic!("wrong event: {other:?}"),
    };
    assert!(
        rendered.contains(&chars.to_string()),
        "the line must carry the measured size: {rendered:?}"
    );
    assert!(
        rof::tui::render::parse_lenient_line(&line) == rendered,
        "the lenient reader must render it the same way the live view does"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn a_cap_that_binds_routes_through_the_existing_window() {
    // Requirement: a cap is enforced by the mechanism that already reduces
    // context, not by a new reduction algorithm. The §4.1 assembler already
    // windows a file that does not fit the volatile budget, so a cap that
    // binds must show up as exactly that: the same lever, narrower.
    let client = Fake::read_then_write();
    let (orch, reg, root) = harness(client.clone(), "cap-binds", |cfg| {
        // 2_000 tokens = 8_000 chars, below the ~9.4k fixture file.
        cfg.per_turn_context_cap = 2_000;
    });
    orch.run_loop(&Session::new("g".into()), &reg, &root).await;

    let capped = client
        .calls()
        .into_iter()
        .find(|c| c.prompt.contains("--- schema.txt"))
        .expect("the re-ask turn");
    assert!(
        !capped.prompt.contains("MIDDLE_MARKER_LINE"),
        "a file over the cap was shipped whole, so the cap did not bind"
    );
    assert!(
        capped.prompt.contains("...["),
        "the existing windowing lever must be what cut it, not a new filter: {}",
        capped.prompt.chars().take(200).collect::<String>()
    );
    // Ground truth: the measurement is of the reduced context, not of what
    // the budget would have allowed.
    let measured = measurements(&orch.trace().events());
    let reask = measured
        .iter()
        .find(|(agent, turn, _)| agent == "implementer" && turn != "ask")
        .expect("the re-ask turn is measured");
    assert_eq!(reask.2 as usize, capped.prompt.chars().count());
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn a_cap_never_drops_the_task_statement_or_the_system_prompt() {
    // The failure mode a cap must not have: a smaller context that no longer
    // says what the task is. The goal and the task line live in the layers
    // (above the assembler's budget) and the system prompt is a separate
    // request field, so neither is the assembler's to cut — and a cap routed
    // through the assembler must not reach either.
    let client = Fake::read_then_write();
    let (orch, reg, root) = harness(client.clone(), "cap-safe", |cfg| {
        cfg.per_turn_context_cap = 2_000;
    });
    orch.run_loop(&Session::new("fix the parser".into()), &reg, &root)
        .await;

    let calls = client.calls();
    assert!(!calls.is_empty());
    for call in &calls {
        assert!(
            call.prompt.contains("fix the parser"),
            "the goal/task statement was cut from a {} turn",
            call.agent
        );
        assert!(
            call.prompt.contains("CURRENT TASK"),
            "the task line was cut from a {} turn",
            call.agent
        );
        assert!(
            !call.system.is_empty(),
            "the system prompt must survive: {}",
            call.agent
        );
    }
    // ... and the system prompt is byte-identical to the uncapped run's, so a
    // cap is a context cut and nothing else.
    let plain = Fake::read_then_write();
    let (orch2, reg2, root2) = harness(plain.clone(), "cap-safe-off", |_| {});
    orch2
        .run_loop(&Session::new("fix the parser".into()), &reg2, &root2)
        .await;
    assert_eq!(
        calls.iter().map(|c| &c.system).collect::<Vec<_>>(),
        plain.calls().iter().map(|c| &c.system).collect::<Vec<_>>(),
        "a cap changed the system prompt"
    );
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&root2).ok();
}
