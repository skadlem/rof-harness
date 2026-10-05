//! One OpenAI-compatible chat/completions adapter implementing
//! provider_core::LlmClient. Covers DeepSeek/Atria/OpenRouter/Ollama through
//! config (endpoint + key + model id). See research/crate-provider-core.md.
use async_trait::async_trait;
use provider_core::{
    infer_stop, parse_retry_after, parse_streaming_json, Capabilities, Credentials, LlmClient,
    LlmError, Request, Response, StopReason, ToolCallRef, Usage,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Per-endpoint capability profile. Thresholds are re-measured per endpoint,
/// never transferred (v1 profile.rs lesson).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointProfile {
    pub emission_threshold_chars: usize,
    /// Exact JSON fragment merged into the request body to switch thinking
    /// off. DeepSeek's measured knob is `{"thinking":{"type":"disabled"}}`;
    /// `enable_thinking` and `reasoning_effort` are ignored there (live probe,
    /// api.deepseek.com deepseek-flash). None = no known knob: the ladder
    /// skips the rung instead of sending a placebo.
    #[serde(default)]
    pub thinking_off: Option<serde_json::Value>,
}

impl Default for EndpointProfile {
    fn default() -> Self {
        Self {
            emission_threshold_chars: 12_000,
            thinking_off: Some(serde_json::json!({"thinking": {"type": "disabled"}})),
        }
    }
}

pub struct OpenAiCompat {
    endpoint: String,
    profile: EndpointProfile,
    http: reqwest::Client,
    key_env: String,
    /// Extra per-request headers (e.g. opencode-go's `x-opencode-session`:
    /// a stable per-conversation id for routing + prompt caching).
    headers: Vec<(String, String)>,
}

impl OpenAiCompat {
    pub fn new(endpoint: &str, profile: EndpointProfile) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            profile,
            http: reqwest::Client::builder()
                .user_agent(format!("rof/{}", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            key_env: "OPENAI_API_KEY".to_string(),
            headers: Vec::new(),
        }
    }

    pub fn with_key_env(mut self, env: &str) -> Self {
        self.key_env = env.to_string();
        self
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn profile(&self) -> &EndpointProfile {
        &self.profile
    }
}

#[async_trait]
impl LlmClient for OpenAiCompat {
    async fn complete(&self, model: &str, req: &Request) -> Result<Response, LlmError> {
        let t = Instant::now();
        let mut knobs = WireKnobs::from_req(req, &self.profile);
        // ponytail: retry lives here, not in every caller. Free tiers 429 often.
        // Cold-start extension: 503s get up to 8 attempts with 60s sleeps
        // (~4 min, covers Modal scale-from-zero); other retryable errors keep
        // 5; truncated attempts follow the ladder until it has no rung left.
        // A server `Retry-After` above `RETRY_AFTER_CAP` is terminal instead,
        // and the computed 2^attempt backoff carries 0–25% jitter so locked-
        // step workers do not stampede (pi provider-retry.ts:44-58,66).
        let mut last_err = "no attempts".to_string();
        let mut last_after: Option<Duration> = None;
        // Billed attempts are metered: sum what each carried so the error
        // path never loses a re-send's usage. Only attempts that RETURNED
        // usage are billed (completed, or truncated then rejected);
        // pre-generation HTTP failures carry no usage and are not charged
        // (research/decision-audit-provider-economics.md C10).
        let mut usage_acc: Option<Usage> = None;
        let mut attempt = 0u32;
        loop {
            if attempt > 0 {
                let Some(d) = schedule_delay(last_after.take(), &last_err, attempt) else {
                    break;
                };
                tokio::time::sleep(d).await;
            }
            match self.once(model, req, &knobs).await {
                Ok(mut r) => {
                    r.latency_ms = t.elapsed().as_millis() as u64;
                    r.attempts = u64::from(attempt) + 1;
                    // Ladder-recovered attempts were billed too: carry what
                    // the failed re-sends cost so metering can count it.
                    r.retry_usage = usage_acc.take().map(Box::new);
                    return Ok(r);
                }
                Err(f) => {
                    last_err = f.msg.clone();
                    usage_acc = merge_usage(usage_acc, f.usage.map(|u| *u));
                    if !f.retryable {
                        break;
                    }
                    if f.truncated {
                        // Ladder rung first: an exhausted ladder stops instead
                        // of re-sending the identical shape for a 5th time.
                        if !apply_ladder(&mut knobs, f.content_chars, f.reasoning_chars) {
                            break;
                        }
                        last_after = f.after;
                    } else {
                        let cap = if is_cold_start(&f.msg) { 7 } else { 4 };
                        if attempt >= cap {
                            break;
                        }
                        last_after = f.after;
                    }
                }
            }
            attempt += 1;
        }
        Err(LlmError::Metered {
            source: last_err,
            usage: usage_acc,
        })
    }

    fn capabilities(&self, _model: &str) -> Capabilities {
        Capabilities {}
    }

