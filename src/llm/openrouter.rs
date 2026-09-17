use super::{LlmClient, LlmError, LlmReq, LlmResp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Instant;

const DEFAULT_BASE: &str = "https://openrouter.ai/api/v1";

#[derive(Debug, Clone, Serialize)]
struct ChatMsg {
    role: String,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
struct ChatReq {
    model: String,
    messages: Vec<ChatMsg>,
    max_tokens: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    cost: Option<f64>,
    /// DeepSeek: tokens served from its prefix cache.
    #[serde(default)]
    prompt_cache_hit_tokens: Option<u64>,
    /// OpenRouter/OpenAI shape: nested cached-token detail.
    #[serde(default)]
    prompt_tokens_details: Option<PromptDetails>,
}

#[derive(Debug, Clone, Deserialize)]
struct PromptDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
}

/// A completion that hit the output ceiling is not a completion: the
/// caller sees an empty or partial answer and treats it as the model's
/// choice. DeepSeek's reasoning models spend the whole budget on hidden
/// reasoning and ship an empty `content` when `max_tokens` is too small, so
/// this is the difference between "the model wrote nothing" and "the model
/// was cut off".
const FINISH_LENGTH: &str = "length";

#[derive(Debug, Clone, Deserialize)]
struct Choice {
    message: ChoiceMsg,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChoiceMsg {
    #[serde(default, deserialize_with = "null_as_empty")]
    content: String,
}

fn null_as_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Deserialize)]
struct ChatResp {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

/// OpenRouter chat-completions client (works for Anthropic/OpenAI/xAI
/// models via OpenRouter model ids like "anthropic/claude-x").
/// Token comes from the `OR_TOKEN` env var (never logged).
pub struct OpenRouterClient {
    token: String,
    base: String,
    http: reqwest::Client,
}

impl OpenRouterClient {
    pub fn new(token: String) -> Self {
        Self {
            token,
            base: DEFAULT_BASE.to_string(),
            http: reqwest::Client::new(),
        }
    }

    pub fn from_env() -> Option<Self> {
        std::env::var("OR_TOKEN")
            .ok()
            .filter(|t| !t.is_empty())
            .map(Self::new)
    }

    /// Any OpenAI-compatible chat-completions endpoint (DeepSeek, NVIDIA,
    /// Qwen DashScope compat mode, ...). Neutral env names; map your
    /// provider key to ROF_TOKEN and the base URL to ROF_CHAT_BASE.
    pub fn from_compat_env() -> Option<Self> {
        let token = std::env::var("ROF_TOKEN")
            .ok()
            .filter(|t| !t.trim().is_empty())?;
        let base = std::env::var("ROF_CHAT_BASE")
            .ok()
            .filter(|t| !t.trim().is_empty())?;
        Some(Self {
            token,
            base,
            http: reqwest::Client::new(),
        })
    }

    /// The judge on a different provider than the executor (§4.5 arm #4).
    /// `ROF_VERIFY_TOKEN` + `ROF_VERIFY_BASE` name their own client so a
    /// self-review A/B can become an independent one without any other
    /// change; absent either, the caller falls back to the shared client and
    /// the run is identical to before.
    pub fn from_verify_env() -> Option<Self> {
        let token = std::env::var("ROF_VERIFY_TOKEN")
            .ok()
            .filter(|t| !t.trim().is_empty())?;
        let base = std::env::var("ROF_VERIFY_BASE")
            .ok()
            .filter(|t| !t.trim().is_empty())?;
        Some(Self {
            token,
            base,
            http: reqwest::Client::new(),
        })
    }

    fn body(model: &str, req: &LlmReq) -> ChatReq {
        ChatReq {
            model: model.to_string(),
            messages: vec![
                ChatMsg {
                    role: "system".to_string(),
                    content: req.system.clone(),
                },
                ChatMsg {
                    role: "user".to_string(),
                    content: req.prompt.clone(),
                },
            ],
            max_tokens: req.max_tokens,
        }
    }
}

#[async_trait]
impl LlmClient for OpenRouterClient {
    async fn complete(&self, model: &str, mut req: LlmReq) -> Result<LlmResp, LlmError> {
        let t = Instant::now();
        // ponytail: retry lives here, not in every caller. Free tiers 429 often.
        let mut last = LlmError::Transport("no attempts".to_string());
        for attempt in 0..4 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(2u64.pow(attempt))).await;
            }
            match self.once(model, &req).await {
                Ok(r) => {
                    return Ok(LlmResp {
                        latency_ms: t.elapsed().as_millis() as u64,
                        attempts: attempt as u64 + 1,
                        ..r
                    })
                }
                Err(e) => {
                    let text = e.to_string();
                    // A truncation is worth one immediate retry: the model's
                    // own reasoning budget varies per prompt, and a re-roll
                    // often finishes. It is not worth four slow retries.
                    let truncated = text.contains(FINISH_LENGTH);
                    let retryable = truncated
                        || ["429", "500", "502", "503", "504", "402"]
                            .iter()
                            .any(|c| text.contains(c));
                    last = e;
                    if !retryable || (truncated && attempt > 0) {
                        break;
                    }
                    if truncated {
                        // ponytail: the only retry worth making is a roomier
                        // one — a same-size re-roll just truncates again.
                        req.max_tokens = req.max_tokens.saturating_mul(2);
                    }
                }
            }
        }
        Err(last)
    }
}

