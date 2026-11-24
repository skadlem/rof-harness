use async_trait::async_trait;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tool_core::{Invocation, Tool, ToolDefinition, ToolError, ToolOutcome, TOOL_EXEC};

use crate::common::{parse_args, policy_wording, EXEC_TIMEOUT};
use crate::policy::Policy;
use crate::runner::{cmd_schema, run_allowed_raw, CmdArgs};

pub struct ExecTool {
    policy: Arc<Policy>,
}

#[async_trait]
impl Tool for ExecTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: TOOL_EXEC.to_string(),
            description: policy_wording(
                &self.policy,
                "run one allowlisted command, parsed into argv without a shell. Pipes, redirection and command chaining do not work; stdin is closed. Quote arguments containing spaces. For scripts, write a file under the tool root, then execute it.",
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
        let (ok, content, truncated) =
            run_allowed_raw(&self.policy, &args.cmd, EXEC_TIMEOUT, &cancel).await?;
        Ok(ToolOutcome {
            content,
            truncated,
            success: ok,
        })
    }
}

pub fn exec_tool(policy: Arc<Policy>) -> ExecTool {
    ExecTool { policy }
}
