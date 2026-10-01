//! One OpenAI-compatible chat/completions adapter implementing
//! provider_core::LlmClient. Covers DeepSeek/Atria/OpenRouter/Ollama through
//! config (endpoint + key + model id). See research/crate-provider-core.md.
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt as _;
use provider_core::{
    infer_stop, parse_retry_after, parse_streaming_json, Capabilities, Credentials, LlmClient,
    LlmError, Request, Response, StopReason, StreamEvent, StreamReducer, ToolCallRef, Usage,
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Per-endpoint capability profile. Thresholds are re-measured per endpoint,
/// never transferred (v1 profile.rs lesson).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointProfile {
    pub emission_threshold_chars: usize,
}

impl Default for EndpointProfile {
    fn default() -> Self {
        Self {
            emission_threshold_chars: 12_000,
        }
    }
}

pub struct OpenAiCompat {
    endpoint: String,
    profile: EndpointProfile,
    http: reqwest::Client,
    key_env: String,
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
        }
    }

    pub fn with_key_env(mut self, env: &str) -> Self {
        self.key_env = env.to_string();
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
        let mut knobs = WireKnobs::from_req(req);
        // ponytail: retry lives here, not in every caller. Free tiers 429 often.
        // Cold-start extension: 503s get up to 8 attempts with 60s sleeps
        // (~4 min, covers Modal scale-from-zero); everything else keeps 5.
        let mut last_err = "no attempts".to_string();
        let mut last_after: Option<Duration> = None;
        let mut attempt = 0u32;
        loop {
            if attempt > 0 {
                let d = last_after.take().unwrap_or_else(|| {
                    if is_cold_start(&last_err) {
                        Duration::from_secs(60)
                    } else {
                        Duration::from_secs(2u64.pow(attempt.min(10)))
                    }
                });
                tokio::time::sleep(d).await;
            }
            match self.once(model, req, &knobs).await {
                Ok(mut r) => {
                    r.latency_ms = t.elapsed().as_millis() as u64;
                    r.attempts = u64::from(attempt) + 1;
                    return Ok(r);
                }
                Err(f) => {
                    last_err = f.msg.clone();
                    if !f.retryable {
                        break;
                    }
                    let cap = if is_cold_start(&f.msg) { 7 } else { 4 };
                    if attempt >= cap {
                        break;
                    }
                    if f.truncated {
                        if !apply_ladder(&mut knobs, f.content_chars) {
                            break;
                        }
                        last_after = f.after;
                    } else {
                        last_after = f.after;
                    }
                }
            }
            attempt += 1;
        }
        // unreachable-looking tail kept for structure; loop always breaks to err
        #[allow(unreachable_code)]
        Err(LlmError::Transport(last_err))
    }

    async fn stream(
        &self,
        model: &str,
        req: &Request,
    ) -> Result<BoxStream<'static, StreamEvent>, LlmError> {
        let key = self.resolve_key("openai").await?.api_key;
        let knobs = WireKnobs::from_req(req);
        let mut body = wire_body(model, req, &knobs);
        body["stream"] = serde_json::Value::Bool(true);
        let mut call = self
            .http
            .post(format!("{}/chat/completions", self.endpoint))
            .header("Accept", "text/event-stream");
        if !key.trim().is_empty() {
            call = call.bearer_auth(key);
        }
        let resp = call
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        if !resp.status().is_success() {
            let code = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let short: String = text.chars().take(300).collect();
            return Err(LlmError::Transport(format!("{code}: {short}")));
        }
        // Collect with per-chunk idle timeout + per-frame cap, then project to
        // events. The reducer still owns the cumulative message downstream.
        let mut raw = String::new();
        let mut stream = resp.bytes_stream();
        loop {
            match tokio::time::timeout(
                Duration::from_secs(MODEL_RESPONSE_IDLE_TIMEOUT_SECS),
                stream.next(),
            )
            .await
            {
                Err(_) => return Err(LlmError::Transport("sse idle timeout".into())),
                Ok(None) => break,
                Ok(Some(Err(e))) => return Err(LlmError::Transport(e.to_string())),
                Ok(Some(Ok(chunk))) => {
                    raw.push_str(&String::from_utf8_lossy(&chunk));
                }
            }
        }
        let events = sse_to_events(&raw)?;
        Ok(futures::stream::iter(events).boxed())
    }

    fn capabilities(&self, _model: &str) -> Capabilities {
        Capabilities {
            sends_finish_reason: true,
        }
    }

    async fn resolve_key(&self, _provider: &str) -> Result<Credentials, LlmError> {
        // Per-call resolve, never cached: tokens expire mid-run.
        Ok(Credentials {
            api_key: std::env::var(&self.key_env).unwrap_or_default(),
        })
    }
}

