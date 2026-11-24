use async_trait::async_trait;
use provider_core::{
    classify_error, parse_retry_after, Credentials, Delta, ErrorClass, LlmClient, LlmError,
    Request, Response, Usage,
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

use crate::protocol::{parse_body, wire_body, wire_usage, BodyFail, ChatResp, StreamAcc};
use crate::retry::{apply_ladder, is_cold_start, schedule_delay, WireKnobs};

/// Per-endpoint capability profile. Thresholds are re-measured per endpoint,
/// never transferred (v1 profile.rs lesson).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointProfile {
    /// Exact JSON fragment merged into the request body to switch thinking
    /// off. DeepSeek documents `{"thinking":{"type":"disabled"}}`;
    /// a 2026-10-19 api.deepseek.com probe did not detect a change from its
    /// older `enable_thinking` field. None = no known knob: the ladder skips
    /// the rung instead of sending a placebo.
    #[serde(default)]
    pub thinking_off: Option<serde_json::Value>,
    /// Reasoning-effort level sent as `reasoning_effort` on every request;
    /// `None` keeps the body byte-identical to before the knob existed.
    /// DeepSeek docs (2026-11-09) list low/high/max and default high; xhigh
    /// maps to high. A one-prompt api.deepseek.com probe on 2026-10-19 did not
    /// detect an effect, which is historical evidence, not a universal
    /// endpoint claim. The vLLM probe measured 0/722/1060/1149/2976 tokens
    /// for none/low/high/xhigh/max; effort stays per-run, with no global default.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

impl Default for EndpointProfile {
    fn default() -> Self {
        Self {
            thinking_off: Some(serde_json::json!({"thinking": {"type": "disabled"}})),
            reasoning_effort: None,
        }
    }
}

/// Chosen (no anchor): generous ceilings — a gateway that accepts TCP and
/// never answers must wedge `complete()` no longer than this. Re-anchor when
/// a measured task exceeds 600 s; connect is always a misconfiguration wait.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Adapter-internal credential handle: per-call env resolve, never cached
/// past expiry. A static env key cannot recover from 401/403 (fatal); only a
/// refreshable source retries stale credentials.
#[derive(Debug, Clone)]
pub struct CredentialSource {
    env_var: String,
    refreshable: bool,
}

impl CredentialSource {
    pub fn env(var: &str) -> Self {
        Self {
            env_var: var.to_string(),
            refreshable: false,
        }
    }

    pub fn is_refreshable(&self) -> bool {
        self.refreshable
    }

    /// Per-call resolve. Empty or missing is a fatal Auth error, never an
    /// empty bearer token.
    pub fn resolve(&self) -> Result<Credentials, LlmError> {
        let key = std::env::var(&self.env_var).unwrap_or_default();
        if key.trim().is_empty() {
            return Err(LlmError::Auth(format!(
                "missing API key: {} is unset or empty",
                self.env_var
            )));
        }
        Ok(Credentials { api_key: key })
    }

    pub fn check(&self) -> Result<(), LlmError> {
        self.resolve().map(|_| ())
    }
}

pub struct OpenAiCompat {
    endpoint: String,
    profile: EndpointProfile,
    http: reqwest::Client,
    creds: CredentialSource,
    /// Extra per-request headers (e.g. opencode-go's `x-opencode-session`:
    /// a stable per-conversation id for routing + prompt caching).
    headers: Vec<(String, String)>,
    /// Whole-request ceiling: without it a silent gateway wedges `complete()`
    /// forever (the agent loop consults its wallclock budget only at step
    /// head, never mid-request).
    request_timeout: Duration,
}

impl OpenAiCompat {
    pub fn new(endpoint: &str, profile: EndpointProfile) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            profile,
            http: reqwest::Client::builder()
                .user_agent(format!("rof/{}", env!("CARGO_PKG_VERSION")))
                .connect_timeout(CONNECT_TIMEOUT)
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            creds: CredentialSource::env("OPENAI_API_KEY"),
            headers: Vec::new(),
            request_timeout: REQUEST_TIMEOUT,
        }
    }

    /// Override the whole-request ceiling (tests, slow endpoints). Zero
    /// construction-site changes elsewhere: the default stays `REQUEST_TIMEOUT`.
    #[cfg(test)]
    pub fn with_request_timeout(mut self, d: Duration) -> Self {
        self.request_timeout = d;
        self
    }

    pub fn with_key_env(mut self, env: &str) -> Self {
        let refreshable = self.creds.refreshable;
        self.creds = CredentialSource {
            env_var: env.to_string(),
            refreshable,
        };
        self
    }

    /// Mark the credential source refreshable: stale 401/403 bodies retry as
    /// `AuthStale` instead of failing as fatal Auth.
    pub fn with_refreshable(mut self, refreshable: bool) -> Self {
        self.creds.refreshable = refreshable;
        self
    }

    /// Fail fast at startup when the key is missing or empty.
    pub fn check_credentials(&self) -> Result<(), LlmError> {
        self.creds.check()
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
        self.run(model, req, None).await
    }

    /// SSE streaming (`stream: true`). HTTP errors and truncation go through
    /// the same ladder as `complete`; a stream that ends before `[DONE]` or a
    /// finish_reason is a retryable error. Deltas are display-only: a retry
    /// replays text from the start, so the returned `Response` is the truth.
    async fn complete_streaming(
        &self,
        model: &str,
        req: &Request,
        sink: &mut (dyn FnMut(Delta) + Send),
    ) -> Result<Response, LlmError> {
        self.run(model, req, Some(sink)).await
    }
}

