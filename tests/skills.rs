//! Stage 1 acceptance: a skill gets created (as a proposal), approved, and
//! then used by a later task — plus the gate cases that keep an agent from
//! editing its own instructions unattended.

use async_trait::async_trait;
use rof::config::AppConfig;
use rof::eval::{EvalSuite, EvalTask, EvaluationRunner};
use rof::llm::{ContextService, ExecutorService, LlmClient, LlmError, LlmReq, LlmResp};
use rof::obs::{TraceEvent, TraceSink};
use rof::skills::{SkillError, SkillManager, SkillOp, SkillPolicy};
use rof::tools::{ToolError, ToolRegistry};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A scratch base dir, a workdir to run tasks in, and a skills store — the
/// store lives *outside* the workdir, as `~/.rof/skills` does.
struct Env {
    base: PathBuf,
    work: PathBuf,
    skills: PathBuf,
}

fn env(tag: &str) -> Env {
    let base = std::env::temp_dir().join(format!("rof-skill-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let work = base.join("work");
    let skills = base.join("skills-store");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("notes.md"), "eval target content").unwrap();
    Env { base, work, skills }
}

impl Env {
    fn manager(&self, policy: SkillPolicy) -> SkillManager {
        SkillManager::new(self.skills.clone(), None, policy)
    }
}

/// Answers per role; implementer replies are consumed in order (a request for
/// a body first, then the write), planner and reviewer are fixed.
struct SkillFake {
    imp_replies: Mutex<Vec<String>>,
    prompts: Mutex<Vec<String>>,
}

impl SkillFake {
    fn new(imp_replies: Vec<serde_json::Value>) -> Self {
        Self {
            imp_replies: Mutex::new(imp_replies.into_iter().map(|v| v.to_string()).collect()),
            prompts: Mutex::new(Vec::new()),
        }
    }
    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
    fn wrote(&self, needle: &str) -> bool {
        self.prompts().iter().any(|p| p.contains(needle))
    }
}

#[async_trait]
impl LlmClient for SkillFake {
    async fn complete(&self, _model: &str, req: LlmReq) -> Result<LlmResp, LlmError> {
        self.prompts.lock().unwrap().push(req.prompt.clone());
        let text = if req.system.contains("reviewer") {
            "{\"pass\": true, \"feedback\": \"ok\"}".to_string()
        } else if req.system.contains("implementer") {
            let mut q = self.imp_replies.lock().unwrap();
            if q.len() > 1 {
                q.remove(0)
            } else {
                q.first().cloned().unwrap_or_else(|| "{}".to_string())
            }
        } else {
            "{\"tasks\": [\"t\"], \"acceptance\": [\"a\"]}".to_string()
        };
        Ok(LlmResp {
            text,
            input_tokens: 4,
            output_tokens: 2,
            latency_ms: 1,
            cost_usd: None,
            cached_input_tokens: 0,
            attempts: 1,
        })
    }
}

fn suite(goal: &str) -> EvalSuite {
    EvalSuite {
        name: "skills".to_string(),
        tasks: vec![EvalTask {
            name: "task".to_string(),
            goal: goal.to_string(),
            expect_pass: true,
            checks: Vec::new(),
            expect_writes: true,
            max_tokens: None,
        }],
    }
}

fn runner(e: &Env, client: Arc<dyn LlmClient>, policy: SkillPolicy) -> EvaluationRunner {
    runner_with(e, client, policy, "always")
}

fn runner_with(
    e: &Env,
    client: Arc<dyn LlmClient>,
    policy: SkillPolicy,
    planner: &str,
) -> EvaluationRunner {
    let mut cfg = AppConfig::default();
    cfg.skills.root = Some(e.skills.clone());
    cfg.skills.policy = policy;
    cfg.planner = planner.to_string();
    // Per-task copies stay inside this test's own base dir. With the default
    // (the shared system temp dir) every test in this binary writes copies
    // named `rof-task-<pid>-task-<uuid>` and one test's teardown can delete a
    // sibling's in-flight copy — the flake this file used to clean up after.
    cfg.task_root = Some(e.base.join("task-root"));
    EvaluationRunner::new(
        Arc::new(TraceSink::new()),
        cfg,
        ContextService::new(client.clone(), "fake-ctx".to_string()),
        ExecutorService::new(client, "fake-exec".to_string(), None),
        e.work.clone(),
    )
}

const LESSON: &str =
    "Put the test in the file's existing `#[cfg(test)] mod tests`; never add a second one.";

/// The acceptance path end to end: task 1 proposes a skill (nothing lands),
/// a human approves it, task 2 names it and gets the body in its prompt.
#[tokio::test]
async fn create_propose_approve_then_a_later_task_reuses_the_skill() {
    let e = env("flow");
    let proposal = serde_json::json!({
        "artifact": "wrote notes.md and learned something",
        "notes": "second `mod tests` broke the build last time",
        "writes": [{"path": "notes.md", "content": "updated"}],
        "skills": [{
            "op": "create",
            "name": "add-a-unit-test",
            "description": "Adding a unit test to an existing file in this repo",
            "body": LESSON,
            "rationale": "the same mistake cost a whole retry round",
        }],
    });
    let client = Arc::new(SkillFake::new(vec![proposal]));
    let rep = runner(&e, client.clone(), SkillPolicy::Propose)
        .run_suite(&suite("do the eval target work"))
        .await;
    assert!(rep.tasks[0].matched, "{:?}", rep.tasks[0]);
    // Proposed, not applied: the store is untouched and a proposal is waiting.
    assert_eq!(
        rep.aggregate.skills.proposed, 1,
        "{:?}",
        rep.aggregate.skills
    );
    assert_eq!(rep.aggregate.skills.applied, 0);
    assert!(!rep.tasks[0].feedback.is_empty() || rep.tasks[0].passed);
    assert!(!e.skills.join("add-a-unit-test").exists());
    let mgr = e.manager(SkillPolicy::Propose);
    let pending = mgr.proposals();
    assert_eq!(pending.len(), 1, "one proposal waiting: {pending:?}");
    assert_eq!(pending[0].agent, "implementer");
    assert!(
        pending[0].rationale.contains("retry round"),
        "the rationale travels with the proposal: {:?}",
        pending[0]
    );
    assert!(matches!(pending[0].op, SkillOp::Create { .. }));

    // The human half: approve.
    let (p, change) = mgr.approve(&pending[0].id).unwrap();
    assert_eq!(p.status, "applied");
    assert_eq!(change.name, "add-a-unit-test");
    let md = e.skills.join("add-a-unit-test/SKILL.md");
    assert!(md.is_file(), "approval writes the skill: {md:?}");
    assert!(mgr
        .index(10)
        .contains("add-a-unit-test: Adding a unit test"));
    assert!(
        mgr.proposals().is_empty(),
        "an applied proposal is not pending"
    );
    // Approving twice is a mistake, not a no-op.
    assert!(mgr.approve(&pending[0].id).is_err());

    // Task 2 names the skill: the body must reach the model's prompt, and the
    // run reports the reuse.
    let client2 = Arc::new(SkillFake::new(vec![serde_json::json!({
        "artifact": "x",
        "notes": "y",
        "writes": [{"path": "notes.md", "content": "again"}],
    })]));
    let rep2 = runner(&e, client2.clone(), SkillPolicy::Propose)
        .run_suite(&suite(
            "add tests the way the add-a-unit-test procedure says, to finish the eval target work",
        ))
        .await;
    assert!(rep2.tasks[0].matched, "{:?}", rep2.tasks[0]);
    assert_eq!(
        rep2.aggregate.skills.reused, 3,
        "planner, implementer and reviewer each got the body: {:?}",
        rep2.aggregate.skills
    );
    assert!(
        rep2.aggregate.skills.listed >= 1,
        "{:?}",
        rep2.aggregate.skills
    );
    assert_eq!(
        rep2.aggregate.skills.viewed, 0,
        "nothing was asked for by name"
    );
    assert!(
        client2.wrote(LESSON),
        "the recorded rule must be in a prompt: {:?}",
        client2.prompts()
    );
    assert!(client2.wrote("[SKILLS]"), "the index rides the stable head");

    // With the planner skipped there is no planner prompt to inject into, and
    // the count says so: two agents ran, two bodies were delivered.
    let client3 = Arc::new(SkillFake::new(vec![serde_json::json!({
        "artifact": "x",
        "notes": "y",
        "writes": [{"path": "notes.md", "content": "third"}],
    })]));
    let rep3 = runner_with(&e, client3, SkillPolicy::Propose, "skip")
        .run_suite(&suite(
            "add tests the way the add-a-unit-test procedure says, to finish the eval target work",
        ))
        .await;
    assert!(rep3.tasks[0].matched, "{:?}", rep3.tasks[0]);
    assert_eq!(
        rep3.aggregate.skills.reused, 2,
        "implementer + reviewer, no phantom planner reuse: {:?}",
        rep3.aggregate.skills
    );
    std::fs::remove_dir_all(&e.base).ok();
}

/// Progressive disclosure's other half: the model asks for a body it did not
/// get automatically, and the harness fetches it through the gate.
#[tokio::test]
async fn a_skill_view_request_is_honored_in_the_bounded_extra_turn() {
    let e = env("view");
    e.manager(SkillPolicy::Propose)
        .apply(&SkillOp::Create {
            name: "add-a-unit-test".to_string(),
            description: "Adding a unit test to an existing file".to_string(),
            body: LESSON.to_string(),
        })
        .unwrap();

    // First implementer turn: ask for the skill, propose nothing.
    // Second turn (after the extra one): write the file.
    let client = Arc::new(SkillFake::new(vec![
        serde_json::json!({ "artifact": "need the procedure", "skill_views": ["add-a-unit-test"] }),
        serde_json::json!({
            "artifact": "x",
            "notes": "y",
            "writes": [{"path": "notes.md", "content": "done"}],
        }),
    ]));
    let sink = Arc::new(TraceSink::new());
    let mut cfg = AppConfig::default();
    cfg.skills.root = Some(e.skills.clone());
    let rep = EvaluationRunner::new(
        sink.clone(),
        cfg,
        ContextService::new(client.clone(), "fake-ctx".to_string()),
        ExecutorService::new(client.clone(), "fake-exec".to_string(), None),
        e.work.clone(),
    )
    .run_suite(&suite("do the eval target work"))
    .await;
    assert!(rep.tasks[0].matched, "{:?}", rep.tasks[0]);
    assert_eq!(rep.aggregate.skills.viewed, 1, "{:?}", rep.aggregate.skills);
    assert_eq!(
        rep.aggregate.skills.reused, 0,
        "the goal does not name the skill, so nothing was injected"
    );
    assert!(
        client.wrote("[REQUESTED SKILLS]") && client.wrote(LESSON),
        "the asked-for body reaches the model: {:?}",
        client.prompts()
    );
    assert!(
        sink.events().iter().any(|ev| matches!(
            ev,
            TraceEvent::SkillOp { op, ok: true, .. } if op == "view"
        )),
        "a view event is emitted"
    );
    assert!(
        sink.events().iter().any(|ev| matches!(
            ev,
            TraceEvent::ToolCall { tool, .. } if tool == "skills.view"
        )),
        "a model-initiated read is a tool call too"
    );
    std::fs::remove_dir_all(&e.base).ok();
}

/// A bad op is refused where the agent can see why, with nothing written.
#[test]
fn a_malformed_op_is_refused_without_writing_anything() {
    let e = env("bad");
    let mgr = e.manager(SkillPolicy::Propose);
    for bad in [
        SkillOp::Create {
            name: "Bad Name".to_string(),
            description: "x".to_string(),
            body: String::new(),
        },
        SkillOp::Create {
            name: "no-description".to_string(),
            description: "  ".to_string(),
            body: String::new(),
        },
        SkillOp::Patch {
            name: "missing-skill".to_string(),
            find: "a".to_string(),
            replace: "b".to_string(),
        },
        SkillOp::WriteFile {
            name: "missing-skill".to_string(),
            rel: "../../escape.md".to_string(),
            content: "x".to_string(),
        },
    ] {
        assert!(mgr.manage(bad, "implementer", "").is_err());
    }
    assert!(mgr.proposals().is_empty(), "nothing was proposed");
    assert!(!e.skills.exists(), "nothing was created");
    std::fs::remove_dir_all(&e.base).ok();
}

/// Read-only policy refuses every manage op, including a valid one.
#[test]
fn readonly_policy_refuses_manage_but_still_reads() {
    let e = env("readonly");
    let direct = e.manager(SkillPolicy::Direct);
    direct
        .apply(&SkillOp::Create {
            name: "existing-skill".to_string(),
            description: "one line".to_string(),
            body: "body".to_string(),
        })
        .unwrap();
    let ro = e.manager(SkillPolicy::ReadOnly);
    assert!(ro.view("existing-skill", None).is_ok(), "reads still work");
    let err = ro
        .manage(
            SkillOp::Delete {
                name: "existing-skill".to_string(),
            },
            "implementer",
            "",
        )
        .unwrap_err();
    assert!(matches!(err, SkillError::Denied(_)), "{err:?}");
    assert!(e.skills.join("existing-skill/SKILL.md").is_file());
    std::fs::remove_dir_all(&e.base).ok();
}

/// Direct policy is the opt-in: the same op lands without a proposal.
#[test]
fn direct_policy_applies_ops_and_supports_the_whole_op_set() {
    let e = env("direct");
    let m = e.manager(SkillPolicy::Direct);
    let change = m
        .manage(
            SkillOp::Create {
                name: "add-a-unit-test".to_string(),
                description: "Adding a unit test to an existing file".to_string(),
                body: "first rule".to_string(),
            },
            "implementer",
            "",
        )
        .unwrap();
    assert_eq!(change.outcome, "applied");
    m.apply(&SkillOp::Patch {
        name: "add-a-unit-test".to_string(),
        find: "first rule".to_string(),
        replace: "second rule".to_string(),
    })
    .unwrap();
    m.apply(&SkillOp::WriteFile {
        name: "add-a-unit-test".to_string(),
        rel: "references/example.md".to_string(),
        content: "an example".to_string(),
    })
    .unwrap();
    let v = m.view("add-a-unit-test", None).unwrap();
    assert!(v.body.contains("second rule"), "{:?}", v.body);
    assert_eq!(
        v.files.len(),
        2,
        "SKILL.md + the support file: {:?}",
        v.files
    );
    let support = m
        .view("add-a-unit-test", Some("references/example.md"))
        .unwrap();
    assert_eq!(support.body, "an example");
    // A path that would leave the skill directory is refused, not written.
    assert!(m
        .apply(&SkillOp::WriteFile {
            name: "add-a-unit-test".to_string(),
            rel: "../escaped.md".to_string(),
            content: "nope".to_string(),
        })
        .is_err());
    assert!(!e.skills.join("escaped.md").exists());
    m.apply(&SkillOp::Delete {
        name: "add-a-unit-test".to_string(),
    })
    .unwrap();
    assert!(m.list().is_empty());
    std::fs::remove_dir_all(&e.base).ok();
}

/// A repo's own `./skills/` is readable and never writable.
#[test]
fn a_repo_skills_root_is_read_only_and_cannot_be_shadowed() {
    let e = env("repo-root");
    let repo = e.base.join("work/skills");
    for (dir, name, desc) in [
        (&e.skills, "shared-name", "the user's own"),
        (&repo, "shared-name", "the repo's"),
        (&repo, "repo-only", "only the repo has this"),
    ] {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {desc}\n---\nbody\n"),
        )
        .unwrap();
    }
    let m = SkillManager::new(e.skills.clone(), Some(repo.clone()), SkillPolicy::Direct);
    let scan = m.scan();
    let names: Vec<&str> = scan.skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["repo-only", "shared-name"], "{scan:?}");
    let shared = scan
        .skills
        .iter()
        .find(|s| s.name == "shared-name")
        .unwrap();
    assert_eq!(shared.description, "the user's own", "the user root wins");
    assert_eq!(shared.source, "user");
    // Editing a repo-only skill is refused rather than writing into a
    // checked-in tree behind the user's back.
    let err = m
        .manage(
            SkillOp::Patch {
                name: "repo-only".to_string(),
                find: "body".to_string(),
                replace: "changed".to_string(),
            },
            "implementer",
            "",
        )
        .unwrap_err();
    assert!(matches!(err, SkillError::Denied(_)), "{err:?}");
    assert!(std::fs::read_to_string(repo.join("repo-only/SKILL.md"))
        .unwrap()
        .contains("body"));
    std::fs::remove_dir_all(&e.base).ok();
}

