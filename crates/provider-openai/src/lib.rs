//! One OpenAI-compatible chat/completions adapter implementing
//! provider_core::LlmClient. Covers DeepSeek/Atria/OpenRouter/Ollama through
//! config (endpoint + key + model id). See research/crate-provider-core.md.
mod client;
mod pricing;
mod protocol;
mod retry;
#[cfg(test)]
mod test_support;

pub use client::{EndpointProfile, OpenAiCompat};
