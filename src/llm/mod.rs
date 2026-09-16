use async_trait::async_trait;
pub mod openrouter;
pub use openrouter::OpenRouterClient;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmReq {
    pub system: String,
    pub prompt: String,
    pub max_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResp {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub latency_ms: u64,
    /// Provider-reported spend when available (OpenRouter does; most don't).
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// Input tokens billed as cache hits (DeepSeek: prompt_cache_hit_tokens).
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// HTTP attempts made for this call (1 = first try succeeded).
    #[serde(default)]
    pub attempts: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("all models in fallback chain failed")]
    AllFailed,
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete(&self, model: &str, req: LlmReq) -> Result<LlmResp, LlmError>;
}

/// Context LLM: cheap model for planning / summarization / context optimization.
#[derive(Clone)]
pub struct ContextService {
    client: Arc<dyn LlmClient>,
    pub model: String,
}

impl ContextService {
    pub fn new(client: Arc<dyn LlmClient>, model: String) -> Self {
        Self { client, model }
    }
    pub async fn complete(&self, req: LlmReq) -> Result<LlmResp, LlmError> {
        self.client.complete(&self.model, req).await
    }

    /// Compress context that overflowed its budget — the cheap model's second
    /// duty per the two-tier design. Returns the full response so the caller
    /// can trace tokens/cost like any other model call.
    pub async fn summarize(&self, text: &str, max_tokens: usize) -> Result<LlmResp, LlmError> {
        self.complete(LlmReq {
            system: format!(
                "Compress the input to at most {max_tokens} tokens. Preserve exactly: file paths, \
                 command output, error text, decisions taken. Drop: pleasantries, restated goals, \
                 repeated context. Output only the compressed text."
            ),
            prompt: text.to_string(),
            max_tokens,
        })
        .await
    }
}

/// Executor LLM: strong model for tool-using actions + final review,
/// with optional fallback model.
#[derive(Clone)]
pub struct ExecutorService {
    client: Arc<dyn LlmClient>,
    pub model: String,
    pub fallback: Option<String>,
}

impl ExecutorService {
    pub fn new(client: Arc<dyn LlmClient>, model: String, fallback: Option<String>) -> Self {
        Self {
            client,
            model,
            fallback,
        }
    }
    pub async fn complete(&self, req: LlmReq) -> Result<LlmResp, LlmError> {
        match self.client.complete(&self.model, req.clone()).await {
            Ok(r) => Ok(r),
            Err(_) => match &self.fallback {
                Some(fb) => self.client.complete(fb, req).await,
                None => Err(LlmError::AllFailed),
            },
        }
    }
}

/// Deterministic stub for offline testing of the harness flow.
/// Dispatches on the system prompt so planner/implementer/reviewer each
/// get a well-formed reply and the full loop passes offline.
pub struct StubClient;

#[async_trait]
impl LlmClient for StubClient {
    async fn complete(&self, _model: &str, req: LlmReq) -> Result<LlmResp, LlmError> {
        let t = Instant::now();
        let text = if req.system.contains("reviewer") {
            "{\"pass\": true, \"feedback\": \"stub: artifact matches plan\"}".to_string()
        } else if req.system.contains("implementer") {
            "{\"artifact\": \"stub implementation\", \"notes\": \"offline demo\"}".to_string()
        } else {
            "{\"tasks\": [\"analyze goal\", \"implement\", \"verify\"], \"acceptance\": [\"cargo test passes\"]}".to_string()
        };
        Ok(LlmResp {
            text,
            input_tokens: req.prompt.len() as u64 / 4,
            output_tokens: 60,
            latency_ms: t.elapsed().as_millis() as u64,
            cost_usd: None,
            cached_input_tokens: 0,
            attempts: 1,
        })
    }
}

/// Strict JSON parse first; else scan for the first {...} block (weak
/// models wrap JSON in prose). Brace-match so nested objects survive.
pub fn parse_lenient(text: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str(text) {
        return Some(v);
    }
    let start = text.find('{')?;
    let mut depth = 0;
    for (i, c) in text[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&text[start..start + i + 1]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_lenient;

    #[test]
    fn extracts_json_from_prose() {
        let v = parse_lenient("Sure! Here it is:\n{\"pass\": true, \"feedback\": \"ok\"}\nDone.")
            .unwrap();
        assert_eq!(v["pass"], true);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_lenient("no braces here").is_none());
        assert!(parse_lenient("{\"pass\": tru").is_none());
    }
}

// OpenRouterClient lives in openrouter.rs, wired in main.rs via OR_TOKEN.
