use super::{Agent, AgentCtx};
use crate::llm::LlmReq;
use crate::obs::TraceEvent;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub tasks: Vec<String>,
    pub acceptance: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOutput {
    pub summary: String,
    pub data: serde_json::Value,
}

/// Sample agent: Planner turns a vague goal + layered context into a
/// structured plan with acceptance criteria, using the cheap Context LLM.
/// No tools by policy.
pub struct PlannerAgent<'a> {
    llm: &'a crate::llm::ContextService,
}

impl<'a> PlannerAgent<'a> {
    pub fn new(llm: &'a crate::llm::ContextService) -> Self {
        Self { llm }
    }
}

#[async_trait]
impl Agent for PlannerAgent<'_> {
    fn name(&self) -> &'static str {
        "planner"
    }
    async fn run(&self, ctx: AgentCtx<'_>) -> anyhow::Result<AgentOutput> {
        let req = LlmReq {
            system: "You are a planner. Output JSON {tasks[], acceptance[]}.".to_string(),
            prompt: ctx.view.prompt.clone(),
            max_tokens: 4096,
            reasoning_off: false,
            reasoning_low: false,
            roomier: false,
        };
        let resp = self
            .llm
            .complete(req)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        ctx.trace.emit(TraceEvent::ModelCall {
            agent: self.name().to_string(),
            model: self.llm.model.clone(),
            input_tokens: resp.input_tokens,
            output_tokens: resp.output_tokens,
            latency_ms: resp.latency_ms,
            cost_usd: resp.cost_usd,
            cached_input_tokens: resp.cached_input_tokens,
            attempts: resp.attempts,
        });
        let data: serde_json::Value =
            crate::llm::parse_lenient(&resp.text).unwrap_or(serde_json::json!({"raw": resp.text}));
        let plan = Plan {
            tasks: data
                .get("tasks")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default(),
            acceptance: data
                .get("acceptance")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default(),
        };
        Ok(AgentOutput {
            summary: format!("{} tasks planned", plan.tasks.len()),
            data,
        })
    }
}