/// The gate: the grant matrix decides who may touch the store, and the skills
/// root is not reachable through the file tools.
#[tokio::test]
async fn the_gate_decides_who_reaches_the_skill_store() {
    let e = env("gate");
    let mut cfg = AppConfig::default();
    cfg.skills.root = Some(e.skills.clone());
    let reg =
        ToolRegistry::with_defaults(e.work.clone(), cfg.permissions.clone(), cfg.skills.clone());
    let create = serde_json::json!({
        "op": "create",
        "name": "gated-skill",
        "description": "created through the gate",
        "body": "body",
        "agent": "implementer",
        "rationale": "test",
    });

    // implementer may propose; the others may not manage at all.
    let (r, _) = reg
        .call("implementer", "skills.manage", None, create.clone())
        .await;
    assert!(
        r.unwrap().output.contains("proposed"),
        "implementer may propose"
    );
    for agent in ["planner", "reviewer"] {
        let (r, _) = reg.call(agent, "skills.manage", None, create.clone()).await;
        assert!(r.is_err(), "{agent} must not manage skills");
    }
    // list grants, and a view that fails on a missing skill rather than on
    // policy (the grant is real).
    for agent in ["planner", "reviewer", "implementer"] {
        let (r, _) = reg
            .call(agent, "skills.list", None, serde_json::json!({}))
            .await;
        assert!(r.is_ok(), "{agent} may list");
    }
    let (r, _) = reg
        .call(
            "reviewer",
            "skills.view",
            None,
            serde_json::json!({"name": "nope"}),
        )
        .await;
    assert!(
        matches!(r, Err(ToolError::Failed(_))),
        "the reviewer's view is denied by the missing skill, not by the gate"
    );
    // The skills root is deliberately NOT on the file-tool allowlist: fs.write
    // must not become a way around the skills write policy.
    let (r, _) = reg
        .call(
            "implementer",
            "fs.write",
            Some(&e.skills.join("gated-skill/SKILL.md")),
            serde_json::json!({"path": "SKILL.md", "content": "hijacked"}),
        )
        .await;
    assert!(r.is_err(), "fs.write must not reach the skill store");
    let (r, _) = reg
        .call(
            "planner",
            "fs.read",
            Some(&e.skills.join("anything.md")),
            serde_json::json!({"path": "anything.md"}),
        )
        .await;
    assert!(r.is_err(), "fs.read must not reach the skill store either");
    assert!(!e.skills.exists(), "nothing was written by any of this");
    std::fs::remove_dir_all(&e.base).ok();
}

