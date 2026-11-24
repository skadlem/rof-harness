use async_trait::async_trait;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tool_core::{Invocation, Tool, ToolDefinition, ToolError, ToolOutcome, TOOL_TEST};

use crate::common::{parse_args, policy_wording, EXEC_TIMEOUT};
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
            description: policy_wording(
                &self.policy,
                "run an allowlisted check, parsed into argv without a shell. Pipes, redirection and command chaining do not work; stdin is closed. Quote arguments containing spaces. Report a PASS/FAIL verdict.",
            ),
            schema: cmd_schema(),
        }
    }
    async fn execute(
        &self,
        inv: Invocation,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let args: CmdArgs = parse_args(&inv.args)?;
        let (ok, out, truncated) =
            run_allowed(&self.policy, &args.cmd, EXEC_TIMEOUT, &cancel).await?;
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