impl OpenAiCompat {
    async fn run(
        &self,
        model: &str,
        req: &Request,
        mut sink: Option<&mut (dyn FnMut(Delta) + Send)>,
    ) -> Result<Response, LlmError> {
        let t = Instant::now();
        let mut knobs = WireKnobs::from_req(req, &self.profile);
        // ponytail: retry lives here, not in every caller. Free tiers 429 often.
        // Cold-start extension: 503s get up to 8 attempts with 60s sleeps
        // (~4 min, covers Modal scale-from-zero); other retryable errors keep
        // 5; truncated attempts follow the ladder until it has no rung left.
        // A server `Retry-After` above `RETRY_AFTER_CAP` is terminal instead,
        // and the computed 2^attempt backoff carries 0–25% jitter so locked-
        // step workers do not stampede (pi provider-retry.ts:44-58,66).
        let mut last_err = LlmError::Transport("no attempts".into());
        let mut last_label = "no attempts".to_string();
        let mut last_after: Option<Duration> = None;
        // Billed attempts are metered: sum what each carried so the error
        // path never loses a re-send's usage. Only attempts that RETURNED
        // usage are billed (completed, or truncated then rejected);
        // pre-generation HTTP failures carry no usage and are not charged.
        let mut usage_acc: Option<Usage> = None;
        let mut last_retryable = false;
        let mut attempt = 0u32;
        loop {
            if attempt > 0 {
                let Some(d) = schedule_delay(last_after.take(), &last_label, attempt) else {
                    break;
                };
                tokio::time::sleep(d).await;
            }
            match self
                .once(
                    model,
                    req,
                    &knobs,
                    sink.as_mut()
                        .map(|s| &mut **s as &mut (dyn FnMut(Delta) + Send)),
                )
                .await
            {
                Ok(mut r) => {
                    r.latency_ms = t.elapsed().as_millis() as u64;
                    r.attempts = u64::from(attempt) + 1;
                    // Ladder-recovered attempts were billed too: carry what
                    // the failed re-sends cost so metering can count it.
                    r.retry_usage = usage_acc.take().map(Box::new);
                    return Ok(r);
                }
                Err(f) => {
                    last_label = f.msg.clone();
                    last_err = *f.error;
                    last_retryable = f.retryable;
                    usage_acc = match (usage_acc, f.usage.map(|u| *u)) {
                        (Some(a), Some(n)) => Some(a.plus(&n)),
                        (a, n) => a.or(n),
                    };
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
            source: Box::new(last_err),
            usage: usage_acc,
            // The retryable ladder ran out of rungs: the outer loop must not
            // re-run it. Non-retryable failures report false.
            exhausted: last_retryable,
        })
    }
}

struct OnceFail {
    /// Typed failure for the final `Metered` source. Boxed: keeps the
    /// `Result` small (the failure path is cold).
    error: Box<LlmError>,
    /// Short label (`CODE: body-head`) driving delay and cold-start checks.
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

/// Stale-credential text. Only a refreshable source may retry it; a static
/// env key treats the same body as fatal Auth.
fn is_stale_msg(s: &str) -> bool {
    classify_error(&LlmError::Transport(s.to_string())) == ErrorClass::AuthStale
}

fn is_retryable(e: &LlmError, truncated: bool) -> bool {
    truncated
        || matches!(
            classify_error(e),
            ErrorClass::Retryable | ErrorClass::Overload | ErrorClass::AuthStale
        )
}

impl OpenAiCompat {
    async fn once(
        &self,
        model: &str,
        req: &Request,
        k: &WireKnobs,
        sink: Option<&mut (dyn FnMut(Delta) + Send)>,
    ) -> Result<Response, OnceFail> {
        // Env resolve per attempt, never cached past expiry. Empty is fatal
        // before any HTTP round trip.
        let key = self.creds.resolve().map_err(|e| OnceFail {
            msg: format!("{e:?}"),
            error: Box::new(e),
            after: None,
            retryable: false,
            truncated: false,
            content_chars: 0,
            reasoning_chars: 0,
            usage: None,
        })?;
        let mut call = self
            .http
            .post(format!("{}/chat/completions", self.endpoint));
        // Per-request ceiling from the field: `RequestBuilder::timeout`
        // replaces the client default for this call, so `with_request_timeout`
        // is real. Default equals `REQUEST_TIMEOUT` — byte-identical without it.
        call = call.timeout(self.request_timeout);
        for (name, value) in &self.headers {
            call = call.header(name.as_str(), value.as_str());
        }
        call = call.bearer_auth(key.api_key);
        let mut body = wire_body(model, req, k);
        if sink.is_some() {
            body["stream"] = true.into();
            body["stream_options"] = serde_json::json!({"include_usage": true});
        }
        let resp = call.json(&body).send().await.map_err(|e| {
            // reqwest's Display stops at "error sending request for url
            // (...)"; the timeout only lives in the source chain. Walk it
            // so forensics can tell a hang from connection-refused. The
            // chain of a send error never contains "503" (statuses take
            // the status branch), so `is_cold_start` is unaffected.
            let mut msg = e.to_string();
            let mut src = std::error::Error::source(&e);
            while let Some(x) = src {
                msg.push_str(": ");
                msg.push_str(&x.to_string());
                src = x.source();
            }
            OnceFail {
                msg: msg.clone(),
                error: Box::new(LlmError::Transport(msg)),
                after: None,
                retryable: true,
                truncated: false,
                content_chars: 0,
                reasoning_chars: 0,
                usage: None,
            }
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
            let status = code.as_u16();
            // Some gateways bill and report usage on an error body; carry it.
            let usage = serde_json::from_str::<ChatResp>(&text)
                .ok()
                .and_then(|r| wire_usage(r.usage, model))
                .map(Box::new);
            // 401/403 on a static env key is fatal Auth, never retried. Only
            // a refreshable source retries stale bodies as AuthStale.
            if status == 401 || status == 403 {
                if self.creds.is_refreshable() && is_stale_msg(&text) {
                    let error = Box::new(LlmError::Transport(msg.clone()));
                    return Err(OnceFail {
                        msg,
                        error,
                        after,
                        retryable: true,
                        truncated: false,
                        content_chars: 0,
                        reasoning_chars: 0,
                        usage,
                    });
                }
                let error = Box::new(LlmError::Auth(msg.clone()));
                return Err(OnceFail {
                    msg,
                    error,
                    after: None,
                    retryable: false,
                    truncated: false,
                    content_chars: 0,
                    reasoning_chars: 0,
                    usage,
                });
            }
            let error = Box::new(LlmError::Http {
                status,
                retry_after: after,
                body: short,
            });
            let retryable = is_retryable(&error, false);
            return Err(OnceFail {
                msg,
                error,
                after,
                retryable,
                truncated: false,
                content_chars: 0,
                reasoning_chars: 0,
                usage,
            });
        }
        let v: serde_json::Value = match sink {
            Some(sink) => read_stream(resp, sink).await?,
            None => resp
                .json()
                .await
                .map_err(|e| transport_fail(e.to_string()))?,
        };
        parse_body(&v, model, k.max_tokens).map_err(
            |BodyFail {
                 msg,
                 usage,
                 truncated,
                 content_chars,
                 reasoning_chars,
             }| {
                let error = Box::new(LlmError::Transport(msg.clone()));
                let retryable = is_retryable(&error, truncated);
                OnceFail {
                    msg,
                    error,
                    after: None,
                    retryable,
                    truncated,
                    content_chars,
                    reasoning_chars,
                    usage: usage.map(Box::new),
                }
            },
        )
    }
}

fn transport_fail(msg: String) -> OnceFail {
    OnceFail {
        error: Box::new(LlmError::Transport(msg.clone())),
        msg,
        after: None,
        retryable: true,
        truncated: false,
        content_chars: 0,
        reasoning_chars: 0,
        usage: None,
    }
}

/// Apply one SSE line; true on `data: [DONE]`.
fn sse_line(line: &[u8], acc: &mut StreamAcc, sink: &mut (dyn FnMut(Delta) + Send)) -> bool {
    let line = String::from_utf8_lossy(line);
    let Some(data) = line.trim().strip_prefix("data:") else {
        return false;
    };
    let data = data.trim();
    if data == "[DONE]" {
        return true;
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
        acc.push(&v, sink);
    }
    false
}

/// Read an SSE body into a non-streaming-shaped JSON body. Comments, `event:`
/// lines and unparseable chunks are skipped. EOF with neither `[DONE]` nor a
/// finish_reason is a cut stream: retryable, never a truncated success.
async fn read_stream(
    mut resp: reqwest::Response,
    sink: &mut (dyn FnMut(Delta) + Send),
) -> Result<serde_json::Value, OnceFail> {
    let mut acc = StreamAcc::default();
    let mut buf: Vec<u8> = Vec::new();
    let mut done = false;
    while !done {
        let chunk = resp
            .chunk()
            .await
            .map_err(|e| transport_fail(format!("stream broke: {e}")))?;
        let at_eof = chunk.is_none();
        if let Some(c) = chunk {
            buf.extend_from_slice(&c);
        }
        // At EOF an unterminated last line still counts.
        while let Some(i) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=i).collect();
            done = sse_line(&line, &mut acc, sink);
            if done {
                break;
            }
        }
        if at_eof {
            done = done || (!buf.is_empty() && sse_line(&buf, &mut acc, sink));
            break;
        }
    }
    if !done && acc.finish.is_none() {
        return Err(transport_fail(
            "stream ended before [DONE] or finish_reason".into(),
        ));
    }
    Ok(acc.into_body())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retry::MAX_TOKENS_CEILING;
    use crate::test_support::{
        client_on, ok_body, req_with_tool, serve, sse_body, trunc_body, Canned,
    };
    use provider_core::ProviderMessage;
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
            LlmError::Metered { source, .. } => {
                let s = format!("{source:?}");
                assert!(s.contains("429"), "{s}");
            }
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
                ..
            } => {
                let s = format!("{source:?}");
                assert!(s.contains("finish_reason=length"), "{s}");
                assert_eq!((u.input, u.output), (5, 64));
            }
            other => panic!("expected Metered with usage, got {other:?}"),
        }
    }
    #[tokio::test]
    async fn unauthorized_is_fatal_without_retry() {
        std::env::set_var("TEST_OAI_KEY_401", "k");
        let (ep, bodies) = serve(vec![Canned {
            status: 401,
            headers: vec![],
            body: r#"{"error": {"message": "invalid_api_key"}}"#.into(),
        }]);
        let c = client_on(&ep, "TEST_OAI_KEY_401");
        let mut req = req_with_tool();
        req.tools.clear();
        let err = c.complete("m", &req).await.unwrap_err();
        assert_eq!(bodies.lock().unwrap().len(), 1, "401: no re-send");
        match err {
            LlmError::Metered {
                source, exhausted, ..
            } => {
                assert!(
                    matches!(&*source, LlmError::Auth(_)),
                    "static 401 is Auth, got {source:?}"
                );
                assert!(!exhausted, "fatal: the ladder never ran");
                assert_eq!(classify_error(&source), provider_core::ErrorClass::Fatal);
            }
            other => panic!("expected Metered, got {other:?}"),
        }
    }
    #[tokio::test]
    async fn missing_key_fails_fast_with_zero_http_calls() {
        std::env::remove_var("TEST_OAI_KEY_ABSENT");
        let (ep, bodies) = serve(vec![Canned {
            status: 200,
            headers: vec![],
            body: ok_body("never"),
        }]);
        let c = client_on(&ep, "TEST_OAI_KEY_ABSENT");
        assert!(
            matches!(c.check_credentials(), Err(LlmError::Auth(_))),
            "startup check fails fast"
        );
        let mut req = req_with_tool();
        req.tools.clear();
        let err = c.complete("m", &req).await.unwrap_err();
        assert_eq!(bodies.lock().unwrap().len(), 0, "no HTTP without a key");
        match err {
            LlmError::Metered {
                source, exhausted, ..
            } => {
                assert!(matches!(&*source, LlmError::Auth(_)), "got {source:?}");
                assert!(!exhausted);
            }
            other => panic!("expected Metered, got {other:?}"),
        }
        std::env::set_var("TEST_OAI_KEY_ABSENT", "k");
        assert!(c.check_credentials().is_ok());
        std::env::remove_var("TEST_OAI_KEY_ABSENT");
    }
    #[tokio::test]
    async fn stale_body_retries_only_for_refreshable_source() {
        std::env::set_var("TEST_OAI_KEY_STALE", "k");
        let stale = || Canned {
            status: 401,
            headers: vec![("Retry-After".into(), "0".into())],
            body: "token expired, refresh needed".into(),
        };
        // Static env key: fatal, one send.
        let (ep, bodies) = serve(vec![stale()]);
        let c = client_on(&ep, "TEST_OAI_KEY_STALE");
        let mut req = req_with_tool();
        req.tools.clear();
        let err = c.complete("m", &req).await.unwrap_err();
        assert_eq!(bodies.lock().unwrap().len(), 1);
        match err {
            LlmError::Metered { source, .. } => {
                assert_eq!(classify_error(&source), provider_core::ErrorClass::Fatal)
            }
            other => panic!("expected Metered, got {other:?}"),
        }
        // Refreshable source: the same body retries and recovers.
        let (ep2, bodies2) = serve(vec![
            stale(),
            Canned {
                status: 200,
                headers: vec![],
                body: ok_body("back"),
            },
        ]);
        let c2 = client_on(&ep2, "TEST_OAI_KEY_STALE").with_refreshable(true);
        let r = c2.complete("m", &req).await.unwrap();
        assert_eq!(r.message.content, "back");
        assert_eq!(r.attempts, 2);
        assert_eq!(bodies2.lock().unwrap().len(), 2);
    }
    #[tokio::test]
    async fn rate_limited_source_carries_status_and_exhausted() {
        std::env::set_var("TEST_OAI_KEY_429X", "k");
        let canned: Vec<Canned> = (0..5)
            .map(|_| Canned {
                status: 429,
                headers: vec![("Retry-After".into(), "0".into())],
                body: "rate limited".into(),
            })
            .collect();
        let (ep, bodies) = serve(canned);
        let c = client_on(&ep, "TEST_OAI_KEY_429X");
        let mut req = req_with_tool();
        req.tools.clear();
        let err = c.complete("m", &req).await.unwrap_err();
        assert_eq!(bodies.lock().unwrap().len(), 5, "1 + 4 retries");
        match err {
            LlmError::Metered {
                source, exhausted, ..
            } => match &*source {
                LlmError::Http {
                    status,
                    retry_after,
                    ..
                } => {
                    assert_eq!(*status, 429);
                    assert_eq!(*retry_after, Some(Duration::from_secs(0)));
                    assert!(exhausted, "the ladder ran out of rungs");
                    assert_eq!(
                        classify_error(&source),
                        provider_core::ErrorClass::Retryable
                    );
                }
                other => panic!("expected Http source, got {other:?}"),
            },
            other => panic!("expected Metered, got {other:?}"),
        }
    }
    /// A gateway that accepts TCP and never answers must surface as a
    /// bounded, retryable Transport timeout riding the ladder to a terminal
    /// Metered error — never an infinite await.
    #[tokio::test(start_paused = true)]
    async fn hanging_endpoint_times_out_as_retryable_transport_error() {
        std::env::set_var("TEST_OAI_KEY_HANG", "k");
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ep = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            // Accept and hold every connection open, never writing: the wedge.
            for stream in l.incoming() {
                std::mem::forget(stream);
            }
        });
        let c =
            client_on(&ep, "TEST_OAI_KEY_HANG").with_request_timeout(Duration::from_millis(100));
        let mut req = req_with_tool();
        req.tools.clear();
        let t0 = tokio::time::Instant::now();
        let err = c.complete("m", &req).await.unwrap_err();
        match err {
            LlmError::Metered {
                source, exhausted, ..
            } => {
                let s = source.to_string();
                assert!(s.contains("operation timed out"), "{s}");
                assert!(exhausted, "the 1+4 ladder ran out: {s}");
                // A timeout hits no fatal keyword: it rides the retry ladder.
                assert_eq!(
                    classify_error(&source),
                    provider_core::ErrorClass::Retryable
                );
            }
            other => panic!("expected Metered, got {other:?}"),
        }
        // Paused-clock pin that the override applied: virtual elapsed stays
        // far under the 600s client default (a vacuous setter would run the
        // ladder at 5x600s ~= 50min virtual; the override runs ~30s virtual
        // = 5x100ms + the 2/4/8/16s rungs).
        assert!(
            t0.elapsed() < Duration::from_secs(600),
            "the 100ms override must bound the request: {:?}",
            t0.elapsed()
        );
    }
    #[tokio::test]
    async fn cold_start_503_retries_and_recovers() {
        std::env::set_var("TEST_OAI_KEY_503", "k");
        let (ep, bodies) = serve(vec![
            Canned {
                status: 503,
                headers: vec![("Retry-After".into(), "0".into())],
                body: "Service Unavailable".into(),
            },
            Canned {
                status: 200,
                headers: vec![],
                body: ok_body("warm"),
            },
        ]);
        let c = client_on(&ep, "TEST_OAI_KEY_503");
        let mut req = req_with_tool();
        req.tools.clear();
        let r = c.complete("m", &req).await.unwrap();
        assert_eq!(r.message.content, "warm");
        assert_eq!(r.attempts, 2);
        assert_eq!(bodies.lock().unwrap().len(), 2);
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
                exhausted,
            } => {
                let s = format!("{source:?}");
                assert!(s.contains("429"), "{s}");
                assert!(exhausted, "the 1+4 ladder ran out of rungs");
                // Every failed attempt was billed the prompt: the error sums them.
                assert_eq!(u.input, 35);
            }
            other => panic!("expected Metered with usage, got {other:?}"),
        }
    }

    fn delta(d: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"choices": [{"delta": d}]})
    }
    fn ok_sse(body: String) -> Canned {
        Canned {
            status: 200,
            headers: vec![("Content-Type".into(), "text/event-stream".into())],
            body,
        }
    }
    async fn stream(
        key: &str,
        canned: Vec<Canned>,
    ) -> (Result<Response, LlmError>, Vec<Delta>, Vec<String>) {
        std::env::set_var(key, "k");
        let (ep, bodies) = serve(canned);
        let mut req = req_with_tool();
        req.tools.clear();
        let mut seen = Vec::new();
        let r = client_on(&ep, key)
            .complete_streaming("m", &req, &mut |d| seen.push(d))
            .await;
        let b = bodies.lock().unwrap().clone();
        (r, seen, b)
    }
    #[tokio::test]
    async fn streamed_text_arrives_in_order_with_usage_chunk() {
        let chunks = [
            delta(serde_json::json!({"content": "Hel"})),
            delta(serde_json::json!({"content": "lo "})),
            delta(serde_json::json!({"content": "you"})),
            serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
            serde_json::json!({"choices": [], "usage": {"prompt_tokens": 3, "completion_tokens": 4}}),
        ];
        let (r, seen, bodies) =
            stream("TEST_OAI_KEY_S1", vec![ok_sse(sse_body(&chunks, true))]).await;
        let r = r.unwrap();
        let t = |s: &str| Delta::Text(s.into());
        assert_eq!(seen, vec![t("Hel"), t("lo "), t("you")]);
        assert_eq!(r.message.content, "Hello you");
        assert_eq!((r.usage.input, r.usage.output), (3, 4));
        assert_eq!(r.stop, provider_core::StopReason::Stop);
        let sent: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["stream_options"]["include_usage"], true);
    }
    #[tokio::test]
    async fn streamed_tool_call_fragments_concatenate_per_index() {
        let tc = |v: serde_json::Value| delta(serde_json::json!({"tool_calls": [v]}));
        let chunks = [
            tc(
                serde_json::json!({"index": 0, "id": "c9", "function": {"name": "read", "arguments": ""}}),
            ),
            tc(serde_json::json!({"index": 0, "function": {"arguments": "{\"pa"}})),
            tc(serde_json::json!({"index": 0, "function": {"arguments": "th\": \"a."}})),
            tc(serde_json::json!({"index": 0, "function": {"arguments": "rs\"}"}})),
            serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        ];
        let (r, seen, _) = stream("TEST_OAI_KEY_S2", vec![ok_sse(sse_body(&chunks, true))]).await;
        let r = r.unwrap();
        assert!(seen.is_empty());
        assert_eq!(r.message.tool_calls.len(), 1);
        let c = &r.message.tool_calls[0];
        assert_eq!((c.id.as_str(), c.name.as_str()), ("c9", "read"));
        assert_eq!(c.args, serde_json::json!({"path": "a.rs"}));
        assert_eq!(r.stop, provider_core::StopReason::ToolUse);
    }
    #[tokio::test]
    async fn streamed_reasoning_is_carried_without_fabricated_usage() {
        let chunks = [
            delta(serde_json::json!({"reasoning_content": "hm"})),
            delta(serde_json::json!({"reasoning": "m"})),
            delta(serde_json::json!({"content": "ok"})),
            serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
        ];
        let (r, seen, _) = stream("TEST_OAI_KEY_S3", vec![ok_sse(sse_body(&chunks, false))]).await;
        let r = r.unwrap();
        assert_eq!(
            seen,
            vec![
                Delta::Reasoning("hm".into()),
                Delta::Reasoning("m".into()),
                Delta::Text("ok".into())
            ]
        );
        // Same carry as the non-streaming path: reasoning_content wins when set.
        assert_eq!(r.message.thinking.as_deref(), Some("hm"));
        assert_eq!(r.message.content, "ok");
        // No usage chunk: nothing reported, so no cost and no reasoning count.
        assert!(r.usage.cost_usd.is_none() && r.usage.reasoning.is_none());
    }
    // Real time: a paused clock auto-advances during socket IO and fires the
    // 600 s request timeout. Costs one 2-3 s backoff.
    #[tokio::test]
    async fn stream_cut_before_finish_is_retried_not_truncated_success() {
        let cut = sse_body(&[delta(serde_json::json!({"content": "par"}))], false);
        let good = sse_body(
            &[
                delta(serde_json::json!({"content": "whole"})),
                serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
            ],
            true,
        );
        let (r, seen, bodies) =
            stream("TEST_OAI_KEY_S5", vec![ok_sse(cut.clone()), ok_sse(good)]).await;
        let r = r.unwrap();
        assert_eq!((r.message.content.as_str(), r.attempts), ("whole", 2));
        assert_eq!(bodies.len(), 2);
        // Retry replays: display-only deltas saw the cut text too.
        assert_eq!(seen[0], Delta::Text("par".into()));
    }
    #[tokio::test]
    async fn non_streaming_body_has_no_stream_key() {
        std::env::set_var("TEST_OAI_KEY_S6", "k");
        let (ep, bodies) = serve(vec![Canned {
            status: 200,
            headers: vec![],
            body: ok_body("x"),
        }]);
        let mut req = req_with_tool();
        req.tools.clear();
        client_on(&ep, "TEST_OAI_KEY_S6")
            .complete("m", &req)
            .await
            .unwrap();
        let sent: serde_json::Value = serde_json::from_str(&bodies.lock().unwrap()[0]).unwrap();
        assert!(sent.get("stream").is_none() && sent.get("stream_options").is_none());
    }
}
