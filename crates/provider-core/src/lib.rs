//! LLM boundary vocabulary: shared request/response/error types.
mod client;
mod errors;
mod json;
mod types;

pub use client::LlmClient;
pub use errors::{classify_error, parse_retry_after, ErrorClass, LlmError};
pub use json::parse_streaming_json;
pub use types::{
    infer_stop, AssistantMessage, Credentials, ProviderMessage, Request, Response, StopReason,
    Thinking, ToolCallRef, Usage,
};

#[cfg(test)]
mod tests;