/// SSE caps: reasoning models go minutes between events; tool-arg deltas can
/// be huge. Both are load-bearing (Unreal adapter.go:129-130).
pub const MODEL_RESPONSE_IDLE_TIMEOUT_SECS: u64 = 30 * 60;
pub const MAX_SSE_FRAME_BYTES: usize = 256 << 20;

#[derive(Debug, Clone)]
struct WireKnobs {
    max_tokens: usize,
    reasoning_effort: Option<String>,
    enable_thinking: Option<bool>,
    reasoning: Option<bool>,
    roomier: bool,
    shrunk: bool,
}

impl WireKnobs {
    fn from_req(req: &Request) -> Self {
        // Thinking::Off is already the strongest rung: start reasoning-off so
        // the ladder only has roomier/shrink left. Auto starts unshaped.
        let reasoning = matches!(req.thinking, provider_core::Thinking::Off).then_some(false);
        Self {
            max_tokens: req.max_tokens,
            reasoning_effort: None,
            enable_thinking: None,
            reasoning,
            roomier: false,
            shrunk: false,
        }
    }
}

/// Cheapest-quality rungs first; roomier only when content shipped, shrink
/// terminal on the zero-content sequence. Returns whether a reshape applied.
fn apply_ladder(k: &mut WireKnobs, content_chars: usize) -> bool {
    if k.reasoning_effort.is_none() && k.enable_thinking.is_none() && k.reasoning.is_none() {
        k.reasoning_effort = Some("low".to_string());
        return true;
    }
    if k.enable_thinking.is_none() && k.reasoning.is_none() {
        k.enable_thinking = Some(false);
        return true;
    }
    if k.reasoning.is_none() {
        // Exclusive: gateways reject contradictory knob combos, so the
        // strongest rung travels alone.
        k.reasoning = Some(false);
        k.reasoning_effort = None;
        k.enable_thinking = None;
        return true;
    }
    if !k.roomier && content_chars > 0 {
        k.roomier = true;
        k.max_tokens = k.max_tokens.saturating_mul(2);
        k.reasoning_effort = None;
        k.enable_thinking = None;
        k.reasoning = None;
        return true;
    }
    if content_chars == 0 && !k.shrunk && k.max_tokens > 1024 {
        k.shrunk = true;
        k.max_tokens = (k.max_tokens / 2).max(1024);
        k.reasoning = Some(false);
        k.reasoning_effort = None;
        k.enable_thinking = None;
        return true;
    }
    false
}

fn wire_body(model: &str, req: &Request, k: &WireKnobs) -> serde_json::Value {
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
            o
        })
        .collect();
    let mut b = serde_json::json!({
        "model": model,
        "messages": messages,
        "max_tokens": k.max_tokens,
    });
    if let Some(e) = &k.reasoning_effort {
        b["reasoning_effort"] = serde_json::Value::String(e.clone());
    }
    if let Some(t) = k.enable_thinking {
        b["enable_thinking"] = serde_json::Value::Bool(t);
    }
    if let Some(r) = k.reasoning {
        b["reasoning"] = serde_json::Value::Bool(r);
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
            })?
            .api_key;
        let mut call = self
            .http
            .post(format!("{}/chat/completions", self.endpoint));
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
            return Err(OnceFail {
                msg,
                after,
                retryable,
                truncated: false,
                content_chars: 0,
            });
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| OnceFail {
            msg: e.to_string(),
            after: None,
            retryable: true,
            truncated: false,
            content_chars: 0,
        })?;
        parse_body(&v, model, k.max_tokens).map_err(|msg| {
            let truncated = msg.contains("finish_reason=length");
            let content_chars = truncated_content_chars(&msg);
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
                msg,
                after: None,
                retryable,
                truncated,
                content_chars,
            }
        })
    }
}

/// 503 from serverless GPU endpoints usually means cold start (scale-from-zero
/// takes minutes for big models), not rejection. Worth out-waiting; other
/// errors keep the short ladder.
fn is_cold_start(msg: &str) -> bool {
    msg.contains("503")
}

