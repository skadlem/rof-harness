use async_trait::async_trait;
use provider_core::{
    parse_retry_after, Capabilities, Credentials, LlmClient, LlmError, Request, Response, Usage,
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

use crate::protocol::{parse_body, wire_body, wire_usage, BodyFail, ChatResp};
use crate::retry::{
    apply_ladder, chars_after, is_cold_start, merge_usage, schedule_delay, WireKnobs,
};

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

    /// Reserved: always empty, ignores `model` (see `provider-core::Capabilities`).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retry::MAX_TOKENS_CEILING;
    use crate::test_support::{client_on, ok_body, req_with_tool, serve, trunc_body, Canned};
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
}