/// The stage's instrument: three tasks that share one procedure. A suite that
/// cannot be verified cannot answer whether a skill was reused.
#[test]
fn the_skill_suite_loads_and_shares_one_procedure() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/suites/skill-tasks.json");
    let s = EvalSuite::load(&path).unwrap();
    assert_eq!(s.name, "skill-tasks");
    assert_eq!(s.tasks.len(), 3);
    for t in &s.tasks {
        assert!(t.expect_pass && t.expect_writes, "{t:?}");
        assert_eq!(t.checks, vec!["cargo test".to_string()], "{t:?}");
    }
}

/// Skill metrics are folded from the trace, and only successes count.
#[test]
fn skill_metrics_count_successful_ops_only() {
    let mut r = rof::eval::EvalReport::default();
    let ev = |op: &str, ok: bool| TraceEvent::SkillOp {
        agent: "implementer".to_string(),
        op: op.to_string(),
        name: "some-skill".to_string(),
        ok,
        bytes: 10,
    };
    r.fold(&ev("list", true));
    r.fold(&ev("view", true));
    r.fold(&ev("reuse", true));
    r.fold(&ev("propose", true));
    r.fold(&ev("apply", true));
    r.fold(&ev("propose", false));
    assert_eq!(r.skills.listed, 1);
    assert_eq!(r.skills.viewed, 1);
    assert_eq!(r.skills.reused, 1);
    assert_eq!(r.skills.proposed, 1, "a refused op is not work done");
    assert_eq!(r.skills.applied, 1);
}
