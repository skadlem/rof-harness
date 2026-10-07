use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Thinking {
    Off,
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderMessage {
    pub role: String,
    pub content: String,
    /// Assistant tool calls to echo back (strict providers 422 without them).
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRef>,
    /// Tool-result linkage id (strict providers 422 without it).
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// Assistant thinking the endpoint demands echoed back (DeepSeek thinking
    /// mode 400s without `reasoning_content` on prior assistant messages).
    /// None = the provider sent none; never fabricated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub messages: Vec<ProviderMessage>,
    pub tools: Vec<tool_core::ToolDeclaration>,
    pub max_tokens: usize,
    pub thinking: Thinking,
    pub extras: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    Pending,
    Stop,
    ToolUse,
    MaxTokens,
    Refused,
    Error,
    Aborted,
    Deferred,
}

impl StopReason {
    /// Everything but `Pending` is terminal.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, StopReason::Pending)
    }
    /// Terminal-with-error. Refused counts; MaxTokens does not (it is a
    /// reshape-and-retry signal, not a failure). Deferred is type-only.
    pub fn is_error(&self) -> bool {
        matches!(
            self,
            StopReason::Refused | StopReason::Error | StopReason::Aborted
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Subset of output, not additional. None = provider doesn't report.
    pub reasoning: Option<u64>,
    pub cost_usd: Option<f64>,
}

impl Usage {
    /// Billed total. Reasoning is already inside `output`: never add it again.
    pub fn total_tokens(&self) -> u64 {
        self.input + self.output
    }
    /// Output minus reasoning (the visible content share). Saturates: a
    /// provider that over-reports reasoning yields 0, never underflow.
    pub fn content_tokens(&self) -> u64 {
        self.output
            .saturating_sub(self.reasoning.unwrap_or(0).min(self.output))
    }

    /// Sum of two independently billed requests (retry-ladder re-sends):
    /// token fields add, reasoning stays a subset of output, cost adds when
    /// either side knows it. Tokens and spend are never refunded.
    pub fn plus(&self, next: &Usage) -> Usage {
        let mut u = self.clone();
        u.input = u.input.saturating_add(next.input);
        u.output = u.output.saturating_add(next.output);
        u.cache_read = u.cache_read.saturating_add(next.cache_read);
        u.cache_write = u.cache_write.saturating_add(next.cache_write);
        if let Some(r) = next.reasoning {
            u.reasoning = Some(u.reasoning.unwrap_or(0).saturating_add(r));
        }
        if let Some(c) = next.cost_usd {
            u.cost_usd = Some(u.cost_usd.unwrap_or(0.0) + c);
        }
        u
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRef {
    pub id: String,
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub content: String,
    pub tool_calls: Vec<ToolCallRef>,
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub message: AssistantMessage,
    pub stop: StopReason,
    pub usage: Usage,
    pub latency_ms: u64,
    pub attempts: u64,
    pub raw_stop_reason: Option<String>,
    /// Usage the FAILED attempts of this same call were billed (retry-ladder
    /// re-sends a recovered final attempt does not carry). `None` = none.
    /// Internal field — NOT an OpenAI wire field (our accounting seam).
    /// `#[serde(default)]` keeps stored responses parsing.
    #[serde(default)]
    pub retry_usage: Option<Box<Usage>>,
}

impl Response {
    /// What this call actually billed: final attempt plus every failed
    /// re-send the ladder made. Metering must record THIS, not `usage`:
    /// a recovered ladder is still billed spend.
    pub fn billed_usage(&self) -> Usage {
        match &self.retry_usage {
            Some(extra) => extra.plus(&self.usage),
            None => self.usage.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Credentials {
    pub api_key: String,
}
/// Map a provider finish reason to `StopReason`. `None`/empty/unknown means
/// the provider sent nothing: infer from tool calls (non-empty -> ToolUse).
/// A length/max-tokens reason is surfaced as `MaxTokens` even when tool calls
/// are present — never coerced to `ToolUse`, or the loop would execute
/// truncated arguments.
pub fn infer_stop(raw: Option<&str>, message: &AssistantMessage) -> StopReason {
    let infer = || {
        if message.tool_calls.is_empty() {
            StopReason::Stop
        } else {
            StopReason::ToolUse
        }
    };
    let r = match raw.map(str::trim) {
        Some(s) if !s.is_empty() => s.to_lowercase(),
        _ => return infer(),
    };
    let n: String = r.chars().filter(|c| c.is_alphanumeric()).collect();
    match n.as_str() {
        "length" | "maxtokens" | "truncated" | "outputlimit" => StopReason::MaxTokens,
        s if s.contains("tool") || s.contains("function") => StopReason::ToolUse,
        s if s.contains("refus")
            || s.contains("contentfilter")
            || s.contains("safety")
            || s.contains("policy") =>
        {
            StopReason::Refused
        }
        s if s.contains("defer") => StopReason::Deferred,
        s if s.contains("abort") || s.contains("cancel") => StopReason::Aborted,
        s if s.contains("error") || s.contains("fail") => StopReason::Error,
        s if s.contains("pending") => StopReason::Pending,
        "stop" | "endturn" | "stopsequence" | "eos" | "done" | "complete" | "finished" => {
            StopReason::Stop
        }
        _ => infer(),
    }
}
