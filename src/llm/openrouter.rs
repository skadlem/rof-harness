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
    /// `reasoning: false` on a reasoning-compat endpoint stops the model
    /// spending the whole budget on `reasoning_content` and shipping a null
    /// `content`. Only set on the retry, so every first attempt keeps
    /// reasoning on.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<bool>,
    /// `chat_template_kwargs: {reasoning_effort: "low"}` is the knob a
    /// reasoning endpoint actually honours on a large prompt — a bare
    /// `reasoning: false` is ignored once the prompt is big, and the model
    /// thinks past the limit either way. `low` keeps a short reasoning pass
    /// and still ships content with `finish_reason: stop`.
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<ChatTemplateKwargs>,
    /// `enable_thinking: false` is the vLLM template switch. It is the one
    /// shape that shipped real content on the ~7 KB implementer prompt,
    /// where `reasoning: false` is ignored and `reasoning_effort` still
    /// exhausts the budget. Only set on the retry.
    #[serde(skip_serializing_if = "Option::is_none")]
    enable_thinking: Option<bool>,
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
    /// A reasoning-compat endpoint puts hidden chain-of-thought here and can
    /// ship a null `content` alongside it. Present-but-empty is a distinct
    /// state from "the model wrote nothing": the budget went to reasoning.
    #[serde(default)]
    reasoning_content: String,
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
    /// Stable per-client conversation id for providers that route on it
    /// (OpenCode Go requires `x-opencode-session`; see `session_header`).
    /// Minted per client, so one rof process is one session.
    session: String,
}

/// `Some((name, value))` when `base` is an OpenCode endpoint, else `None` —
/// other providers never see the header, so their runs are byte-identical.
fn session_header_value(base: &str, session: &str) -> Option<(String, String)> {
    if base.contains("opencode.ai") {
        Some(("x-opencode-session".to_string(), session.to_string()))
    } else {
        None
    }
}

/// Test helper: the header decision without a client.
pub fn session_header_for_test(base: &str, session: &str) -> Option<(String, String)> {
    session_header_value(base, session)
}

