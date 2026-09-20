use super::{Agent, AgentCtx, AgentOutput};
use crate::llm::LlmReq;
use crate::obs::TraceEvent;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub pass: bool,
    pub feedback: String,
}

/// Reviewer: checks the implementer's artifact against the plan's
/// acceptance criteria using the Executor LLM. Read-only; returns
/// pass/fail + feedback for the next round.
pub struct ReviewerAgent<'a> {
    llm: &'a crate::llm::ExecutorService,
}

impl<'a> ReviewerAgent<'a> {
    pub fn new(llm: &'a crate::llm::ExecutorService) -> Self {
        Self { llm }
    }
}

#[async_trait]
impl Agent for ReviewerAgent<'_> {
    fn name(&self) -> &'static str {
        "reviewer"
    }
    async fn run(&self, ctx: AgentCtx<'_>) -> anyhow::Result<AgentOutput> {
        let req = LlmReq {
            system: "You are a reviewer. Given PLAN and ARTIFACT, output JSON {pass: bool, feedback: string}. Rules: (1) if EXPECT WRITES is yes and WRITES MADE is 0, fail — prose is not a deliverable; (2) if CHECKS shows real command output, a failure there means fail; (3) if CHECKS says (none configured), judge the artifact itself and never fail merely for missing evidence. When the round taught a rule that generalizes beyond this task (a mistake to avoid, a procedure that worked), name it in feedback as `SKILL: <when to use it> — <the one rule>`; the implementer records those as skills a human approves."
                .to_string(),
            prompt: ctx.view.prompt.clone(),
            max_tokens: 4096,
            reasoning_off: false,
            reasoning_low: false,
            roomier: false,
            thinking_off: false,
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
        let v: serde_json::Value =
            crate::llm::parse_lenient(&resp.text).unwrap_or(serde_json::json!({"raw": resp.text}));
        let verdict = Verdict {
            pass: v.get("pass").and_then(|b| b.as_bool()).unwrap_or(false),
            feedback: v
                .get("feedback")
                .and_then(|s| s.as_str())
                .unwrap_or("unparseable verdict; treating as fail")
                .to_string(),
        };
        ctx.trace.emit(TraceEvent::ReviewVerdict {
            pass: verdict.pass,
            feedback: verdict.feedback.clone(),
        });
        Ok(AgentOutput {
            summary: format!("verdict: {}", if verdict.pass { "pass" } else { "fail" }),
            data: serde_json::to_value(&verdict).unwrap_or_default(),
        })
    }
}