impl OpenRouterClient {
    async fn once(&self, model: &str, req: &LlmReq) -> Result<LlmResp, LlmError> {
        let res = self
            .http
            .post(format!("{}/chat/completions", self.base))
            .bearer_auth(&self.token)
            .header("HTTP-Referer", "rof-harness")
            .header("X-Title", "rof-harness")
            .json(&Self::body(model, req))
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        if !res.status().is_success() {
            let code = res.status();
            let text = res.text().await.unwrap_or_default();
            let short: String = text.chars().take(300).collect();
            return Err(LlmError::Transport(format!("{code}: {short}")));
        }
        let body: ChatResp = res
            .json()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        let choice = body.choices.into_iter().next();
        // A truncation is reported as an error, not as an empty answer: the
        // retry loop above then re-asks, and a caller that counts model
        // errors sees a cutoff instead of "the model wrote nothing".
        let truncated =
            choice.as_ref().and_then(|c| c.finish_reason.as_deref()) == Some(FINISH_LENGTH);
        let text = choice.map(|c| c.message.content).unwrap_or_default();
        if truncated {
            return Err(LlmError::Transport(format!(
                "output truncated at {} tokens (finish_reason=length); raise max_tokens",
                req.max_tokens
            )));
        }
        let (inp, out, cost, cached) = body
            .usage
            .map(|u| {
                let cached = u
                    .prompt_cache_hit_tokens
                    .or_else(|| u.prompt_tokens_details.and_then(|d| d.cached_tokens))
                    .unwrap_or(0);
                (u.prompt_tokens, u.completion_tokens, u.cost, cached)
            })
            .unwrap_or((0, 0, None, 0));
        Ok(LlmResp {
            text,
            input_tokens: inp,
            output_tokens: out,
            latency_ms: 0, // set by complete(), which owns the timer
            cost_usd: cost,
            cached_input_tokens: cached,
            attempts: 1, // overwritten by complete() with the real attempt count
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_shape() {
        let req = LlmReq {
            system: "s".into(),
            prompt: "p".into(),
            max_tokens: 10,
        };
        let b = OpenRouterClient::body("anthropic/claude-x", &req);
        assert_eq!(b.model, "anthropic/claude-x");
        assert_eq!(b.messages.len(), 2);
        assert_eq!(b.messages[0].role, "system");
    }

    /// §4.5 arm #4: the judge gets its own client only when both the token
    /// and the base are present, so the run is unchanged when the slot is not
    /// in use. Whitespace-only is absent, matching the other env parsers.
    #[test]
    fn verify_env_requires_token_and_base() {
        std::env::remove_var("ROF_VERIFY_TOKEN");
        std::env::remove_var("ROF_VERIFY_BASE");
        assert!(OpenRouterClient::from_verify_env().is_none());

        std::env::set_var("ROF_VERIFY_TOKEN", "   ");
        std::env::set_var("ROF_VERIFY_BASE", "https://judge.example/v1");
        assert!(
            OpenRouterClient::from_verify_env().is_none(),
            "a whitespace-only token must not build a client"
        );

        std::env::set_var("ROF_VERIFY_TOKEN", "secret");
        let j = OpenRouterClient::from_verify_env().expect("token + base builds the judge client");
        assert_eq!(j.token, "secret");
        assert_eq!(j.base, "https://judge.example/v1");
        assert_ne!(
            j.base, DEFAULT_BASE,
            "the judge is not the default endpoint"
        );

        std::env::remove_var("ROF_VERIFY_TOKEN");
        std::env::remove_var("ROF_VERIFY_BASE");
    }
}
