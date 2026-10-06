use async_trait::async_trait;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tool_core::{
    CallStatus, Invocation, Tool, ToolCall, ToolDefinition, ToolError, ToolOutcome, TOOL_TEST,
};

use crate::common::{dispatch, parse_args, EXEC_TIMEOUT};
use crate::policy::Policy;
use crate::runner::{cmd_schema, run_allowed, CmdArgs};

pub struct TestTool {
    policy: Arc<Policy>,
}

#[async_trait]
impl Tool for TestTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: TOOL_TEST.to_string(),
            description: "run an allowlisted check, report PASS/FAIL verdict".to_string(),
            schema: cmd_schema(),
        }
    }
    fn prepare(&self, call: &ToolCall) -> CallStatus {
        dispatch(call)
    }
    async fn execute(
        &self,
        inv: Invocation,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let args: CmdArgs = parse_args(&inv.args)?;
        let (ok, out, truncated) = run_allowed(&self.policy, &args.cmd, EXEC_TIMEOUT).await?;
        // Cascade policy (what FAIL does to the loop) lives in loop/bets, not here.
        let content = if ok {
            format!("PASS: {}\n{out}", args.cmd)
        } else {
            format!("FAIL: {}\n{out}", args.cmd)
        };
        Ok(ToolOutcome {
            content,
            truncated,
            success: ok,
        })
    }
}

pub fn test_tool(policy: Arc<Policy>) -> TestTool {
    TestTool { policy }
}
