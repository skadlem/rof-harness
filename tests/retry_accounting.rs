use rof::llm::{LlmClient, LlmReq, OpenRouterClient};
use std::io::{Read, Write};
use std::net::TcpListener;

fn request() -> LlmReq {
    LlmReq {
        system: "test".into(),
        prompt: "test".into(),
        max_tokens: 4096,
        reasoning_off: false,
        reasoning_low: false,
        thinking_off: false,
        roomier: false,
        shrunk: false,
    }
}

// The real HTTP client and retry loop run against complete provider-shaped
// responses; the server substitutes only the remote inference endpoint.
fn server(responses: Vec<serde_json::Value>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let thread = std::thread::spawn(move || {
        for body in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 4096];
            loop {
                let n = socket.read(&mut buf).unwrap();
                assert!(n > 0, "request ended before its body");
                bytes.extend_from_slice(&buf[..n]);
                if let Some(pos) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..pos]);
                    let len: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= pos + 4 + len {
                        break;
                    }
                }
            }
            let body = body.to_string();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        }
    });
    (base, thread)
}

#[tokio::test]
async fn chat_retry_includes_usage_from_truncated_attempt() {
    let (base, server) = server(vec![
        serde_json::json!({"choices":[{"message":{"content":null,"reasoning_content":"thinking"},"finish_reason":"length"}],"usage":{"prompt_tokens":100,"completion_tokens":20,"prompt_cache_hit_tokens":40,"cost":0.01}}),
        serde_json::json!({"choices":[{"message":{"content":"answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":200,"completion_tokens":30,"prompt_tokens_details":{"cached_tokens":80},"cost":0.02}}),
    ]);
    let response = OpenRouterClient::from_parts(base, "test".into())
        .complete("test", request())
        .await
        .unwrap();
    server.join().unwrap();
    assert_eq!(response.text, "answer");
    assert_eq!(response.input_tokens, 300, "every billed attempt counts");
    assert_eq!(response.output_tokens, 50);
    assert_eq!(response.cached_input_tokens, 120);
    assert!((response.cost_usd.unwrap() - 0.03).abs() < 1e-9);
    assert_eq!(response.attempts, 2);
}

#[tokio::test]
async fn responses_retry_includes_usage_from_incomplete_attempt() {
    let (base, server) = server(vec![
        serde_json::json!({"status":"incomplete","output":[],"usage":{"input_tokens":100,"output_tokens":20,"input_tokens_details":{"cached_tokens":40},"output_tokens_details":{"reasoning_tokens":20}}}),
        serde_json::json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"answer"}]}],"usage":{"input_tokens":200,"output_tokens":30,"input_tokens_details":{"cached_tokens":80},"output_tokens_details":{"reasoning_tokens":0}}}),
    ]);
    let response = OpenRouterClient::from_parts(base, "test".into())
        .complete("muse-test", request())
        .await
        .unwrap();
    server.join().unwrap();
    assert_eq!(response.text, "answer");
    assert_eq!(
        response.input_tokens, 300,
        "incomplete responses are billed too"
    );
    assert_eq!(response.output_tokens, 50);
    assert_eq!(response.cached_input_tokens, 120);
    assert_eq!(response.attempts, 2);
}

#[tokio::test]
async fn exhausted_completion_retains_usage_for_trace_and_budget() {
    let (base, server) = server(vec![serde_json::json!({
        "choices":[{"message":{"content":null,"reasoning_content":"thinking"},"finish_reason":"length"}],
        "usage":{"prompt_tokens":100,"completion_tokens":20,"prompt_cache_hit_tokens":40,"cost":0.01}
    })]);
    let client = OpenRouterClient::from_parts(base, "test".into()).with_profile(
        rof::llm::profile::EndpointProfile {
            ladder: vec![],
            ..Default::default()
        },
    );
    let error = client.complete("test", request()).await.unwrap_err();
    server.join().unwrap();
    assert!(error.to_string().contains("finish_reason=length"));
    let usage = error.usage();
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 20);
    assert_eq!(usage.cached_input_tokens, 40);
    assert_eq!(usage.cost_usd, Some(0.01));
    assert_eq!(usage.attempts, 1);
    let trace = rof::obs::TraceSink::new();
    error.record_usage(&trace, "implementer", "test");
    assert_eq!(
        trace.total_tokens(),
        120,
        "failed completions still spend the task budget"
    );
    assert_eq!(trace.len(), 1, "one aggregate usage event per logical call");
}