fn truncated_content_chars(text: &str) -> usize {
    let Some(i) = text.find("content=") else {
        return 0;
    };
    let rest = &text[i + "content=".len()..];
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

/// Parse one chat/completions body. Truncation and empty answers are errors
/// (never empty answers): the retry ladder needs the counts to decide.
fn parse_body(v: &serde_json::Value, model: &str, max_tokens: usize) -> Result<Response, String> {
    let resp: ChatResp =
        serde_json::from_value(v.clone()).map_err(|e| format!("bad chat body: {e}"))?;
    let choice = resp.choices.into_iter().next().ok_or_else(|| {
        format!("empty content from {model} (finish_reason=None; reasoning_content=0 chars); the endpoint shipped no text")
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
    let (input, output, cache_read, reasoning_tok) = resp
        .usage
        .map(|u| {
            (
                u.prompt_tokens,
                u.completion_tokens,
                u.prompt_tokens_details
                    .map(|d| d.cached_tokens)
                    .unwrap_or(0),
                u.completion_tokens_details
                    .map(|d| d.reasoning_tokens)
                    .filter(|&n| n > 0),
            )
        })
        .unwrap_or((0, 0, 0, None));
    if stop == StopReason::MaxTokens {
        return Err(format!(
            "output truncated at {max_tokens} tokens (finish_reason=length; content={} chars; reasoning_content={} chars); raise max_tokens",
            text.trim().len(),
            reasoning.trim().len()
        ));
    }
    if text.trim().is_empty() && message.tool_calls.is_empty() {
        return Err(format!(
            "empty content from {model} (finish_reason={finish:?}; reasoning_content={} chars); the endpoint shipped no text",
            reasoning.trim().len()
        ));
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
            cost_usd: None,
        },
        latency_ms: 0,
        attempts: 1,
        raw_stop_reason: finish,
    })
}

/// Project one SSE transcript into Start/Delta/Done. One frame over the cap
/// fails the stream; deltas accumulate content, tool-arg fragments join at Done.
fn sse_to_events(raw: &str) -> Result<Vec<StreamEvent>, LlmError> {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut ids: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut arg_frags: Vec<String> = Vec::new();
    let mut finish: Option<String> = None;
    for line in raw.lines() {
        let t = line.trim();
        let Some(payload) = t.strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        if payload.len() > MAX_SSE_FRAME_BYTES {
            return Err(LlmError::Transport("sse frame too large".into()));
        }
        let v: serde_json::Value =
            serde_json::from_str(payload).map_err(|e| LlmError::Transport(e.to_string()))?;
        let choice = v
            .pointer("/choices/0")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if let Some(fr) = choice
            .pointer("/finish_reason")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty() && *s != "null")
        {
            finish = Some(fr.to_string());
        }
        if let Some(c) = choice.pointer("/delta/content").and_then(|x| x.as_str()) {
            content.push_str(c);
        }
        if let Some(c) = choice
            .pointer("/delta/reasoning_content")
            .and_then(|x| x.as_str())
        {
            reasoning.push_str(c);
        }
        if let Some(arr) = choice
            .pointer("/delta/tool_calls")
            .and_then(|x| x.as_array())
            .cloned()
        {
            for tc in arr {
                let idx = tc.pointer("/index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                while ids.len() <= idx {
                    ids.push(String::new());
                    names.push(String::new());
                    arg_frags.push(String::new());
                }
                if let Some(id) = tc.pointer("/id").and_then(|x| x.as_str()) {
                    if !id.is_empty() {
                        ids[idx] = id.to_string();
                    }
                }
                if let Some(n) = tc.pointer("/function/name").and_then(|x| x.as_str()) {
                    if !n.is_empty() {
                        names[idx] = n.to_string();
                    }
                }
                if let Some(a) = tc.pointer("/function/arguments").and_then(|x| x.as_str()) {
                    arg_frags[idx].push_str(a);
                }
            }
        }
    }
    let mut calls = Vec::new();
    for i in 0..ids.len() {
        let raw_args = if arg_frags[i].trim().is_empty() {
            "{}".to_string()
        } else {
            arg_frags[i].clone()
        };
        calls.push(ToolCallRef {
            id: if ids[i].is_empty() {
                format!("call-{i}")
            } else {
                ids[i].clone()
            },
            name: names[i].clone(),
            args: parse_streaming_json(&raw_args),
        });
    }
    let message = provider_core::AssistantMessage {
        content: content.clone(),
        tool_calls: calls,
        thinking: (!reasoning.trim().is_empty()).then_some(reasoning),
    };
    let mut events = vec![StreamEvent::Start {
        partial: provider_core::AssistantMessage {
            content: String::new(),
            tool_calls: Vec::new(),
            thinking: None,
        },
    }];
    if !content.is_empty() {
        events.push(StreamEvent::Delta {
            index: 0,
            text: content,
        });
    }
    let _ = StreamReducer::new(); // reducer owns cumulative downstream; see tests
    events.push(StreamEvent::Done {
        message: {
            let _ = &finish;
            message
        },
    });
    Ok(events)
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
                },
                ProviderMessage {
                    role: "user".into(),
                    content: "hi".into(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
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
                },
                ProviderMessage {
                    role: "tool".into(),
                    content: "ok".into(),
                    tool_calls: Vec::new(),
                    tool_call_id: Some("c1".into()),
                },
            ],
            tools: vec![],
            max_tokens: 64,
            thinking: Thinking::Auto,
            extras: serde_json::Value::Null,
        };
        let b = wire_body("m", &req, &WireKnobs::from_req(&req));
        assert_eq!(b["messages"][0]["tool_calls"][0]["id"], "c1");
        assert_eq!(b["messages"][0]["tool_calls"][0]["type"], "function");
        assert_eq!(b["messages"][1]["tool_call_id"], "c1");
    }

    #[test]
    fn request_mapping_snapshot_incl_tools() {
        let req = req_with_tool();
        let k = WireKnobs::from_req(&req);
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
        assert!(b.get("reasoning").is_none());
        // Thinking::Off maps to reasoning:false, nothing else.
        let mut off = req.clone();
        off.thinking = Thinking::Off;
        let b2 = wire_body("m", &off, &WireKnobs::from_req(&off));
        assert_eq!(b2["reasoning"], false);
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
        assert!(e.contains("finish_reason=length"), "{e}");
        assert!(e.contains("content=4 chars"), "{e}");
        assert!(e.contains("reasoning_content=3 chars"), "{e}");
        // Empty answer is also an error, never Ok("").
        let empty = serde_json::json!({
            "choices": [{"message": {"content": null}, "finish_reason": "stop"}]
        });
        let e2 = parse_body(&empty, "m", 64).unwrap_err();
        assert!(e2.contains("empty content"), "{e2}");
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

    #[tokio::test]
    async fn sse_multi_chunk_reduces() {
        std::env::set_var("TEST_OAI_KEY_R3", "k");
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hel\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"a.rs\\\"}\" }}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let (ep, _) = serve(vec![Canned {
            status: 200,
            headers: vec![("Content-Type".into(), "text/event-stream".into())],
            body: sse.into(),
        }]);
        let c = client_on(&ep, "TEST_OAI_KEY_R3");
        let mut req = req_with_tool();
        req.tools.clear();
        let stream = c.stream("m", &req).await.unwrap();
        let events: Vec<StreamEvent> = stream.collect().await;
        let mut red = StreamReducer::new();
        let mut deltas = 0;
        for ev in events {
            if matches!(ev, StreamEvent::Delta { .. }) {
                deltas += 1;
            }
            red.apply(ev).unwrap();
        }
        assert!(deltas >= 1, "content must arrive as deltas");
        let msg = red.into_message().unwrap();
        assert_eq!(msg.content, "hello");
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].args["path"], "a.rs");
        assert!(red_error_free(&msg));
    }

    fn red_error_free(msg: &provider_core::AssistantMessage) -> bool {
        !msg.content.is_empty() || !msg.tool_calls.is_empty()
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
    fn ladder_climbs_then_roomier_or_shrink() {
        let mut k = WireKnobs {
            max_tokens: 1000,
            reasoning_effort: None,
            enable_thinking: None,
            reasoning: None,
            roomier: false,
            shrunk: false,
        };
        assert!(apply_ladder(&mut k, 50));
        assert_eq!(k.reasoning_effort.as_deref(), Some("low"));
        assert!(apply_ladder(&mut k, 50));
        assert_eq!(k.enable_thinking, Some(false));
        assert!(apply_ladder(&mut k, 50));
        assert_eq!(k.reasoning, Some(false));
        assert!(apply_ladder(&mut k, 50));
        assert!(k.roomier && k.max_tokens == 2000);
        // Zero-content path shrinks instead of doubling.
        let mut z = WireKnobs {
            max_tokens: 8000,
            reasoning_effort: None,
            enable_thinking: None,
            reasoning: Some(false),
            roomier: false,
            shrunk: false,
        };
        assert!(apply_ladder(&mut z, 0));
        assert!(z.shrunk && z.max_tokens == 4000);
        assert!(!z.roomier);
    }

    #[test]
    fn capabilities_send_finish_reason() {
        let c = OpenAiCompat::new("http://x", EndpointProfile::default());
        assert!(c.capabilities("m").sends_finish_reason);
    }

    #[test]
    fn sse_caps_are_the_stub_values() {
        assert_eq!(MODEL_RESPONSE_IDLE_TIMEOUT_SECS, 30 * 60);
        assert_eq!(MAX_SSE_FRAME_BYTES, 256 << 20);
    }

    #[test]
    fn extras_merge_and_profile_threshold_default() {
        assert_eq!(EndpointProfile::default().emission_threshold_chars, 12_000);
        let mut req = req_with_tool();
        req.extras = serde_json::json!({"temperature": 0});
        let k = WireKnobs::from_req(&req);
        let b = wire_body("m", &req, &k);
        assert_eq!(b["temperature"], 0);
        let _ = HashMap::<String, String>::new();
    }
}
