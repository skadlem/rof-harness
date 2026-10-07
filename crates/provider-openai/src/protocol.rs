use provider_core::{
    infer_stop, parse_streaming_json, Request, Response, StopReason, ToolCallRef, Usage,
};
use serde::Deserialize;

use crate::pricing::price_usd;
use crate::retry::WireKnobs;

pub(crate) fn wire_body(model: &str, req: &Request, k: &WireKnobs) -> serde_json::Value {
    // Thinking-mode conversation: any assistant message carried real
    // reasoning. The passback requirement ("reasoning_content ... must be
    // passed back"; 400 otherwise) is OFFICIALLY documented on DeepSeek's
    // Thinking Mode page. Two behaviors below are empirical workarounds:
    // empty string is accepted, and a request ending on tool messages
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

#[derive(Debug, Deserialize, Default)]
pub(crate) struct ChatResp {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    pub(crate) usage: Option<WireUsage>,
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
pub(crate) struct WireUsage {
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
pub(crate) struct BodyFail {
    pub(crate) msg: String,
    pub(crate) usage: Option<Usage>,
}

/// Wire usage -> `Usage`; None when the body shipped no usage object at all.
/// Never zeros: the budget must not read "unreported" as "free".
pub(crate) fn wire_usage(u: Option<WireUsage>, model: &str) -> Option<Usage> {
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

/// Parse one chat/completions body. Truncation and empty answers are errors
/// (never empty answers): the retry ladder needs the counts to decide.
pub(crate) fn parse_body(
    v: &serde_json::Value,
    model: &str,
    max_tokens: usize,
) -> Result<Response, BodyFail> {
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
    use crate::client::EndpointProfile;
    use crate::retry::chars_after;
    use crate::test_support::req_with_tool;
    use provider_core::{ProviderMessage, Thinking};
    use std::collections::HashMap;
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