    async fn resolve_key(&self, _provider: &str) -> Result<Credentials, LlmError> {
        // Per-call resolve, never cached: tokens expire mid-run.
        Ok(Credentials {
            api_key: std::env::var(&self.key_env).unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone)]
struct WireKnobs {
    max_tokens: usize,
    /// Endpoint's thinking-off fragment, if it has a real one.
    thinking_off: Option<serde_json::Value>,
    thinking_suppressed: bool,
}

impl WireKnobs {
    fn from_req(req: &Request, profile: &EndpointProfile) -> Self {
        // Thinking::Off applies the endpoint's real suppression knob up front;
        // the ladder then only has token-raising left for that call.
        let thinking_off = profile.thinking_off.clone();
        let thinking_suppressed =
            matches!(req.thinking, provider_core::Thinking::Off) && thinking_off.is_some();
        Self {
            max_tokens: req.max_tokens,
            thinking_off,
            thinking_suppressed,
        }
    }
}

/// Ceiling the overflow ladder may raise `max_tokens` to. Chosen (no anchor):
/// bounds one ladder's spend; the ladder unit test pins the bound.
const MAX_TOKENS_CEILING: usize = 32_768;

/// Cheapest-first reshape for one truncated (`finish_reason=length`) attempt.
/// Overflow needs room, never less of it: (1) thinking off — reasoning ate
/// the whole budget (content=0, reasoning>0) and the endpoint has a real
/// knob, applied once; (2) raise `max_tokens` — double, ceiling-bounded.
/// Returns whether a reshape applied; false ends the ladder.
fn apply_ladder(k: &mut WireKnobs, content_chars: usize, reasoning_chars: usize) -> bool {
    if !k.thinking_suppressed
        && content_chars == 0
        && reasoning_chars > 0
        && k.thinking_off.is_some()
    {
        k.thinking_suppressed = true;
        return true;
    }
    let raised = k.max_tokens.saturating_mul(2).min(MAX_TOKENS_CEILING);
    if raised > k.max_tokens {
        k.max_tokens = raised;
        return true;
    }
    false
}

fn wire_body(model: &str, req: &Request, k: &WireKnobs) -> serde_json::Value {
    // Thinking-mode conversation: any assistant message carried real
    // reasoning. The passback requirement ("reasoning_content ... must be
    // passed back"; 400 otherwise) is OFFICIALLY documented on DeepSeek's
    // Thinking Mode page. Two behaviors below are EMPIRICAL workarounds,
    // absent from the docs (research/decision-audit-provider-economics.md
    // C11): empty string is accepted, and a request ending on tool messages
    // 400s if ANY assistant message OMITS the key. Enforcement is
    // INTERMITTENT when no reasoning exists anywhere in history (measured:
    // ablation matrix, 5/12 runs died while an identical-shape 27-msg run
    // passed), so the belt is shape-based: every assistant row carries the
    // key — real echo or empty string — whenever the conversation is in
    // thinking mode OR the request ends on tool messages (the only 400 shape
    // ever observed). User-ending and assistant-ending requests and
    // non-thinking endpoints keep their rows untouched (no unknown fields for
    // strict endpoints).
    let thinking_mode = req.messages.iter().any(|m| {
        m.role == "assistant" && m.thinking.as_deref().is_some_and(|t| !t.trim().is_empty())
    });
    let echo_key = thinking_mode || req.messages.last().is_some_and(|m| m.role == "tool");
    let messages: Vec<serde_json::Value> = req
        .messages
        .iter()
        .map(|m| {
            let mut o = serde_json::json!({"role": m.role, "content": m.content});
            if !m.tool_calls.is_empty() {
                o["tool_calls"] = m
                    .tool_calls
                    .iter()
                    .map(|tc| {
                        let args = match &tc.args {
                            serde_json::Value::String(s) => s.clone(),
                            v => v.to_string(),
                        };
                        serde_json::json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {"name": tc.name, "arguments": args},
                        })
                    })
                    .collect();
            }
            if let Some(id) = &m.tool_call_id {
                o["tool_call_id"] = serde_json::Value::String(id.clone());
            }
            // DeepSeek thinking mode: key presence is mandatory (see above).
            if m.role == "assistant" && echo_key {
                o["reasoning_content"] =
                    match m.thinking.as_deref().filter(|t| !t.trim().is_empty()) {
                        Some(t) => serde_json::Value::String(t.to_string()),
                        None => serde_json::Value::String(String::new()),
                    };
            }
            o
        })
        .collect();
    let mut b = serde_json::json!({
        "model": model,
        "messages": messages,
        "max_tokens": k.max_tokens,
    });
    if k.thinking_suppressed {
        if let Some(frag) = k.thinking_off.as_ref().and_then(|f| f.as_object()) {
            for (kk, vv) in frag {
                b[kk.as_str()] = vv.clone();
            }
        }
    }
    if !req.tools.is_empty() {
        let tools: Vec<serde_json::Value> = req
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.schema,
                    }
                })
            })
            .collect();
        b["tools"] = serde_json::Value::Array(tools);
    }
    if let Some(o) = req.extras.as_object() {
        for (kk, vv) in o {
            b[kk] = vv.clone();
        }
    }
    b
}

struct OnceFail {
    msg: String,
    after: Option<Duration>,
    retryable: bool,
    truncated: bool,
    content_chars: usize,
    reasoning_chars: usize,
    /// Usage the failed response carried; summed across attempts in `complete`.
    /// Boxed: keeps the `Result` small (the failure path is cold).
    usage: Option<Box<Usage>>,
}

impl OpenAiCompat {
    async fn once(&self, model: &str, req: &Request, k: &WireKnobs) -> Result<Response, OnceFail> {
        // Env resolve per attempt, never cached past expiry.
        let key = self
            .resolve_key("openai")
            .await
            .map_err(|e| OnceFail {
                msg: format!("{e:?}"),
                after: None,
                retryable: false,
                truncated: false,
                content_chars: 0,
                reasoning_chars: 0,
                usage: None,
            })?
            .api_key;
        let mut call = self
            .http
            .post(format!("{}/chat/completions", self.endpoint));
        for (name, value) in &self.headers {
            call = call.header(name.as_str(), value.as_str());
        }
        if !key.trim().is_empty() {
            call = call.bearer_auth(key);
        }
        let resp = call
            .json(&wire_body(model, req, k))
            .send()
            .await
            .map_err(|e| OnceFail {
                msg: e.to_string(),
                after: None,
                retryable: true,
                truncated: false,
                content_chars: 0,
                reasoning_chars: 0,
                usage: None,
            })?;
        if !resp.status().is_success() {
            let code = resp.status();
            let after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_retry_after);
            let text = resp.text().await.unwrap_or_default();
            let short: String = text.chars().take(300).collect();
            let msg = format!("{code}: {short}");
            let retryable = matches!(
                provider_core::classify_error(&msg),
                provider_core::ErrorClass::Retryable
                    | provider_core::ErrorClass::Overload
                    | provider_core::ErrorClass::AuthStale
            );
            // Some gateways bill and report usage on an error body; carry it.
            let usage = serde_json::from_str::<ChatResp>(&text)
                .ok()
                .and_then(|r| wire_usage(r.usage, model))
                .map(Box::new);
            return Err(OnceFail {
                msg,
                after,
                retryable,
                truncated: false,
                content_chars: 0,
                reasoning_chars: 0,
                usage,
            });
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| OnceFail {
            msg: e.to_string(),
            after: None,
            retryable: true,
            truncated: false,
            content_chars: 0,
            reasoning_chars: 0,
            usage: None,
        })?;
        parse_body(&v, model, k.max_tokens).map_err(|BodyFail { msg, usage }| {
            let truncated = msg.contains("finish_reason=length");
            let retryable = if truncated {
                true
            } else {
                matches!(
                    provider_core::classify_error(&msg),
                    provider_core::ErrorClass::Retryable
                        | provider_core::ErrorClass::Overload
                        | provider_core::ErrorClass::AuthStale
                )
            };
            OnceFail {
                content_chars: chars_after(&msg, "; content="),
                reasoning_chars: chars_after(&msg, "; reasoning_content="),
                msg,
                after: None,
                retryable,
                truncated,
                usage: usage.map(Box::new),
            }
        })
    }
}

