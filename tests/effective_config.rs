use rof::config::AppConfig;
use rof::eval::runner::RunLabel;
use rof::eval::suite::EvalSuite;
use serde_json::json;

#[test]
fn effective_generation_changes_run_label_and_round_trips() {
    let baseline: AppConfig = serde_json::from_value(json!({})).unwrap();
    let configured: AppConfig = serde_json::from_value(json!({
        "generation": {
            "thinking":"off", "implementer_max_tokens":4096,
            "reviewer_max_tokens":2048,"reask_budget":6000,
            "compact":true,"reasoning_effort":"low"
        }
    }))
    .unwrap();
    let suite = EvalSuite {
        name: "config-provenance".into(),
        tasks: vec![],
    };
    let dir = std::path::Path::new(".");
    assert_ne!(
        RunLabel::build(&baseline, &suite, dir).config_hash,
        RunLabel::build(&configured, &suite, dir).config_hash
    );
    let value: serde_json::Value = serde_json::from_str(&configured.to_json()).unwrap();
    assert_eq!(value["generation"]["thinking"], "off");
    assert_eq!(value["generation"]["reasoning_effort"], "low");
    let roundtrip: AppConfig = serde_json::from_str(&configured.to_json()).unwrap();
    assert_eq!(roundtrip.to_json(), configured.to_json());
}

#[test]
fn execution_deadlines_have_bounded_defaults_and_round_trip() {
    let cfg = AppConfig::default();
    let value: serde_json::Value = serde_json::from_str(&cfg.to_json()).unwrap();
    assert_eq!(value["limits"]["connect_timeout_ms"], 10000);
    assert_eq!(value["limits"]["request_timeout_ms"], 120000);
    assert_eq!(value["limits"]["task_timeout_ms"], 900000);
    assert_eq!(value["permissions"]["command_timeout_ms"], 300000);
    let configured: AppConfig = serde_json::from_value(
        json!({"limits":{"task_timeout_ms":500},"permissions":{"command_timeout_ms":250}}),
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&configured.to_json()).unwrap();
    assert_eq!(value["limits"]["task_timeout_ms"], 500);
    assert_eq!(value["permissions"]["command_timeout_ms"], 250);
}

#[test]
fn cli_resolves_environment_into_serialized_effective_config() {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_rof"));
    command.arg("config");
    for key in [
        "ROF_CONFIG",
        "OR_TOKEN",
        "ROF_TOKEN",
        "ROF_CHAT_BASE",
        "ROF_VERIFY_TOKEN",
        "ROF_VERIFY_BASE",
    ] {
        command.env_remove(key);
    }
    for (key, value) in [
        ("ROF_THINKING", "low"),
        ("ROF_IMPLEMENTER_MAX_TOKENS", "4096"),
        ("ROF_REVIEWER_MAX_TOKENS", "2048"),
        ("ROF_REASK_BUDGET", "6000"),
        ("ROF_COMPACT", "yes"),
        ("ROF_REASONING_EFFORT", "medium"),
        ("ROF_CONNECT_TIMEOUT_MS", "50"),
        ("ROF_REQUEST_TIMEOUT_MS", "100"),
        ("ROF_TASK_TIMEOUT_MS", "500"),
        ("ROF_COMMAND_TIMEOUT_MS", "250"),
    ] {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["generation"],
        json!({"thinking":"low","implementer_max_tokens":4096,"reviewer_max_tokens":2048,"reask_budget":6000,"compact":true,"reasoning_effort":"medium"})
    );
    assert_eq!(
        value["limits"],
        json!({"connect_timeout_ms":50,"request_timeout_ms":100,"task_timeout_ms":500})
    );
    assert_eq!(value["permissions"]["command_timeout_ms"], 250);
}

#[derive(Default)]
struct Capture(std::sync::Mutex<Vec<rof::llm::LlmReq>>);
#[async_trait::async_trait]
impl rof::llm::LlmClient for Capture {
    async fn complete(
        &self,
        _model: &str,
        req: rof::llm::LlmReq,
    ) -> Result<rof::llm::LlmResp, rof::llm::LlmError> {
        self.0.lock().unwrap().push(req);
        Ok(rof::llm::LlmResp {
            text: r#"{"pass":true,"feedback":"ok"}"#.into(),
            input_tokens: 1,
            output_tokens: 1,
            latency_ms: 1,
            cost_usd: None,
            cached_input_tokens: 0,
            attempts: 1,
        })
    }
}

#[tokio::test]
async fn reviewer_uses_generation_snapshot_after_environment_changes() {
    use rof::agents::{Agent, AgentCtx, ReviewerAgent};
    let capture = std::sync::Arc::new(Capture::default());
    let generation = rof::config::GenerationConfig {
        thinking: "off".into(),
        reviewer_max_tokens: 2048,
        ..Default::default()
    };
    let service = rof::llm::ExecutorService::new(capture.clone(), "snapshot".into(), None)
        .with_generation(generation);
    let old = std::env::var_os("ROF_THINKING");
    std::env::set_var("ROF_THINKING", "on");
    let old_cap = std::env::var_os("ROF_REVIEWER_MAX_TOKENS");
    std::env::set_var("ROF_REVIEWER_MAX_TOKENS", "8192");
    let view = rof::context::CtxView {
        prompt: "review artifact".into(),
        used_tokens: 3,
        truncated: false,
    };
    let trace = rof::obs::TraceSink::new();
    let result = ReviewerAgent::new(&service)
        .run(AgentCtx {
            view: &view,
            context: None,
            executor: Some(&service),
            tools: None,
            workdir: None,
            trace: &trace,
            volatile_budget: 12000,
        })
        .await;
    for (key, value) in [("ROF_THINKING", old), ("ROF_REVIEWER_MAX_TOKENS", old_cap)] {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
    result.unwrap();
    let requests = capture.0.lock().unwrap();
    assert_eq!(requests[0].max_tokens, 2048);
    assert!(requests[0].thinking_off && requests[0].reasoning_off);
}

#[tokio::test]
async fn runtime_client_applies_effort_and_request_deadline() {
    use rof::llm::LlmClient;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (_connection, _) = listener.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
    });
    let generation = rof::config::GenerationConfig {
        reasoning_effort: Some("low".into()),
        ..Default::default()
    };
    let limits = rof::config::ExecutionLimits {
        connect_timeout_ms: 50,
        request_timeout_ms: 20,
        ..Default::default()
    };
    let client = rof::llm::OpenRouterClient::from_parts(base, "test".into())
        .with_runtime(&generation, &limits);
    let payload: serde_json::Value =
        serde_json::from_str(&rof::llm::wire_body_for_test(&client, "model")).unwrap();
    assert_eq!(payload["reasoning_effort"], "low");
    let request = rof::llm::base_req_for_test(1024);
    let completed = tokio::time::timeout(
        std::time::Duration::from_millis(150),
        client.complete("model", request),
    )
    .await;
    assert!(
        matches!(completed, Ok(Err(_))),
        "request deadline must terminate the stalled provider call"
    );
    server.join().unwrap();
}