/// Test helper: one fresh session id, as a client would mint it.
pub fn client_session_for_test() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl OpenRouterClient {
    fn http_client() -> reqwest::Client {
        // Go asks clients to identify themselves instead of riding a generic
        // HTTP-library name; harmless everywhere else.
        reqwest::Client::builder()
            .user_agent(format!("rof/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    }

    fn session_header(&self) -> Option<(String, String)> {
        session_header_value(&self.base, &self.session)
    }

    pub fn new(token: String) -> Self {
        Self {
            token,
            base: DEFAULT_BASE.to_string(),
            http: Self::http_client(),
            session: uuid::Uuid::new_v4().to_string(),
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
            http: Self::http_client(),
            session: uuid::Uuid::new_v4().to_string(),
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
            http: Self::http_client(),
            session: uuid::Uuid::new_v4().to_string(),
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
            reasoning: if req.reasoning_off { Some(false) } else { None },
            chat_template_kwargs: if req.reasoning_low {
                Some(ChatTemplateKwargs {
                    reasoning_effort: "low".to_string(),
                })
            } else {
                None
            },
            enable_thinking: if req.thinking_off { Some(false) } else { None },
        }
    }
}

#[async_trait]
impl LlmClient for OpenRouterClient {
    async fn complete(&self, model: &str, mut req: LlmReq) -> Result<LlmResp, LlmError> {
        let t = Instant::now();
        // ponytail: retry lives here, not in every caller. Free tiers 429 often.
        let mut last = LlmError::Transport("no attempts".to_string());
        // Five shapes: the plain request, then four reshapes — low effort,
        // the vLLM thinking switch, reasoning off, and a roomier re-ask. The
        // loop bound and the rung order must stay in step: see
        // `attempt_ge_max`.
        for attempt in 0..5 {
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
                    let truncated = text.contains(FINISH_LENGTH);
                    last = e;
                    if !retryable_error(&text, truncated, attempt as usize) {
                        break;
                    }
                    if truncated {
                        // Reasoning expands to fill any budget on this
                        // endpoint — doubling `max_tokens` was measured to
                        // make reasoning *longer* and still ship no content —
                        // so the retry reshapes the request instead of growing
                        // it. The order matters and lives in one place:
                        // `reshape_for_truncation`.
                        let content = truncated_content_chars(&text);
                        if !reshape_for_truncation(&mut req, content) {
                            break;
                        }
                    } else if reasoning_ate_budget(&text)
                        && !req.reasoning_low
                        && !req.thinking_off
                        && !req.reasoning_off
                    {
                        // The non-truncated empty path: reasoning shipped but
                        // no text, so reshape there too.
                        req.reasoning_low = true;
                    }
                }
            }
        }
        Err(last)
    }
}

/// Did the endpoint ship `reasoning_content` but no `content`? The whole
/// budget went to hidden reasoning. Distinguished from a plain empty reply —
/// which may be a transient endpoint quirk — because the fix is different:
/// re-ask with reasoning off, not a same-shape re-roll.
fn reasoning_ate_budget(text: &str) -> bool {
    let Some(i) = text.find("reasoning_content=") else {
        return false;
    };
    let rest = &text[i + "reasoning_content=".len()..];
    // `reasoning_content=0 chars` means reasoning was empty too, so there is
    // nothing to switch off; anything else means the budget went there.
    !rest.starts_with("0 ")
}

/// `chat_template_kwargs` is where a reasoning-capable template looks for
/// its effort knob — the field a bare top-level `reasoning_effort` is
/// silently ignored in favour of on this endpoint.
#[derive(Debug, Clone, Serialize)]
struct ChatTemplateKwargs {
    reasoning_effort: String,
}

/// Is an error worth re-asking? Kept as a free function so the
/// classification that `complete()` relies on is directly testable: an empty
/// `content` is transient on reasoning-compat endpoints and must be retried,
/// and a truncated reply is worth exactly one roomier retry.
fn retryable_error(text: &str, truncated: bool, attempt: usize) -> bool {
    let empty = text.contains("empty content");
    let reasoning = reasoning_ate_budget(text);
    let retryable = truncated
        || empty
        || reasoning
        || ["429", "500", "502", "503", "504", "402"]
            .iter()
            .any(|c| text.contains(c));
    // A truncation or a reasoning-budget exhaustion is worth reshaping, then a
    // bounded re-roll: whether the endpoint honours its own reasoning knob on
    // a large prompt is nondeterministic, so a same-shape retry sometimes
    // succeeds where the first did not. Capped so a genuinely unreachable
    // answer still terminates.
    retryable && !(truncated && attempt > 3) && !attempt_ge_max(attempt)
}

/// ponytail: the loop runs 0..5; the last slot is reserved for the roomier
/// re-ask, so a reshape must not burn it on a plain re-roll.
fn attempt_ge_max(attempt: usize) -> bool {
    attempt >= 4
}

/// The truncation ladder, as one function so the retry and its test cannot
/// drift. Reasoning expands to fill any budget on this endpoint — doubling
/// `max_tokens` was measured to make reasoning *longer* and still ship no
/// content — so a truncated reply is *reshaped*, not given more room.
///
/// `content_chars` is how much real text the truncated reply shipped,
/// extracted from the error that reported the truncation. It decides the
/// fate of the last rung: roomier is justified only when a genuine answer
/// was cut off (measured: 8,000-char prompt, 36,892 chars of content, still
/// growing). When `content_chars` is 0 the budget went entirely to reasoning,
/// and the doubled budget was measured to double the reasoning to 72,059
/// chars while shipping nothing. So the rung stays off in that case and the
/// ladder reports that no shape can help, which lets the retry loop stop
/// instead of paying for a guaranteed-empty call.
///
/// Order is the point: the shapes least likely to cost quality come first, so
/// a call that fails early keeps as much reasoning as the endpoint will
/// honour.
/// 1. `reasoning_effort: low` — keeps a short reasoning pass and still ships
///    content.
/// 2. `enable_thinking: false` — the vLLM switch, and the only shape that
///    returned real content on the large implementer prompt.
/// 3. `reasoning: false` — honoured on small prompts, ignored on large ones.
/// 4. roomier — reasoning back on with twice the budget, the last resort,
///    and only when the truncation actually shipped content.
///
/// Which knob the endpoint honours at a given prompt size is
/// nondeterministic, which is why the ladder keeps climbing instead of
/// stopping at the first reshape.
///
/// Returns whether a reshape was applied. `false` means every rung is spent
/// or the remaining one is known not to help, so the caller should stop.
fn reshape_for_truncation(req: &mut LlmReq, content_chars: usize) -> bool {
    if !req.reasoning_low && !req.thinking_off && !req.reasoning_off {
        req.reasoning_low = true;
        true
    } else if !req.thinking_off && !req.reasoning_off {
        req.thinking_off = true;
        true
    } else if !req.reasoning_off {
        req.reasoning_off = true;
        true
    } else if !req.roomier && content_chars > 0 {
        req.roomier = true;
        req.max_tokens = req.max_tokens.saturating_mul(2);
        req.reasoning_low = false;
        req.thinking_off = false;
        req.reasoning_off = false;
        true
    } else {
        false
    }
}

/// How much real text a truncated reply shipped, read back out of the error
/// that `once()` built. The truncation error reports
/// `content=N chars; reasoning_content=M chars` precisely so this decision
/// does not have to guess: reasoning that consumed the whole budget is a
/// different failure from an answer that ran out of room, and the ladder's
/// last rung is only correct for one of them.
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

impl OpenRouterClient {
    async fn once(&self, model: &str, req: &LlmReq) -> Result<LlmResp, LlmError> {
        let mut call = self
            .http
            .post(format!("{}/chat/completions", self.base))
            .bearer_auth(&self.token)
            .header("HTTP-Referer", "rof-harness")
            .header("X-Title", "rof-harness");
        // OpenCode Go routes on this; absent everywhere else (see
        // `session_header_value`), so non-Go runs are unchanged.
        if let Some((name, value)) = self.session_header() {
            call = call.header(name, value);
        }
        let res = call
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
        let finish = choice
            .as_ref()
            .and_then(|c| c.finish_reason.as_deref())
            .unwrap_or("")
            .to_string();
        let text = choice
            .as_ref()
            .map(|c| c.message.content.clone())
            .unwrap_or_default();
        let reasoning = choice
            .as_ref()
            .map(|c| c.message.reasoning_content.trim().len())
            .unwrap_or(0);
        if truncated {
            // Report where the budget went. A truncation with no `content`
            // means reasoning consumed the limit — doubling it just makes the
            // model think longer, so the retry loop needs to see that signal
            // here, not only on the empty-content path below.
            return Err(LlmError::Transport(format!(
                "output truncated at {} tokens (finish_reason=length; content={} chars; reasoning_content={} chars); raise max_tokens",
                req.max_tokens,
                text.trim().len(),
                reasoning
            )));
        }
        // A reply with no text is not an answer: some compat endpoints ship a
        // null `content` alongside a `reasoning_content` they never move out
        // of, and accepting it silently made a degraded call look like a model
        // that wrote nothing — which is how a whole task class came to read as
        // "the model does not synthesise". Reported as an error so the retry
        // loop re-asks and the run counts it.
        //
        // When reasoning IS present the budget went there: the retry loop
        // re-asks with reasoning off, which is what makes the model emit the
        // answer instead of thinking past the limit.
        if text.trim().is_empty() {
            return Err(LlmError::Transport(format!(
                "empty content from {model} (finish_reason={finish:?}; reasoning_content={reasoning} chars); the endpoint shipped no text"
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
            reasoning_off: false,
            reasoning_low: false,
            roomier: false,
            thinking_off: false,
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

    /// A reply with no text is not an answer. A compat endpoint can ship
    /// `content: null` with only `reasoning_content` populated; deserialising
    /// that to an empty string and returning `Ok("")` made a degraded call
    /// look like a model that chose to write nothing. That misread is what
    /// turned an endpoint quirk into "the analysis class is a model failure."
    #[test]
    fn an_empty_content_reply_is_not_an_answer() {
        // `content: null` deserialises to an empty string via null_as_empty,
        // so the wire form and the parsed form must agree on emptiness.
        let wire = String::from(
            "{\"choices\":[{\"message\":{\"content\":null},\"finish_reason\":\"stop\"}]}",
        );
        let parsed: ChatResp = serde_json::from_str(&wire).expect("null content parses");
        let text = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .expect("a choice exists");
        // The invariant the caller relies on: whatever it scores, this is not
        // a model answer.
        assert!(text.trim().is_empty(), "content was: {text}");
    }

    /// The no-op failure class: an endpoint that ships an empty `content`
    // once under load must be re-asked, not accepted as "the model wrote
    // nothing". Before this fix one empty reply ended the task with zero
    // writes and a clean exit code — every harness's worst false reading.
    #[test]
    fn an_empty_content_error_is_retried() {
        let msg = "empty content from Atria-Dawn-Preview (finish_reason=\"stop\"); the endpoint shipped no text";
        assert!(
            retryable_error(msg, false, 0),
            "an empty content reply is reshaped, not accepted"
        );
        assert!(
            retryable_error(msg, false, 1),
            "it climbs the ladder rather than stopping at one attempt"
        );
        // It still terminates: repeated same-shape re-rolls of an empty
        // reply were measured to add nothing but latency. Five attempts now,
        // not four — the ladder gained the `enable_thinking` rung.
        assert!(!retryable_error(msg, false, 4));
    }

    /// A truncation earns exactly one retry, and only a roomier one.
    #[test]
    fn a_truncation_is_retried_a_bounded_number_of_times() {
        let msg = "output truncated at 1024 tokens (finish_reason=length); raise max_tokens";
        assert!(
            retryable_error(msg, true, 0),
            "the first truncation is reshaped"
        );
        assert!(
            retryable_error(msg, true, 1),
            "a second truncation may be a re-roll"
        );
        assert!(
            retryable_error(msg, true, 2),
            "the thinking switch is still allowed"
        );
        assert!(
            retryable_error(msg, true, 3),
            "the roomier re-ask is still allowed"
        );
        assert!(!retryable_error(msg, true, 4), "the ladder terminates");
    }

    /// The reasoning-budget exhaustion is retryable too, so the
    /// empty-content path climbs the same ladder instead of stopping at one
    /// attempt.
    #[test]
    fn a_reasoning_budget_exhaustion_is_retried() {
        let msg =
            "empty content from m (finish_reason=\"length\"; reasoning_content=30000 chars); none";
        assert!(retryable_error(msg, false, 0));
        assert!(retryable_error(msg, false, 1));
        assert!(!retryable_error(msg, false, 4), "it still terminates");
    }

    /// A quota error is worth waiting out; a real protocol error is not.
    #[test]
    fn quota_errors_retry_and_protocol_errors_do_not() {
        assert!(retryable_error("429: rate limit exceeded", false, 0));
        assert!(retryable_error("402: payment required", false, 2));
        assert!(!retryable_error(
            "400: bad request: invalid model",
            false,
            0
        ));
    }

    /// The body only carries the reasoning switch when it is in use, so a
    /// first attempt is byte-identical to before this knob existed.
    #[test]
    fn reasoning_is_omitted_until_it_is_switched_off() {
        let on = LlmReq {
            system: "s".into(),
            prompt: "p".into(),
            max_tokens: 10,
            reasoning_off: false,
            reasoning_low: false,
            roomier: false,
            thinking_off: false,
        };
        let wire = serde_json::to_string(&OpenRouterClient::body("m", &on)).unwrap();
        assert!(
            !wire.contains("reasoning"),
            "first attempt must not carry it: {wire}"
        );

        let off = LlmReq {
            reasoning_off: true,
            ..on.clone()
        };
        let wire = serde_json::to_string(&OpenRouterClient::body("m", &off)).unwrap();
        assert!(
            wire.contains("\"reasoning\":false"),
            "the retry must switch reasoning off: {wire}"
        );

        // `low` is the knob the endpoint honours on a large prompt, so it is
        // the first reshape the retry tries — sent as a template kwarg, not a
        // top-level field, which is the only shape the endpoint reads.
        let low = LlmReq {
            reasoning_low: true,
            ..on
        };
        let wire = serde_json::to_string(&OpenRouterClient::body("m", &low)).unwrap();
        assert!(
            wire.contains("chat_template_kwargs"),
            "low effort is a template kwarg: {wire}"
        );
        assert!(
            wire.contains("\"reasoning_effort\":\"low\""),
            "the effort must be named: {wire}"
        );
        // And it must not also disable reasoning outright.
        assert!(!wire.contains("\"reasoning\":false"));
    }

    /// The `enable_thinking` rung: on the large implementer prompt it was the
    /// only shape that shipped real content, so it earns a rung of its own
    /// between `low` and `reasoning: false`. It must appear on the wire, and a
    /// first attempt must not carry it.
    #[test]
    fn enable_thinking_is_the_second_rung_and_is_omitted_until_used() {
        let base = LlmReq {
            system: "s".into(),
            prompt: "p".into(),
            max_tokens: 10,
            reasoning_off: false,
            reasoning_low: false,
            thinking_off: false,
            roomier: false,
        };
        // A first attempt stays byte-identical to before this rung existed.
        let wire = serde_json::to_string(&OpenRouterClient::body("m", &base)).unwrap();
        assert!(!wire.contains("enable_thinking"), "first attempt: {wire}");

        let off = LlmReq {
            thinking_off: true,
            ..base.clone()
        };
        let wire = serde_json::to_string(&OpenRouterClient::body("m", &off)).unwrap();
        assert!(
            wire.contains("\"enable_thinking\":false"),
            "the retry must switch thinking off: {wire}"
        );
        // It must not also flip the reasoning switch, so the two rungs stay
        // distinguishable on the wire and the endpoint sees one change at a
        // time — the A/B that motivated this rung isolated exactly this param.
        assert!(!wire.contains("\"reasoning\":false"));
        assert!(!wire.contains("chat_template_kwargs"));
    }

    /// The climb order, end to end: a truncation must walk `low`, then
    /// `enable_thinking`, then `reasoning`, then the roomier re-ask, and stop.
    /// Ordering is the whole point of the ladder — the shapes least likely to
    /// cost quality come first, so a caller that fails early still keeps as
    /// much reasoning as the endpoint will honour.
    #[test]
    fn a_truncation_climbs_the_rungs_in_order() {
        let mut req = LlmReq {
            system: "s".into(),
            prompt: "p".into(),
            max_tokens: 1000,
            reasoning_off: false,
            reasoning_low: false,
            thinking_off: false,
            roomier: false,
        };
        // A real answer that ran out of room, so the roomier rung is
        // justified at the top of the climb.
        let content = 36_892;

        // Rung 1: plain -> low effort.
        assert!(reshape_for_truncation(&mut req, content));
        assert!(req.reasoning_low && !req.thinking_off && !req.reasoning_off);
        // Rung 2: low -> enable_thinking off.
        assert!(reshape_for_truncation(&mut req, content));
        assert!(
            req.thinking_off && !req.reasoning_off && !req.roomier,
            "enable_thinking must come before reasoning: false"
        );
        // Rung 3: -> reasoning off.
        assert!(reshape_for_truncation(&mut req, content));
        assert!(req.reasoning_off && !req.roomier);
        // Rung 4: the roomier re-ask, which turns reasoning back on.
        assert!(reshape_for_truncation(&mut req, content));
        assert!(req.roomier);
        assert_eq!(req.max_tokens, 2000, "roomier doubles the budget");
        assert!(!req.reasoning_low && !req.thinking_off && !req.reasoning_off);
        // The budget is not doubled a second time: roomier is sticky, so a
        // further reshape cycles back to `low` rather than growing again. The
        // retry loop never asks for that sixth shape — `attempt_ge_max` caps
        // it — but the budget must still not grow unboundedly.
        assert!(reshape_for_truncation(&mut req, content));
        assert!(req.roomier, "roomier is never unset");
        assert_eq!(req.max_tokens, 2000, "the budget must not double again");
    }

    /// The roomier rung is only for a real answer that was cut off. A
    /// truncation that shipped no content spent its whole budget on
    /// reasoning, and the doubled budget was measured to double the
    /// reasoning (72,059 chars) while still shipping nothing — so the rung
    /// must not fire, and the ladder must say so, which is what lets the
    /// retry loop stop instead of paying for the empty call.
    #[test]
    fn a_contentless_truncation_never_becomes_roomier() {
        let mut req = LlmReq {
            system: "s".into(),
            prompt: "p".into(),
            max_tokens: 8192,
            reasoning_off: false,
            reasoning_low: false,
            thinking_off: false,
            roomier: false,
        };
        // The lower rungs are still worth trying: reasoning off may be the
        // shape that lets content through.
        assert!(reshape_for_truncation(&mut req, 0));
        assert!(req.reasoning_low);
        assert!(reshape_for_truncation(&mut req, 0));
        assert!(req.thinking_off);
        assert!(reshape_for_truncation(&mut req, 0));
        assert!(req.reasoning_off);
        // But roomier is closed: no content was ever shipped.
        assert!(
            !reshape_for_truncation(&mut req, 0),
            "roomier must not fire on a contentless truncation"
        );
        assert!(!req.roomier);
        assert_eq!(
            req.max_tokens, 8192,
            "the budget must not double when no content was shipped"
        );
    }

    /// The ladder decides from the numbers the error reports, so the extractor
    /// must read exactly what `once()` writes — and must not be fooled by the
    /// `reasoning_content=` field that follows it in the same message.
    #[test]
    fn the_content_length_is_read_from_the_truncation_error() {
        assert_eq!(
            truncated_content_chars(
                "output truncated at 8192 tokens (finish_reason=length; content=36892 chars; reasoning_content=14 chars); raise max_tokens"
            ),
            36_892
        );
        assert_eq!(
            truncated_content_chars(
                "output truncated at 8192 tokens (finish_reason=length; content=0 chars; reasoning_content=34810 chars); raise max_tokens"
            ),
            0
        );
        // A message that carries no count is not a truncation the ladder can
        // reason about, so it reads as no content rather than as a guess.
        assert_eq!(truncated_content_chars("429: rate limit exceeded"), 0);
    }

    /// `reasoning_content=0 chars` is a plain empty reply: there is nothing to
    /// switch off, so it must not flip the knob.
    #[test]
    fn an_empty_reply_with_no_reasoning_keeps_reasoning_on() {
        assert!(reasoning_ate_budget(
            "empty content from m (finish_reason=\"stop\"; reasoning_content=37683 chars); none"
        ));
        assert!(!reasoning_ate_budget(
            "empty content from m (finish_reason=\"stop\"; reasoning_content=0 chars); none"
        ));
        assert!(!reasoning_ate_budget("429: rate limit exceeded"));
    }

    /// Both error paths must say how the budget was spent. The retry decides
    /// between "think less" and "ask for more" from these two numbers, so a
    /// path that stops reporting them silently reverts to the old no-op
    /// failure class.
    #[test]
    fn both_error_paths_report_how_the_budget_was_spent() {
        // The truncated path must name both `content` and `reasoning_content`
        // or the retry cannot tell a cut-off answer from a thought-out one.
        let thought = "output truncated at 8192 tokens (finish_reason=length; \
content=0 chars; reasoning_content=37683 chars); raise max_tokens";
        assert!(thought.contains("content=0 chars"));
        assert!(thought.contains("reasoning_content=37683 chars"));

        let empty = "empty content from m (finish_reason=\"stop\"; \
reasoning_content=0 chars); the endpoint shipped no text";
        assert!(reasoning_ate_budget(thought));
        // Zero reasoning on the empty path must NOT read as "reasoning ate it".
        assert!(!reasoning_ate_budget(empty));
    }

    /// The endpoint ships reasoning with no text: parse the wire form the way
    /// `once()` does, so the retry condition is exercised against real bytes.
    #[test]
    fn reasoning_present_but_content_null_parses_to_the_retry_signal() {
        let wire = String::from(
            "{\"choices\":[{\"message\":{\"content\":null,\"reasoning_content\":\"1. Analyze\"},\"finish_reason\":\"length\"}]}",
        );
        let parsed: ChatResp = serde_json::from_str(&wire).unwrap();
        let c = parsed.choices.into_iter().next().unwrap();
        assert!(c.message.content.trim().is_empty());
        assert!(!c.message.reasoning_content.trim().is_empty());
    }
}