/// Fold a failed attempt's usage into the running total. Attempts are
/// independent requests, so billed tokens sum (`Usage::plus`).
fn merge_usage(acc: Option<Usage>, next: Option<Usage>) -> Option<Usage> {
    match (acc, next) {
        (Some(a), Some(n)) => Some(a.plus(&n)),
        (a, n) => a.or(n),
    }
}

/// Cap on a server-requested `Retry-After` delay. Pi fails fast above its
/// 60 s `maxRetryDelayMs` default (`provider-retry.ts:44-58`): a longer ask is
/// terminal, never slept on.
const RETRY_AFTER_CAP: Duration = Duration::from_secs(60);

/// Process-local draw counter, mixed into the clock stamp so two draws in the
/// same clock tick (or on a coarse clock) still differ.
static JITTER_SEQ: AtomicU64 = AtomicU64::new(0);

/// 0–25% multiplicative jitter for the exponential rungs
/// (pi `provider-retry.ts:66`). Entropy: wall-clock nanoseconds at retry time
/// (parallel workers reach a retry at distinct instants) mixed with a
/// process-local draw counter; splitmix64 finalizer so coarse clock granularity
/// cannot bias the low bits. No `rand` dependency.
fn backoff_jitter_factor() -> f64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() ^ u64::from(d.subsec_nanos()));
    let seq = JITTER_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut x = now ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x % 250) as f64 / 1000.0
}

/// Backoff before retry `attempt` (1-based): the pre-existing 2^attempt-second
/// rung, clamped at 2^10, plus 0–25% multiplicative jitter.
fn backoff_delay(attempt: u32) -> Duration {
    let base = 2u64.pow(attempt.min(10));
    Duration::from_secs_f64(base as f64 * (1.0 + backoff_jitter_factor()))
}

/// Delay before retrying. `None` = terminal: the server asked for more than
/// `RETRY_AFTER_CAP`. Without a header, cold-start 503s keep the 60 s rung
/// (8 attempts) and other errors keep the jittered 2^attempt rung.
fn schedule_delay(after: Option<Duration>, last_err: &str, attempt: u32) -> Option<Duration> {
    match after {
        Some(d) if d > RETRY_AFTER_CAP => None,
        Some(d) => Some(d),
        None if is_cold_start(last_err) => Some(Duration::from_secs(60)),
        None => Some(backoff_delay(attempt)),
    }
}

/// 503 from serverless GPU endpoints usually means cold start (scale-from-zero
/// takes minutes for big models), not rejection. Worth out-waiting; other
/// errors keep the short ladder.
fn is_cold_start(msg: &str) -> bool {
    msg.contains("503")
}

