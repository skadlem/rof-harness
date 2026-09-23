pub mod explorer;
pub mod implementer;
pub mod planner;
pub mod reviewer;
use crate::context::CtxView;
use crate::llm::{ContextService, ExecutorService};
use crate::obs::TraceSink;
use crate::tools::ToolRegistry;
use async_trait::async_trait;
pub use explorer::{explorer_report_for_test, ExplorerAgent, ExplorerReport, KeyFile, Quote};
pub use implementer::ImplementerAgent;
pub use planner::{AgentOutput, Plan, PlannerAgent};
pub use reviewer::{ReviewerAgent, Verdict};
use std::path::Path;

/// Everything an agent may need. Agents only use what their role allows:
/// Planner -> context LLM; Implementer -> executor LLM + tools;
/// Reviewer -> executor LLM (read-only).
pub struct AgentCtx<'a> {
    pub view: &'a CtxView,
    pub context: Option<&'a ContextService>,
    pub executor: Option<&'a ExecutorService>,
    pub tools: Option<&'a ToolRegistry>,
    pub workdir: Option<&'a Path>,
    pub trace: &'a TraceSink,
    /// The volatile budget (§4.1): chars the assembler may spend on the parts
    /// below the layers. Owned by the orchestrator because the policy is
    /// config; spent by the agent, because the two-turn read flow interleaves
    /// assembly with model calls.
    pub volatile_budget: usize,
}

/// `ROF_THINKING` starting posture for agent calls, as
/// `(thinking_off, reasoning_off, reasoning_low)`.
///
/// The retry ladder escalates after a truncation, which cannot help a model
/// that never terminates reasoning (measured: 33k reasoning chars, 0 content).
/// `off` starts thought-off so there is no budget to burn; `low` starts
/// bounded. Anything else is current behavior. Read from the env at call time
/// (like the provider clients) so arms need no config file.
/// Shared by implementer and reviewer; one mapping, no duplicated logic.
pub(crate) fn thinking_start() -> (bool, bool, bool) {
    thinking_flags(
        &std::env::var("ROF_THINKING")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase(),
    )
}

fn thinking_flags(mode: &str) -> (bool, bool, bool) {
    match mode {
        "off" => (true, true, false),
        "low" => (false, false, true),
        _ => (false, false, false),
    }
}

/// Test helper: the mode mapping without touching the env.
pub fn thinking_flags_for_test(mode: &str) -> (bool, bool, bool) {
    thinking_flags(mode)
}

/// Test helper: the env-driven starting posture.
pub fn thinking_start_for_test() -> (bool, bool, bool) {
    thinking_start()
}

/// Per-call output cap from the env (`ROF_IMPLEMENTER_MAX_TOKENS` /
/// `ROF_REVIEWER_MAX_TOKENS`), clamped so a live arm cannot set an absurd
/// value. Unset or unparsable leaves the caller's default untouched.
pub fn max_tokens_from_env(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map(|v| v.clamp(1024, 32768))
        .unwrap_or(default)
}

#[async_trait]
pub trait Agent: Send + Sync {
    fn name(&self) -> &'static str;
    async fn run(&self, ctx: AgentCtx<'_>) -> anyhow::Result<AgentOutput>;
}
