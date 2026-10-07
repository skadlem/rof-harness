use async_trait::async_trait;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tool_core::{
    CallStatus, Invocation, Tool, ToolCall, ToolDefinition, ToolError, ToolOutcome, TOOL_EXEC,
};

use crate::common::{dispatch, parse_args, EXEC_TIMEOUT};
use crate::policy::Policy;
use crate::runner::{cmd_schema, run_allowed, CmdArgs};

pub struct ExecTool {
    policy: Arc<Policy>,
}

#[async_trait]
impl Tool for ExecTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: TOOL_EXEC.to_string(),
            description: "run an allowlisted command without a shell".to_string(),
            schema: cmd_schema(),
        }
    }
    fn prepare(&self, call: &ToolCall) -> CallStatus {
        dispatch(call)
    }
    async fn execute(
        &self,
        inv: Invocation,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let args: CmdArgs = parse_args(&inv.args)?;
        let (ok, content, truncated) =
            run_allowed(&self.policy, &args.cmd, EXEC_TIMEOUT, &cancel).await?;
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