/// Digits after `marker` in a message (`"; content=4 chars"` -> 4). The
/// ladder needs the counts to pick its rung.
fn chars_after(text: &str, marker: &str) -> usize {
    let Some(i) = text.find(marker) else {
        return 0;
    };
    let rest = &text[i + marker.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().unwrap_or(0)
}

#[derive(Debug, Deserialize, Default)]
struct ChatResp {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChoiceMsg,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ChoiceMsg {
    #[serde(default, deserialize_with = "null_as_empty")]
    content: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    reasoning_content: String,
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    tool_calls: Vec<WireToolCall>,
}

fn null_as_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

fn null_as_empty_vec<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<WireToolCall>, D::Error> {
    Ok(Option::<Vec<WireToolCall>>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Deserialize, Default)]
struct WireToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: WireFunc,
}

#[derive(Debug, Deserialize, Default)]
struct WireFunc {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<CachedDetail>,
    #[serde(default)]
    completion_tokens_details: Option<ReasonDetail>,
}

#[derive(Debug, Deserialize, Default)]
struct CachedDetail {
    #[serde(default)]
    cached_tokens: u64,
}

#[derive(Debug, Deserialize, Default)]
struct ReasonDetail {
    #[serde(default)]
    reasoning_tokens: u64,
}

/// A failed body parse: the message plus whatever usage the body carried
/// (a truncated 200 is billed even though its answer is unusable).
#[derive(Debug)]
struct BodyFail {
    msg: String,
    usage: Option<Usage>,
}

/// Wire usage -> `Usage`; None when the body shipped no usage object at all.
/// Never zeros: the budget must not read "unreported" as "free".
fn wire_usage(u: Option<WireUsage>, model: &str) -> Option<Usage> {
    u.map(|u| {
        let mut usage = Usage {
            input: u.prompt_tokens,
            output: u.completion_tokens,
            cache_read: u
                .prompt_tokens_details
                .map(|d| d.cached_tokens)
                .unwrap_or(0),
            cache_write: 0,
            reasoning: u
                .completion_tokens_details
                .map(|d| d.reasoning_tokens)
                .filter(|&n| n > 0),
            cost_usd: None,
        };
        usage.cost_usd = price_usd(model, usage.input, usage.cache_read, usage.output);
        usage
    })
}

/// USD cost at DeepSeek's published PEAK rates ($/1Mtok, api-docs.deepseek.com
/// "Models & Pricing", fetched 2026-10-03): flash in-hit $0.006 / in-miss
/// $0.30 / out $1.20; v4-pro $0.044 / $1.32 / $3.96. PEAK is a conservative
/// upper bound — DeepSeek bills off-peak at exactly half (weekends and CN
/// holidays in full; the holiday calendar is not modeled). `input` includes
/// cache-hit tokens (OpenAI wire semantics), so they split at the hit rate.
/// The go-routed id `deepseek-v4.1-flash` is the same model at the same price
/// book: OpenCode Go's DeepSeek V4.1 Flash rows are DeepSeek's off-peak pair
/// (hit $0.003 / miss $0.15; research/refresh-cache-economics.md:9-10) and the
/// identical peak tuple ($0.006 / $0.30 / $1.20; Go page Peak row, captured
/// in the same research session), so it takes the flash PEAK rates. The table
/// is keyed by model id with no endpoint dimension — if Go's rates diverge
/// from DeepSeek's, this needs a per-endpoint price book.
/// Unknown models price `None`: never fabricate a cost.
fn price_usd(model: &str, input: u64, cache_read: u64, output: u64) -> Option<f64> {
    let (hit, miss, out) = match model {
        // Legacy alias `deepseek-v4-flash` is served by the same model billed
        // at the Flash price (research/DECISIONS.md:74: "deepseek-flash IS
        // DeepSeek-V4.1-Flash (legacy deepseek-v4-flash served by it, billed
        // at Flash price — api-docs footnote 2026-10-03)").
        m if m.starts_with("deepseek-flash")
            || m.starts_with("deepseek-v4-flash")
            || m.starts_with("deepseek-v4.1-flash") =>
        {
            (0.006, 0.30, 1.20)
        }
        m if m.starts_with("deepseek-v4-pro") => (0.044, 1.32, 3.96),
        _ => return None,
    };
    let miss_in = input.saturating_sub(cache_read);
    Some((cache_read as f64 * hit + miss_in as f64 * miss + output as f64 * out) / 1e6)
}

/// Parse one chat/completions body. Truncation and empty answers are errors
/// (never empty answers): the retry ladder needs the counts to decide.
fn parse_body(v: &serde_json::Value, model: &str, max_tokens: usize) -> Result<Response, BodyFail> {
    let resp: ChatResp = serde_json::from_value(v.clone()).map_err(|e| BodyFail {
        msg: format!("bad chat body: {e}"),
        usage: None,
    })?;
    let wire = wire_usage(resp.usage, model);
    let choice = resp.choices.into_iter().next().ok_or_else(|| BodyFail {
        msg: format!("empty content from {model} (finish_reason=None; reasoning_content=0 chars); the endpoint shipped no text"),
        usage: wire.clone(),
    })?;
    let finish = choice.finish_reason.clone();
    let text = choice.message.content.clone();
    let reasoning = choice.message.reasoning_content.clone();
    let mut calls = Vec::new();
    for (i, tc) in choice.message.tool_calls.iter().enumerate() {
        let args_raw = tc.function.arguments.as_deref().unwrap_or("{}");
        // Salvage chain: truncated/invalid args decode to {} and stay
        // model-visible; strict validation errors never throw here.
        let args = parse_streaming_json(args_raw);
        calls.push(ToolCallRef {
            id: tc.id.clone().unwrap_or_else(|| format!("call-{i}")),
            name: tc.function.name.clone().unwrap_or_default(),
            args,
        });
    }
    let message = provider_core::AssistantMessage {
        content: text.clone(),
        tool_calls: calls,
        thinking: (!reasoning.trim().is_empty()).then_some(reasoning.clone()),
    };
    let stop = infer_stop(finish.as_deref(), &message);
    let (input, output, cache_read, reasoning_tok) = wire
        .as_ref()
        .map(|u| (u.input, u.output, u.cache_read, u.reasoning))
        .unwrap_or((0, 0, 0, None));
    if stop == StopReason::MaxTokens {
        return Err(BodyFail {
            msg: format!(
                "output truncated at {max_tokens} tokens (finish_reason=length; content={} chars; reasoning_content={} chars); raise max_tokens",
                text.trim().len(),
                reasoning.trim().len()
            ),
            usage: wire,
        });
    }
    if text.trim().is_empty() && message.tool_calls.is_empty() {
        return Err(BodyFail {
            msg: format!(
                "empty content from {model} (finish_reason={finish:?}; reasoning_content={} chars); the endpoint shipped no text",
                reasoning.trim().len()
            ),
            usage: wire,
        });
    }
    Ok(Response {
        message,
        stop,
        usage: Usage {
            input,
            output,
            cache_read,
            cache_write: 0,
            reasoning: reasoning_tok,
            cost_usd: wire.as_ref().and_then(|u| u.cost_usd),
        },
        latency_ms: 0,
        attempts: 1,
        raw_stop_reason: finish,
        retry_usage: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use provider_core::{ProviderMessage, Thinking};
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Mutex};

    fn req_with_tool() -> Request {
        Request {
            messages: vec![
                ProviderMessage {
                    role: "system".into(),
                    content: "sys".into(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    thinking: None,
                },
                ProviderMessage {
                    role: "user".into(),
                    content: "hi".into(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    thinking: None,
                },
            ],
            tools: vec![tool_core::ToolDeclaration {
                name: "read".into(),
                description: "read a file".into(),
                schema: serde_json::json!({
                    "type": "object",
                    "required": ["path"],
                    "additionalProperties": false,
                    "properties": {"path": {"type": "string"}}
                }),
            }],
            max_tokens: 64,
            thinking: Thinking::Auto,
            extras: serde_json::Value::Null,
        }
    }

    #[test]
    fn wire_body_threads_tool_ids_for_strict_providers() {
        use provider_core::ToolCallRef;
        let req = Request {
            messages: vec![
                ProviderMessage {
                    role: "assistant".into(),
                    content: String::new(),
                    tool_calls: vec![ToolCallRef {
                        id: "c1".into(),
                        name: "read".into(),
                        args: serde_json::json!({"path": "a"}),
                    }],
                    tool_call_id: None,
                    thinking: None,
                },
                ProviderMessage {
                    role: "tool".into(),
                    content: "ok".into(),
                    tool_calls: Vec::new(),
                    tool_call_id: Some("c1".into()),
                    thinking: None,
                },
            ],
            tools: vec![],
            max_tokens: 64,
            thinking: Thinking::Auto,
            extras: serde_json::Value::Null,
        };
        let b = wire_body(
            "m",
            &req,
            &WireKnobs::from_req(&req, &EndpointProfile::default()),
        );
        assert_eq!(b["messages"][0]["tool_calls"][0]["id"], "c1");
        assert_eq!(b["messages"][0]["tool_calls"][0]["type"], "function");
        assert_eq!(b["messages"][1]["tool_call_id"], "c1");
    }

    #[test]
    fn wire_body_echoes_reasoning_content_in_thinking_mode_conversations() {
        let mk = |role: &str, thinking: Option<&str>| ProviderMessage {
            role: role.into(),
            content: "x".into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            thinking: thinking.map(str::to_owned),
        };
        let req = Request {
            messages: vec![
                mk("assistant", Some("chain of thought")),
                mk("assistant", None),
                mk("assistant", Some("   ")),
                mk("user", Some("not assistant reasoning")),
            ],
            tools: vec![],
            max_tokens: 64,
            thinking: Thinking::Auto,
            extras: serde_json::Value::Null,
        };
        let b = wire_body(
            "m",
            &req,
            &WireKnobs::from_req(&req, &EndpointProfile::default()),
        );
        assert_eq!(b["messages"][0]["reasoning_content"], "chain of thought");
        // Thinking-mode conversation (msg 0 carries real reasoning): every
        // assistant message carries the KEY — empty string when absent.
        // Measured: an omitted key on any assistant message 400s
        // "reasoning_content ... must be passed back" on tool-terminated
        // requests (api.deepseek.com); empty string is accepted.
        assert_eq!(b["messages"][1]["reasoning_content"], "");
        assert_eq!(b["messages"][2]["reasoning_content"], "");
        // Non-assistant messages never carry it.
        assert!(b["messages"][3].get("reasoning_content").is_none());
        // Non-thinking conversation: key stays absent everywhere.
        let plain = Request {
            messages: vec![mk("assistant", None), mk("assistant", None)],
            tools: vec![],
            max_tokens: 64,
            thinking: Thinking::Auto,
            extras: serde_json::Value::Null,
        };
        let pb = wire_body(
            "m",
            &plain,
            &WireKnobs::from_req(&plain, &EndpointProfile::default()),
        );
        assert!(pb["messages"][0].get("reasoning_content").is_none());
        assert!(pb["messages"][1].get("reasoning_content").is_none());
    }

    #[test]
    fn request_mapping_snapshot_incl_tools() {
        let req = req_with_tool();
        let k = WireKnobs::from_req(&req, &EndpointProfile::default());
        let b = wire_body("m", &req, &k);
        assert_eq!(b["model"], "m");
        assert_eq!(b["messages"].as_array().unwrap().len(), 2);
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["max_tokens"], 64);
        let tools = b["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "read");
        assert_eq!(tools[0]["function"]["parameters"]["type"], "object");
        assert!(b.get("thinking").is_none());
        // Thinking::Off applies the profile's real knob, nothing else.
        let mut off = req.clone();
        off.thinking = Thinking::Off;
        let b2 = wire_body(
            "m",
            &off,
            &WireKnobs::from_req(&off, &EndpointProfile::default()),
        );
        assert_eq!(b2["thinking"]["type"], "disabled");
        assert!(b2.get("reasoning").is_none());
        assert!(b2.get("enable_thinking").is_none());
        assert!(b2.get("reasoning_effort").is_none());
    }

    #[test]
    fn response_parse_incl_tool_calls_and_usage_subset() {
        let v = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "doing it",
                    "tool_calls": [{
                        "id": "c1",
                        "type": "function",
                        "function": {"name": "read", "arguments": "{\"path\":\"a.rs\"}"}
                    }],
                    "finish_reason": "tool_calls"
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 100,
                "completion_tokens_details": {"reasoning_tokens": 30}
            }
        });
        let r = parse_body(&v, "m", 64).unwrap();
        assert_eq!(r.message.tool_calls.len(), 1);
        assert_eq!(r.message.tool_calls[0].args["path"], "a.rs");
        assert_eq!(r.stop, StopReason::ToolUse);
        assert_eq!(r.usage.reasoning, Some(30));
        assert_eq!(r.usage.total_tokens(), 110);
        assert_eq!(r.usage.content_tokens(), 70);
    }

    #[test]
    fn cold_start_is_503_only() {
        assert!(is_cold_start("503 Service Unavailable: "));
        assert!(!is_cold_start("429 Too Many Requests"));
        assert!(!is_cold_start("502 Bad Gateway"));
    }

    #[test]
    fn null_tool_calls_decode_to_empty() {
        let v = serde_json::json!({
            "choices": [{"message": {"content": "ok", "tool_calls": null},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2}
        });
        let r = parse_body(&v, "m", 64).unwrap();
        assert_eq!(r.message.content, "ok");
        assert!(r.message.tool_calls.is_empty());
    }

    #[test]
    fn truncation_is_error_with_counts_never_empty_answer() {
        let v = serde_json::json!({
            "choices": [{"message": {"content": "half", "reasoning_content": "rrr"},
                         "finish_reason": "length"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 64}
        });
        let e = parse_body(&v, "m", 64).unwrap_err();
        assert!(e.msg.contains("finish_reason=length"), "{}", e.msg);
        assert!(e.msg.contains("content=4 chars"), "{}", e.msg);
        assert!(e.msg.contains("reasoning_content=3 chars"), "{}", e.msg);
        // The billed usage rides the failure, not just the message.
        let u = e.usage.expect("truncated body carried usage");
        assert_eq!((u.input, u.output), (5, 64));
        // Empty answer is also an error, never Ok("").
        let empty = serde_json::json!({
            "choices": [{"message": {"content": null}, "finish_reason": "stop"}]
        });
        let e2 = parse_body(&empty, "m", 64).unwrap_err();
        assert!(e2.msg.contains("empty content"), "{}", e2.msg);
        assert!(e2.usage.is_none(), "no usage object -> None, never zeros");
    }

    #[test]
    fn truncation_message_parses_content_and_reasoning_counts() {
        let v = serde_json::json!({
            "choices": [{"message": {"content": "", "reasoning_content": "rrrr"},
                         "finish_reason": "length"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 8}
        });
        let f = parse_body(&v, "m", 64).unwrap_err();
        assert_eq!(chars_after(&f.msg, "; content="), 0);
        assert_eq!(chars_after(&f.msg, "; reasoning_content="), 4);
        assert_eq!(chars_after("no counts here", "; content="), 0);
    }

    #[test]
    fn brace_in_string_tool_args_survive() {
        let v = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "x",
                    "tool_calls": [{
                        "id": "c1",
                        "type": "function",
                        "function": {
                            "name": "edit",
                            "arguments": "{\"path\":\"a.rs\",\"search\":\"fn f() {\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let r = parse_body(&v, "m", 64).unwrap();
        assert_eq!(r.message.tool_calls[0].args["search"], "fn f() {");
        // A lone closing brace inside the string must not truncate.
        let v2 = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "x",
                    "tool_calls": [{
                        "id": "c1",
                        "type": "function",
                        "function": {
                            "name": "edit",
                            "arguments": "{\"content\":\"    }\\n\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let r2 = parse_body(&v2, "m", 64).unwrap();
        assert_eq!(r2.message.tool_calls[0].args["content"], "    }\n");
    }

    #[test]
    fn tool_ending_requests_always_carry_reasoning_content() {
        // Regression: no reasoning anywhere in history + request ends on a
        // tool row = the 400 shape. Every assistant row must carry the key.
        let mut req = req_with_tool();
        req.messages.extend([
            ProviderMessage {
                role: "assistant".into(),
                content: "looking".into(),
                tool_calls: vec![provider_core::ToolCallRef {
                    id: "c1".into(),
                    name: "read".into(),
                    args: serde_json::json!({"path": "a"}),
                }],
                tool_call_id: None,
                thinking: None,
            },
            ProviderMessage {
                role: "tool".into(),
                content: "data".into(),
                tool_calls: Vec::new(),
                tool_call_id: Some("c1".into()),
                thinking: None,
            },
        ]);
        let k = WireKnobs::from_req(&req, &EndpointProfile::default());
        let b = wire_body("m", &req, &k);
        let msgs = b["messages"].as_array().unwrap();
        let asst: Vec<&serde_json::Value> =
            msgs.iter().filter(|m| m["role"] == "assistant").collect();
        assert!(!asst.is_empty());
        for m in asst {
            assert_eq!(
                m["reasoning_content"], "",
                "tool-ending request needs the key"
            );
        }
        // Assistant-ending, non-thinking requests keep rows untouched.
        let mut req2 = req_with_tool();
        req2.messages.push(ProviderMessage {
            role: "assistant".into(),
            content: "x".into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            thinking: None,
        });
        let b3 = wire_body(
            "m",
            &req2,
            &WireKnobs::from_req(&req2, &EndpointProfile::default()),
        );
        let m = b3["messages"].as_array().unwrap().last().unwrap();
        assert!(
            m.get("reasoning_content").is_none(),
            "assistant-ending stays untouched"
        );
    }

    #[test]
    fn price_usd_splits_hit_miss_and_never_fabricates() {
        // flash PEAK: hit $0.006/M, miss $0.30/M, out $1.20/M (api-docs 2026-10-03)
        let c = price_usd("deepseek-flash", 1_000_000, 500_000, 250_000).unwrap();
        assert!((c - 0.453).abs() < 1e-9, "{c}"); // 0.003 + 0.15 + 0.30
        let all_hit = price_usd("deepseek-flash", 1_000_000, 1_000_000, 0).unwrap();
        assert!(
            (all_hit - 0.006).abs() < 1e-12,
            "cache hits never pay miss rate"
        );
        let pro = price_usd("deepseek-v4-pro", 1_000_000, 0, 1_000_000).unwrap();
        assert!((pro - (1.32 + 3.96)).abs() < 1e-9, "pro PEAK sum");
        // go-routed matched-run id: same price book as direct flash
        let go = price_usd("deepseek-v4.1-flash", 1_000_000, 500_000, 250_000).unwrap();
        assert!(
            (go - 0.453).abs() < 1e-9,
            "go flash prices like direct flash: {go}"
        );
        // legacy alias: same model billed at the Flash price
        // (research/DECISIONS.md:74)
        let legacy = price_usd("deepseek-v4-flash", 1_000_000, 500_000, 250_000).unwrap();
        assert!(
            (legacy - 0.453).abs() < 1e-9,
            "legacy alias prices like flash: {legacy}"
        );
        assert_eq!(
            price_usd("gpt-4o", 10, 0, 10),
            None,
            "unknown models never fabricate a cost"
        );
    }

    #[test]
    fn parse_body_prices_usage_and_starts_without_retries() {
        let v = serde_json::json!({
            "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1000, "completion_tokens": 500}
        });
        let r = parse_body(&v, "deepseek-flash", 64).unwrap();
        let cost = r.usage.cost_usd.expect("known model is priced");
        assert!((cost - (1000.0 * 0.30 + 500.0 * 1.20) / 1e6).abs() < 1e-12);
        assert!(r.retry_usage.is_none());
    }

    #[tokio::test]
    async fn recovered_ladder_carries_failed_attempt_usage() {
        std::env::set_var("TEST_OAI_KEY_RL", "k");
        let (ep, _) = serve(vec![
            Canned {
                status: 200,
                headers: vec![],
                body: trunc_body("half", ""),
            },
            Canned {
                status: 200,
                headers: vec![],
                body: ok_body("done"),
            },
        ]);
        let c = client_on(&ep, "TEST_OAI_KEY_RL");
        let r = c.complete("m", &req_with_tool()).await.unwrap();
        assert_eq!(r.attempts, 2, "truncation rung then success");
        assert_eq!(
            (r.usage.input, r.usage.output),
            (1, 2),
            "final attempt alone"
        );
        let ru = r.retry_usage.as_ref().expect("failed attempts carried");
        assert_eq!((ru.input, ru.output), (5, 64), "the billed re-send");
        let b = r.billed_usage();
        assert_eq!((b.input, b.output), (6, 66), "metering sees the whole bill");
    }

    struct Canned {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    }

    fn serve(canned: Vec<Canned>) -> (String, Arc<Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let b2 = bodies.clone();
        std::thread::spawn(move || {
            let hits = Arc::new(AtomicUsize::new(0));
            let _ = &hits;
            let mut n = 0usize;
            for stream in l.incoming() {
                let Ok(s) = stream else { break };
                s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut head = String::new();
                let mut len = 0usize;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                    if let Some(v) = line
                        .strip_prefix("Content-Length:")
                        .or_else(|| line.strip_prefix("content-length:"))
                    {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut buf = vec![0u8; len];
                if len > 0 {
                    r.read_exact(&mut buf).unwrap_or(());
                }
                b2.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf).to_string());
                let c = &canned[n.min(canned.len() - 1)];
                let reason = match c.status {
                    200 => "OK",
                    429 => "Too Many Requests",
                    502 => "Bad Gateway",
                    _ => "Error",
                };
                let mut resp = format!(
                    "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    c.status,
                    reason,
                    c.body.len()
                );
                for (hk, hv) in &c.headers {
                    resp.push_str(&format!("{hk}: {hv}\r\n"));
                }
                resp.push_str("\r\n");
                let _ = r
                    .into_inner()
                    .write_all(format!("{resp}{}", c.body).as_bytes());
                n += 1;
                if n >= canned.len() + 2 {
                    break;
                }
            }
        });
        (format!("http://{addr}"), bodies)
    }

    fn ok_body(text: &str) -> String {
        serde_json::json!({
            "choices": [{"message": {"content": text}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2}
        })
        .to_string()
    }

    fn trunc_body(content: &str, reasoning: &str) -> String {
        serde_json::json!({
            "choices": [{
                "message": {"content": content, "reasoning_content": reasoning},
                "finish_reason": "length"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 64}
        })
        .to_string()
    }

    fn client_on(endpoint: &str, key_env: &str) -> OpenAiCompat {
        OpenAiCompat::new(endpoint, EndpointProfile::default()).with_key_env(key_env)
    }

    #[tokio::test]
    async fn retry_on_429_then_502_then_success_counts_attempts() {
        std::env::set_var("TEST_OAI_KEY_R1", "k");
        let (ep, _bodies) = serve(vec![
            Canned {
                status: 429,
                headers: vec![("Retry-After".into(), "0".into())],
                body: "rate limited".into(),
            },
            Canned {
                status: 502,
                headers: vec![("Retry-After".into(), "0".into())],
                body: "bad gateway".into(),
            },
            Canned {
                status: 200,
                headers: vec![],
                body: ok_body("back"),
            },
        ]);
        let c = client_on(&ep, "TEST_OAI_KEY_R1");
        let mut req = req_with_tool();
        req.tools.clear();
        let r = c.complete("m", &req).await.unwrap();
        assert_eq!(r.message.content, "back");
        assert_eq!(r.attempts, 3);
    }

    /// DeepSeek thinking-mode protocol: the reasoning a response carries must
    /// ride the next turn's assistant message back as `reasoning_content`.
    #[tokio::test]
    async fn complete_echoes_prior_turn_reasoning_content_on_the_next_request() {
        std::env::set_var("TEST_OAI_KEY_THINK", "k");
        let (ep, bodies) = serve(vec![
            Canned {
                status: 200,
                headers: vec![],
                body: serde_json::json!({
                    "choices": [{
                        "message": {"content": "first", "reasoning_content": "chain of thought"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 2}
                })
                .to_string(),
            },
            Canned {
                status: 200,
                headers: vec![],
                body: ok_body("second"),
            },
        ]);
        let c = client_on(&ep, "TEST_OAI_KEY_THINK");
        let r1 = c.complete("m", &req_with_tool()).await.unwrap();
        assert_eq!(r1.message.thinking.as_deref(), Some("chain of thought"));
        // Turn 2's history, as agent-loop derives it from the stored item.
        let mut req2 = req_with_tool();
        req2.messages.push(ProviderMessage {
            role: "assistant".into(),
            content: r1.message.content.clone(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            thinking: r1.message.thinking.clone(),
        });
        c.complete("m", &req2).await.unwrap();
        let sent = bodies.lock().unwrap();
        let b: serde_json::Value = serde_json::from_str(&sent[1]).unwrap();
        assert_eq!(b["messages"][2]["reasoning_content"], "chain of thought");
        // Messages that captured no reasoning stay clean on the wire.
        assert!(b["messages"][0].get("reasoning_content").is_none());
    }

    #[tokio::test]
    async fn retry_after_header_is_honored() {
        std::env::set_var("TEST_OAI_KEY_R2", "k");
        let (ep, _) = serve(vec![
            Canned {
                status: 429,
                headers: vec![("Retry-After".into(), "1".into())],
                body: "slow down".into(),
            },
            Canned {
                status: 200,
                headers: vec![],
                body: ok_body("ok"),
            },
        ]);
        let c = client_on(&ep, "TEST_OAI_KEY_R2");
        let mut req = req_with_tool();
        req.tools.clear();
        let t = Instant::now();
        let r = c.complete("m", &req).await.unwrap();
        assert_eq!(r.attempts, 2);
        assert!(
            t.elapsed() >= Duration::from_millis(900),
            "Retry-After: 1 must delay the retry, elapsed {:?}",
            t.elapsed()
        );
    }

    /// A server asking for an hour is a capacity statement, not a retry hint:
    /// return the terminal error instead of sleeping (pi `provider-retry.ts:44-58`).
    #[tokio::test]
    async fn retry_after_above_cap_is_terminal_without_sleeping() {
        std::env::set_var("TEST_OAI_KEY_R3", "k");
        let (ep, bodies) = serve(vec![Canned {
            status: 429,
            headers: vec![("Retry-After".into(), "3600".into())],
            body: "no capacity for an hour".into(),
        }]);
        let c = client_on(&ep, "TEST_OAI_KEY_R3");
        let mut req = req_with_tool();
        req.tools.clear();
        let t = Instant::now();
        let err = tokio::time::timeout(Duration::from_secs(5), c.complete("m", &req))
            .await
            .expect("Retry-After above the cap must fail fast, not sleep")
            .unwrap_err();
        match err {
            LlmError::Metered { source, .. } => assert!(source.contains("429"), "{source}"),
            other => panic!("expected Metered, got {other:?}"),
        }
        assert_eq!(bodies.lock().unwrap().len(), 1, "terminal: no re-send");
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "elapsed {:?}",
            t.elapsed()
        );
    }

    #[tokio::test]
    async fn complete_ladder_sends_thinking_disabled_on_reasoning_only_truncation() {
        std::env::set_var("TEST_OAI_KEY_L1", "k");
        let (ep, bodies) = serve(vec![
            Canned {
                status: 200,
                headers: vec![],
                body: trunc_body("", "rrrr"),
            },
            Canned {
                status: 200,
                headers: vec![],
                body: ok_body("done"),
            },
        ]);
        let c = client_on(&ep, "TEST_OAI_KEY_L1");
        let mut req = req_with_tool();
        req.tools.clear();
        let r = c.complete("m", &req).await.unwrap();
        assert_eq!(r.attempts, 2);
        let sent = bodies.lock().unwrap();
        assert_eq!(sent.len(), 2, "one retry after the thinking-off rung");
        let b0: serde_json::Value = serde_json::from_str(&sent[0]).unwrap();
        let b1: serde_json::Value = serde_json::from_str(&sent[1]).unwrap();
        assert!(b0.get("thinking").is_none());
        assert_eq!(b1["thinking"]["type"], "disabled");
        assert_eq!(b1["max_tokens"], 64, "rung 1 never shrinks or raises");
    }

    #[tokio::test]
    async fn failed_truncation_error_carries_usage() {
        std::env::set_var("TEST_OAI_KEY_U1", "k");
        let (ep, _) = serve(vec![Canned {
            status: 200,
            headers: vec![],
            body: trunc_body("half", ""),
        }]);
        let c = client_on(&ep, "TEST_OAI_KEY_U1");
        let mut req = req_with_tool();
        req.tools.clear();
        // At the ceiling no rung applies, so the first failure is final: one send.
        req.max_tokens = MAX_TOKENS_CEILING;
        let err = c.complete("m", &req).await.unwrap_err();
        match err {
            LlmError::Metered {
                source,
                usage: Some(u),
            } => {
                assert!(source.contains("finish_reason=length"), "{source}");
                assert_eq!((u.input, u.output), (5, 64));
            }
            other => panic!("expected Metered with usage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_2xx_parseable_usage_is_carried_and_summed_across_retries() {
        std::env::set_var("TEST_OAI_KEY_U2", "k");
        let body = serde_json::json!({
            "error": {"message": "rate limited"},
            "usage": {"prompt_tokens": 7, "completion_tokens": 0}
        })
        .to_string();
        // Retry-After: 0 keeps the five attempts instant; the server repeats
        // the last canned reply.
        let canned: Vec<Canned> = (0..5)
            .map(|_| Canned {
                status: 429,
                headers: vec![("Retry-After".into(), "0".into())],
                body: body.clone(),
            })
            .collect();
        let (ep, bodies) = serve(canned);
        let c = client_on(&ep, "TEST_OAI_KEY_U2");
        let mut req = req_with_tool();
        req.tools.clear();
        let err = c.complete("m", &req).await.unwrap_err();
        assert_eq!(bodies.lock().unwrap().len(), 5, "1 + 4 retries");
        match err {
            LlmError::Metered {
                source,
                usage: Some(u),
            } => {
                assert!(source.contains("429"), "{source}");
                // Every failed attempt was billed the prompt: the error sums them.
                assert_eq!(u.input, 35);
            }
            other => panic!("expected Metered with usage, got {other:?}"),
        }
    }

    #[test]
    fn retry_after_both_encodings() {
        assert_eq!(parse_retry_after("0"), Some(Duration::from_secs(0)));
        assert!(
            parse_retry_after("Sun, 06 Nov 2034 08:49:37 GMT")
                .unwrap()
                .as_secs()
                > 200_000_000
        );
    }

    #[test]
    fn schedule_delay_caps_server_requests_at_60s() {
        // Above the cap: terminal, however large the ask — and both header
        // encodings (seconds, HTTP-date) funnel through here.
        assert!(schedule_delay(Some(Duration::from_secs(3600)), "429", 1).is_none());
        assert!(schedule_delay(Some(Duration::from_secs(61)), "429", 1).is_none());
        assert!(
            schedule_delay(parse_retry_after("Sun, 06 Nov 2034 08:49:37 GMT"), "429", 1).is_none()
        );
        // Exactly the cap is still honoured, verbatim.
        assert_eq!(
            schedule_delay(Some(Duration::from_secs(60)), "429", 1),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            schedule_delay(Some(Duration::from_secs(1)), "429", 1),
            Some(Duration::from_secs(1))
        );
        // No header: cold-start 503 keeps its 60 s rung, other errors the
        // jittered 2^attempt rung.
        assert_eq!(
            schedule_delay(None, "503 Service Unavailable", 3),
            Some(Duration::from_secs(60))
        );
        let d = schedule_delay(None, "429 Too Many Requests", 1).unwrap();
        assert!(
            d >= Duration::from_secs(2) && d <= Duration::from_millis(2500),
            "{d:?}"
        );
    }

    #[test]
    fn backoff_delay_jitter_stays_within_rung_bounds() {
        // 0–25% multiplicative jitter: every draw stays in [base, 1.25*base],
        // rung magnitudes and ordering are the pre-jitter 2^attempt ladder,
        // and the 2^10 clamp is unchanged.
        let mut prev_hi = Duration::ZERO;
        for attempt in 1..=11u32 {
            let base = Duration::from_secs(2u64.pow(attempt.min(10)));
            let mut lo = Duration::MAX;
            let mut hi = Duration::ZERO;
            let mut samples = Vec::new();
            for _ in 0..128 {
                let d = backoff_delay(attempt);
                assert!(d >= base, "attempt {attempt}: {d:?} below base {base:?}");
                assert!(
                    d.as_secs_f64() <= base.as_secs_f64() * 1.25,
                    "attempt {attempt}: {d:?} above 1.25*{base:?}"
                );
                lo = lo.min(d);
                hi = hi.max(d);
                samples.push(d);
            }
            assert!(
                samples.iter().any(|d| *d != samples[0]),
                "attempt {attempt}: jitter draws must vary"
            );
            if attempt <= 10 {
                assert!(lo > prev_hi, "attempt {attempt}: rungs must stay ordered");
            } else {
                // 2^10 clamp unchanged: same rung magnitude as attempt 10.
                assert_eq!(base, Duration::from_secs(1024));
            }
            prev_hi = hi;
        }
    }

    #[test]
    fn ladder_thinking_off_first_then_raises_never_shrinks() {
        let profile = EndpointProfile::default();
        let mut req = req_with_tool();
        req.max_tokens = 8_000;
        let mut k = WireKnobs::from_req(&req, &profile);
        // Zero-content reasoning overflow: thinking-off first, no token change.
        assert!(apply_ladder(&mut k, 0, 500));
        assert!(k.thinking_suppressed);
        assert_eq!(k.max_tokens, 8_000);
        // Same failure again: the one-shot rung is spent, so raise (double).
        assert!(apply_ladder(&mut k, 0, 500));
        assert_eq!(k.max_tokens, 16_000);
        assert!(apply_ladder(&mut k, 0, 500));
        assert_eq!(k.max_tokens, 32_000);
        assert!(apply_ladder(&mut k, 0, 500));
        assert_eq!(k.max_tokens, MAX_TOKENS_CEILING, "bounded by the ceiling");
        assert!(!apply_ladder(&mut k, 0, 500), "no rung left at the ceiling");
        // Content shipped: thinking is not the culprit, raise immediately.
        let mut c = WireKnobs::from_req(&req, &profile);
        assert!(apply_ladder(&mut c, 50, 500));
        assert!(!c.thinking_suppressed);
        assert_eq!(c.max_tokens, 16_000);
        // No real knob for this endpoint: the placebo rung is skipped.
        let knobless = EndpointProfile {
            thinking_off: None,
            ..profile
        };
        let mut n = WireKnobs::from_req(&req, &knobless);
        assert!(apply_ladder(&mut n, 0, 500));
        assert!(!n.thinking_suppressed);
        assert_eq!(n.max_tokens, 16_000);
        // Degenerate zero budget: no rung may claim progress (no endless ladder).
        let mut z = WireKnobs {
            max_tokens: 0,
            thinking_off: None,
            thinking_suppressed: true,
        };
        assert!(!apply_ladder(&mut z, 50, 0));
        assert_eq!(z.max_tokens, 0);
    }

    #[test]
    fn ladder_rung_wire_body_sends_thinking_disabled_and_keeps_max_tokens() {
        let profile = EndpointProfile::default();
        let req = req_with_tool();
        let mut k = WireKnobs::from_req(&req, &profile);
        let before = wire_body("m", &req, &k);
        assert!(before.get("thinking").is_none());
        assert!(apply_ladder(&mut k, 0, 12));
        let after = wire_body("m", &req, &k);
        assert_eq!(after["thinking"]["type"], "disabled");
        assert_eq!(after["max_tokens"], 64, "rung 1 spends no extra tokens");
        // The next rung raises and leaves the knob in place.
        assert!(apply_ladder(&mut k, 0, 12));
        let raised = wire_body("m", &req, &k);
        assert_eq!(raised["thinking"]["type"], "disabled");
        assert_eq!(raised["max_tokens"], 128);
    }

    #[test]
    fn extras_merge_and_profile_threshold_default() {
        let profile = EndpointProfile::default();
        assert_eq!(profile.emission_threshold_chars, 12_000);
        assert_eq!(
            profile.thinking_off.as_ref().unwrap()["thinking"]["type"],
            "disabled"
        );
        let mut req = req_with_tool();
        req.extras = serde_json::json!({"temperature": 0});
        let k = WireKnobs::from_req(&req, &profile);
        let b = wire_body("m", &req, &k);
        assert_eq!(b["temperature"], 0);
        let _ = HashMap::<String, String>::new();
    }
}