struct FallbackClient {
    fallback_fails: bool,
}
#[async_trait::async_trait]
impl LlmClient for FallbackClient {
    async fn complete(
        &self,
        model: &str,
        _: LlmReq,
    ) -> Result<rof::llm::LlmResp, rof::llm::LlmError> {
        let usage = rof::llm::LlmUsage {
            input_tokens: if model == "primary" { 100 } else { 200 },
            output_tokens: 20,
            cached_input_tokens: 10,
            cost_usd: Some(0.01),
            attempts: 1,
            latency_ms: 1,
        };
        if model == "primary" || self.fallback_fails {
            return Err(rof::llm::LlmError::Transport("failed".into()).with_usage(usage));
        }
        Ok(rof::llm::LlmResp {
            text: "answer".into(),
            input_tokens: 200,
            output_tokens: 20,
            cached_input_tokens: 10,
            cost_usd: Some(0.01),
            attempts: 1,
            latency_ms: 1,
        })
    }
}

#[tokio::test]
async fn fallback_success_keeps_primary_spend() {
    let service = rof::llm::ExecutorService::new(
        std::sync::Arc::new(FallbackClient {
            fallback_fails: false,
        }),
        "primary".into(),
        Some("fallback".into()),
    );
    let response = service.complete(request()).await.unwrap();
    assert_eq!(response.text, "answer");
    assert_eq!(response.input_tokens, 300);
    assert_eq!(response.output_tokens, 40);
    assert_eq!(response.cached_input_tokens, 20);
    assert_eq!(response.cost_usd, Some(0.02));
    assert_eq!(response.attempts, 2);
}

#[tokio::test]
async fn fallback_failure_keeps_both_models_spend() {
    let service = rof::llm::ExecutorService::new(
        std::sync::Arc::new(FallbackClient {
            fallback_fails: true,
        }),
        "primary".into(),
        Some("fallback".into()),
    );
    let error = service.complete(request()).await.unwrap_err();
    assert_eq!(error.usage().input_tokens, 300);
    assert_eq!(error.usage().output_tokens, 40);
    assert_eq!(error.usage().cost_usd, Some(0.02));
    assert_eq!(error.usage().attempts, 2);
}

#[tokio::test]
async fn failed_summary_retains_spend_without_claiming_delivered_summary() {
    let context = rof::llm::ContextService::new(
        std::sync::Arc::new(FallbackClient {
            fallback_fails: true,
        }),
        "primary".into(),
    );
    let mut policy = rof::context::ContextPolicy::from(&rof::config::TokenBudgets {
        long_term: 2000,
        mid_term: 1000,
        short_term: 6000,
    });
    policy.mid.summarize_at = 0.5;
    let builder = rof::context::ContextBuilder::with_policy(policy);
    let state = rof::context::CtxState {
        long_term: "conventions".into(),
        mid_term: "R".repeat(6000),
        short_term: "round".into(),
    };
    let (view, reports) = builder.plan_summarized(&state, &context).await;
    let mid = &reports[rof::context::LayerKind::Mid.index()];
    assert!(
        !mid.summarized,
        "failed compression falls back to the raw layer"
    );
    assert!(
        mid.summarize.call,
        "failed compression must still be accounted"
    );
    assert_eq!(mid.summarize.input_tokens, 100);
    assert_eq!(mid.summarize.output_tokens, 20);
    assert!(view.prompt.contains("RRRR"));
}
