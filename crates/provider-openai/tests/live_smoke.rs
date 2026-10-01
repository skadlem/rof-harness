//! Live smoke for the OpenAI-compatible adapter. Never runs in CI:
//! every live test is `#[ignore]`d and skips (never fails) unless
//! `LIVE_SMOKE=1` AND `OPENAI_API_KEY` is set.
//! Run: `LIVE_SMOKE=1 ROF_TEST_BASE_URL=<base> cargo test -p provider-openai
//! --test live_smoke -- --ignored --nocapture`. Key names only below, values
//! are never printed.
use provider_core::{LlmClient, ProviderMessage, Request, StopReason, Thinking};
use provider_openai::{EndpointProfile, OpenAiCompat};

struct LiveConfig {
    endpoint: String,
    key_env: &'static str,
    model: String,
}

const SETUP_MSG: &str = "LIVE_SMOKE=1 with OPENAI_API_KEY but no endpoint: set ROF_TEST_BASE_URL (preferred) or OPENAI_BASE_URL to your OpenAI-compatible base URL; no default is assumed";

// Pure picker so the gate is unit-testable without touching env.
fn pick(
    live: &str,
    openai: &str,
    base1: &str,
    base2: &str,
) -> Result<Option<&'static str>, &'static str> {
    if live != "1" {
        return Ok(None);
    }
    let key = if !openai.trim().is_empty() {
        "OPENAI_API_KEY"
    } else {
        return Ok(None);
    };
    if !base1.trim().is_empty() || !base2.trim().is_empty() {
        Ok(Some(key))
    } else {
        Err(SETUP_MSG)
    }
}

fn live_config() -> Result<Option<LiveConfig>, String> {
    let live = std::env::var("LIVE_SMOKE").unwrap_or_default();
    let openai = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    let base1 = std::env::var("ROF_TEST_BASE_URL").unwrap_or_default();
    let base2 = std::env::var("OPENAI_BASE_URL").unwrap_or_default();
    match pick(&live, &openai, &base1, &base2) {
        Ok(None) => Ok(None),
        Err(msg) => Err(msg.to_string()),
        Ok(Some(key_env)) => {
            let endpoint = if !base1.trim().is_empty() {
                base1
            } else {
                base2
            };
            let model =
                std::env::var("ROF_TEST_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
            Ok(Some(LiveConfig {
                endpoint,
                key_env,
                model,
            }))
        }
    }
}

fn tiny_req(prompt: &str, max_tokens: usize) -> Request {
    Request {
        messages: vec![ProviderMessage {
            role: "user".into(),
            content: prompt.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }],
        tools: vec![],
        max_tokens,
        thinking: Thinking::Auto,
        extras: serde_json::Value::Null,
    }
}

#[ignore]
#[tokio::test]
async fn live_basic_complete() {
    let Some(cfg) = live_config().expect(SETUP_MSG) else {
        return;
    };
    let c = OpenAiCompat::new(&cfg.endpoint, EndpointProfile::default()).with_key_env(cfg.key_env);
    let r = c
        .complete(&cfg.model, &tiny_req("reply with exactly: ok", 32))
        .await
        .expect("live basic complete failed");
    assert_eq!(r.stop, StopReason::Stop);
    assert!(r.attempts >= 1);
    assert!(r.usage.output > 0, "usage.output must be > 0");
}

#[ignore]
#[tokio::test]
async fn live_truncation_probe() {
    let Some(cfg) = live_config().expect(SETUP_MSG) else {
        return;
    };
    let c = OpenAiCompat::new(&cfg.endpoint, EndpointProfile::default()).with_key_env(cfg.key_env);
    // Long-output prompt with max_tokens=8: spend stays tiny via the cap while
    // truncation (finish_reason=length) is near-certain.
    let req = tiny_req("write a long story (at least 500 words) about the sea", 8);
    match c.complete(&cfg.model, &req).await {
        Ok(r) => assert_eq!(r.stop, StopReason::MaxTokens),
        Err(e) => {
            let s = format!("{e:?}");
            assert!(!s.trim().is_empty(), "truncation error must never be empty");
            assert!(
                s.contains("length") || s.contains("truncat") || s.contains("max_tokens"),
                "truncation error should name the cause, got: {s}"
            );
        }
    }
}

#[test]
fn gate_picker_skips_fails_loudly() {
    assert!(pick("0", "k", "http://x", "").unwrap().is_none());
    assert!(pick("", "", "http://x", "").unwrap().is_none());
    assert!(pick("1", "", "http://x", "").unwrap().is_none());
    assert_eq!(
        pick("1", "k", "http://x", "").unwrap(),
        Some("OPENAI_API_KEY")
    );
    assert_eq!(
        pick("1", "k", "", "http://y").unwrap(),
        Some("OPENAI_API_KEY")
    );
    assert!(pick("1", "k", "", "").is_err());
}
