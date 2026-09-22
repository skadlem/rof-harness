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

#[async_trait]
pub trait Agent: Send + Sync {
    fn name(&self) -> &'static str;
    async fn run(&self, ctx: AgentCtx<'_>) -> anyhow::Result<AgentOutput>;
}
