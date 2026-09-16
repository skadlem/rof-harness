use rof::config::{AppConfig, PermissionPolicy};
use rof::eval::EvalReport;
use rof::obs::TraceEvent;
use rof::tools::{HttpGetTool, ToolRegistry};
use std::path::PathBuf;

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rof-v1-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ---------------------------------------------------------------- config file

#[test]
fn partial_config_merges_over_defaults() {
    let d = scratch("cfg");
    let p = d.join("rof.json");
    std::fs::write(
        &p,
        r#"{"budgets":{"long_term":1234},"planner":"skip","max_review_rounds":5}"#,
    )
    .unwrap();
    let cfg = AppConfig::load(&p).unwrap();
    assert_eq!(cfg.budgets.long_term, 1234, "stated field is taken");
    assert_eq!(
        cfg.budgets.mid_term,
        AppConfig::default().budgets.mid_term,
        "unstated nested field falls back to default"
    );
    assert_eq!(cfg.planner, "skip");
    assert_eq!(cfg.max_review_rounds, 5);
    // untouched top-level section keeps its default
    assert_eq!(
        cfg.retrieval.max_snippets,
        AppConfig::default().retrieval.max_snippets
    );
}

#[test]
fn config_round_trips_through_its_own_dump() {
    let d = scratch("roundtrip");
    let src = d.join("a.json");
    std::fs::write(&src, r#"{"cost_lambda":0.5,"budgets":{"short_term":900}}"#).unwrap();
    let a = AppConfig::load(&src).unwrap();
    // `rof config` output must reload to the same effective config, or
    // versioned configs cannot be compared.
    let dump = d.join("b.json");
    std::fs::write(&dump, a.to_json()).unwrap();
    let b = AppConfig::load(&dump).unwrap();
    assert!((b.cost_lambda - 0.5).abs() < 1e-9);
    assert_eq!(b.budgets.short_term, 900);
    assert_eq!(b.to_json(), a.to_json());
}

#[test]
fn unknown_keys_are_ignored_and_bad_json_is_an_error() {
    let d = scratch("cfg-bad");
    let ok = d.join("ok.json");
    std::fs::write(&ok, r#"{"future_key":1}"#).unwrap();
    assert!(AppConfig::load(&ok).is_ok(), "forward-compatible configs");

    let bad = d.join("bad.json");
    std::fs::write(&bad, "{not json").unwrap();
    let err = AppConfig::load(&bad).unwrap_err().to_string();
    assert!(err.contains("bad.json"), "error names the file: {err}");
}

// ------------------------------------------------------------------- http.get

fn http_registry(hosts: &[&str]) -> ToolRegistry {
    let policy = PermissionPolicy {
        allowed_hosts: hosts.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    let mut r = ToolRegistry::new(policy);
    r.register(HttpGetTool::new(
        hosts.iter().map(|s| s.to_string()).collect(),
    ));
    r
}

async fn call(r: &ToolRegistry, input: serde_json::Value) -> Result<String, String> {
    let (res, _) = r.call("implementer", "http.get", None, input).await;
    match res {
        Ok(o) => Ok(o.output),
        Err(e) => Err(e.to_string()),
    }
}

#[tokio::test]
async fn http_denies_unlisted_host_and_bad_scheme() {
    let r = http_registry(&["example.com"]);
    let e = call(&r, serde_json::json!({"url": "https://evil.com/x"}))
        .await
        .unwrap_err();
    assert!(e.contains("not allowlisted"), "{e}");

    let e = call(&r, serde_json::json!({"url": "file:///etc/passwd"}))
        .await
        .unwrap_err();
    assert!(e.contains("scheme"), "{e}");

    // suffix trick must not pass an exact-match allowlist
    let e = call(
        &r,
        serde_json::json!({"url": "https://example.com.evil.com/x"}),
    )
    .await
    .unwrap_err();
    assert!(e.contains("not allowlisted"), "{e}");

    let e = call(&r, serde_json::json!({})).await.unwrap_err();
    assert!(e.contains("missing url"), "{e}");
}

#[tokio::test]
async fn http_denied_for_agents_without_the_grant() {
    let r = http_registry(&["example.com"]);
    let (res, _) = r
        .call(
            "reviewer",
            "http.get",
            None,
            serde_json::json!({"url": "https://example.com"}),
        )
        .await;
    assert!(res.unwrap_err().to_string().contains("may not use"));
}

// ------------------------------------------------------- reliability metrics

#[test]
fn retries_and_model_errors_are_counted() {
    let mut r = EvalReport::default();
    for attempts in [1u64, 3, 1, 2] {
        r.fold(&TraceEvent::ModelCall {
            agent: "implementer".into(),
            model: "deepseek-chat".into(),
            input_tokens: 100,
            output_tokens: 10,
            latency_ms: 5,
            cost_usd: None,
            cached_input_tokens: 0,
            attempts,
        });
    }
    assert_eq!(r.retried_calls, 2, "calls that needed >1 attempt");
    assert_eq!(r.max_attempts, 3);
    r.fold(&TraceEvent::ModelError {
        agent: "planner".into(),
        error: "503".into(),
    });
    assert_eq!(r.model_errors, 1);
    // reliability must not distort the success/utility signal
    assert_eq!(r.success_rate(), 0.0);
}

#[test]
fn legacy_trace_lines_without_attempts_still_fold() {
    // Older JSONL has no `attempts` key; serde default must keep it parseable.
    let line = r#"{"ModelCall":{"agent":"planner","model":"m","input_tokens":10,"output_tokens":2,"latency_ms":1}}"#;
    let ev: TraceEvent = serde_json::from_str(line).expect("legacy line parses");
    let mut r = EvalReport::default();
    r.fold(&ev);
    assert_eq!(r.retried_calls, 0);
    assert_eq!(r.max_attempts, 1, "0 attempts is read as one try");
    assert_eq!(r.est_input_tokens, 10);
}
