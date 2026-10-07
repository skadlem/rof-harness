use crate::errors::LlmError;
use crate::types::{Request, Response};
use async_trait::async_trait;

#[async_trait]
pub trait LlmClient: Send + Sync {
    /// One logical request. Retry, backoff, and reshaping live in the
    /// implementor. The default fails loudly instead of fabricating zeros
    /// (the budget meters from `Usage`; zeros would be unlimited free
    /// tokens). Adapters must override with a metered implementation.
    /// Cancellation is drop-based: dropping the returned future aborts the
    /// call. There is no cancel token parameter by design; callers race
    /// `complete` against their own cancellation and drop the loser.
    async fn complete(&self, _model: &str, _req: &Request) -> Result<Response, LlmError> {
        Err(LlmError::Transport(
            "default LlmClient::complete cannot report usage: override it with a metered implementation".into(),
        ))
    }
}
