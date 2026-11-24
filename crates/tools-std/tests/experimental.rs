use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tool_core::{CallStatus, Tool, ToolCall};
use tools_std::{default_denied_globs, experimental_tools, ExperimentalToolsConfig, Policy};

static N: AtomicUsize = AtomicUsize::new(0);

fn temp_root() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "tools-std-exp-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn policy(root: PathBuf, allow_any_cmd: bool) -> Arc<Policy> {
    Arc::new(Policy {
        root,
        allowed_commands: vec![],
        allowed_prefixes: vec![],
        syntax_cmd: None,
        denied_globs: default_denied_globs(),
        pass_env: vec![],
        allow_any_cmd,
        exec_wrap: None,
        head_tail_output: false,
    })
}

fn write_history(path: &std::path::Path, content: &str, is_error: bool) {
    let header = serde_json::json!({
        "seq": 1, "id": "item-1", "parent_id": null,
        "recorded_at": {"secs_since_epoch": 1, "nanos_since_epoch": 0},
        "kind": "Header", "version": 1, "session_id": "current",
        "cwd": "/repo", "model": "test"
    });
    let result = serde_json::json!({
        "seq": 2, "id": "item-2", "parent_id": "item-1",
        "recorded_at": {"secs_since_epoch": 2, "nanos_since_epoch": 0},
        "kind": "ToolResult", "call_id": "call-2", "content": content,
        "is_error": is_error, "recovery": null
    });
    std::fs::write(path, format!("{}\n{}\n", header, result)).unwrap();
}

fn history_tool(tools: &[Arc<dyn Tool>]) -> &Arc<dyn Tool> {
    tools
        .iter()
        .find(|t| t.definition().name == "history_read")
        .expect("history_read registered")
}

async fn invoke(
    tool: &dyn Tool,
    args: serde_json::Value,
) -> Result<tool_core::ToolOutcome, tool_core::ToolError> {
    let call = ToolCall {
        call_id: "test".into(),
        name: tool.definition().name,
        args,
    };
    let CallStatus::Dispatch(inv) = tool.prepare(&call) else {
        panic!("dispatch")
    };
    tool.execute(inv, CancellationToken::new()).await
}

#[test]
fn history_read_is_absent_by_default_and_does_not_change_default_tool_count() {
    let tools = experimental_tools(
        policy(temp_root(), false),
        ExperimentalToolsConfig::default(),
    )
    .unwrap();
    assert_eq!(tools.len(), 6);
    assert!(!tools.iter().any(|t| t.definition().name == "history_read"));
}

#[tokio::test]
async fn history_registry_binds_missing_current_wal_before_writer_creates_it() {
    let temp = temp_root();
    let wal = temp.join("created-after-registry.jsonl");
    let tools = experimental_tools(
        policy(temp, false),
        ExperimentalToolsConfig {
            history_log: Some(wal.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    write_history(&wal, "created by current writer", false);
    let result = invoke(
        history_tool(&tools).as_ref(),
        serde_json::json!({"item_id":"item-2"}),
    )
    .await
    .unwrap();
    assert!(result.content.contains("created by current writer"));
}

#[tokio::test]
async fn history_read_returns_fixed_wal_observation_after_current_file_changes() {
    let temp = temp_root();
    let root = temp.join("repo");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("state.txt"), "current observation").unwrap();
    let wal = temp.join("current.jsonl");
    write_history(&wal, "old observation: λambda", true);
    let tools = experimental_tools(
        policy(root.clone(), false),
        ExperimentalToolsConfig {
            history_log: Some(wal.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    std::fs::write(root.join("state.txt"), "new observation").unwrap();

    let historical = invoke(
        history_tool(&tools).as_ref(),
        serde_json::json!({"item_id":"item-2"}),
    )
    .await
    .unwrap();
    assert!(historical.success);
    assert!(historical.content.contains("historical"));
    assert!(historical.content.contains("item_id=item-2"));
    assert!(historical.content.contains("call_id=call-2"));
    assert!(historical.content.contains("seq=2"));
    assert!(historical.content.contains("is_error=true"));
    assert!(historical.content.contains("old observation: λambda"));

    let view = tools
        .iter()
        .find(|t| t.definition().name == "view")
        .unwrap();
    let current = invoke(view.as_ref(), serde_json::json!({"path":"state.txt"}))
        .await
        .unwrap();
    assert!(current.content.contains("new observation"));
    assert!(!current.content.contains("old observation"));
}

#[tokio::test]
async fn history_read_pages_by_unicode_characters_and_rejects_oversize_page() {
    let temp = temp_root();
    let wal = temp.join("current.jsonl");
    write_history(&wal, &format!("{}END", "🧪".repeat(40_000)), false);
    let tools = experimental_tools(
        policy(temp.clone(), false),
        ExperimentalToolsConfig {
            history_log: Some(wal),
            ..Default::default()
        },
    )
    .unwrap();
    let default_page = invoke(
        history_tool(&tools).as_ref(),
        serde_json::json!({"item_id":"item-2"}),
    )
    .await
    .unwrap();
    assert_eq!(default_page.content.matches("🧪").count(), 8_000);
    assert!(default_page.truncated);
    assert!(invoke(
        history_tool(&tools).as_ref(),
        serde_json::json!({"item_id":"item-2","limit":32769}),
    )
    .await
    .is_err());
    let first = invoke(
        history_tool(&tools).as_ref(),
        serde_json::json!({"item_id":"item-2","limit":32768}),
    )
    .await
    .unwrap();
    assert_eq!(first.content.matches("🧪").count(), 32768);
    let next = invoke(
        history_tool(&tools).as_ref(),
        serde_json::json!({"item_id":"item-2","offset":32768,"limit":8000}),
    )
    .await
    .unwrap();
    assert_eq!(next.content.matches("🧪").count(), 7232);
    assert!(next.content.contains("END"));
}

#[tokio::test]
async fn history_read_rejects_unknown_and_non_tool_item_ids() {
    let temp = temp_root();
    let wal = temp.join("current.jsonl");
    write_history(&wal, "captured", false);
    let tools = experimental_tools(
        policy(temp, false),
        ExperimentalToolsConfig {
            history_log: Some(wal),
            ..Default::default()
        },
    )
    .unwrap();
    for id in ["missing", "item-1"] {
        let result = invoke(
            history_tool(&tools).as_ref(),
            serde_json::json!({"item_id":id}),
        )
        .await;
        assert!(result.is_err(), "{id} must not resolve to a tool result");
    }
}

#[tokio::test]
async fn history_read_uses_only_configured_wal_when_ids_collide() {
    let temp = temp_root();
    let current = temp.join("current.jsonl");
    let previous = temp.join("previous.jsonl");
    write_history(&current, "current WAL", false);
    write_history(&previous, "previous WAL", false);
    let tools = experimental_tools(
        policy(temp.clone(), false),
        ExperimentalToolsConfig {
            history_log: Some(current),
            ..Default::default()
        },
    )
    .unwrap();
    let result = invoke(
        history_tool(&tools).as_ref(),
        serde_json::json!({"item_id":"item-2"}),
    )
    .await
    .unwrap();
    assert!(result.content.contains("current WAL"));
    assert!(!result.content.contains("previous WAL"));
}

#[tokio::test]
async fn history_read_accepts_torn_final_row_but_rejects_middle_corruption() {
    let temp = temp_root();
    let wal = temp.join("current.jsonl");
    write_history(&wal, "captured", false);
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&wal)
        .unwrap()
        .write_all(b"{torn")
        .unwrap();
    let tools = experimental_tools(
        policy(temp.clone(), false),
        ExperimentalToolsConfig {
            history_log: Some(wal.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let result = invoke(
        history_tool(&tools).as_ref(),
        serde_json::json!({"item_id":"item-2"}),
    )
    .await
    .unwrap();
    assert!(result.content.contains("captured"));

    let valid = std::fs::read_to_string(&wal).unwrap();
    let lines: Vec<_> = valid.lines().collect();
    std::fs::write(&wal, format!("{}\n{{bad}}\n{}\n", lines[0], lines[1])).unwrap();
    assert!(invoke(
        history_tool(&tools).as_ref(),
        serde_json::json!({"item_id":"item-2"}),
    )
    .await
    .is_err());
}

#[tokio::test]
async fn history_read_rejects_oversized_log_and_honors_cancellation() {
    let temp = temp_root();
    let wal = temp.join("large.jsonl");
    let file = std::fs::File::create(&wal).unwrap();
    file.set_len(64 * 1024 * 1024 + 1).unwrap();
    let tools = experimental_tools(
        policy(temp, false),
        ExperimentalToolsConfig {
            history_log: Some(wal),
            ..Default::default()
        },
    )
    .unwrap();
    let tool = history_tool(&tools);
    let call = ToolCall {
        call_id: "cancel-history".into(),
        name: "history_read".into(),
        args: serde_json::json!({"item_id":"item-2"}),
    };
    let CallStatus::Dispatch(inv) = tool.prepare(&call) else {
        panic!("dispatch")
    };
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(tool.execute(inv, cancel).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn history_read_refuses_a_symlink_current_wal() {
    use std::os::unix::fs::symlink;
    let temp = temp_root();
    let target = temp.join("target.jsonl");
    let link = temp.join("current.jsonl");
    write_history(&target, "must not follow", false);
    symlink(&target, &link).unwrap();
    let tools = experimental_tools(
        policy(temp, false),
        ExperimentalToolsConfig {
            history_log: Some(link),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(invoke(
        history_tool(&tools).as_ref(),
        serde_json::json!({"item_id":"item-2"}),
    )
    .await
    .is_err());
}

#[test]
fn shell_and_process_require_allow_any_command() {
    let root = std::env::temp_dir();
    for config in [
        ExperimentalToolsConfig {
            shell: true,
            ..Default::default()
        },
        ExperimentalToolsConfig {
            processes: true,
            ..Default::default()
        },
    ] {
        assert!(experimental_tools(policy(root.clone(), false), config).is_err());
    }
}

#[tokio::test]
async fn output_read_paginates_archived_full_output() {
    let temp = temp_root();
    let root = temp.join("repo");
    std::fs::create_dir(&root).unwrap();
    let out = temp.join("archive");
    let tools = experimental_tools(
        policy(root, true),
        ExperimentalToolsConfig {
            output_dir: Some(out),
            ..Default::default()
        },
    )
    .unwrap();
    let exec = tools
        .iter()
        .find(|t| t.definition().name == "exec")
        .unwrap();
    let result = invoke(
        exec.as_ref(),
        serde_json::json!({"cmd":"python3 -c \"print('A'*9000); print('target-marker'); print('target-marker'); print('target-marker')\""}),
    )
    .await
    .unwrap();
    let preview = &result.content[..result.content.len().min(120)];
    assert!(
        preview.contains("output handle="),
        "handle missing from preview: {preview:?}"
    );
    let handle = result
        .content
        .split("handle=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let read = tools
        .iter()
        .find(|t| t.definition().name == "output_read")
        .unwrap();
    let page = invoke(
        read.as_ref(),
        serde_json::json!({"handle":handle,"offset":9000,"limit":32}),
    )
    .await
    .unwrap();
    assert!(page.content.contains("target-marker"));
    let search = invoke(
        read.as_ref(),
        serde_json::json!({"handle":handle,"search":"target-marker","limit":1}),
    )
    .await
    .unwrap();
    assert!(search.content.contains("target-marker"));
    assert!(search.content.contains("matches=3"));
    assert!(search.truncated);
    let next = invoke(
        read.as_ref(),
        serde_json::json!({"handle":handle,"search":"target-marker","offset":1,"limit":1}),
    )
    .await
    .unwrap();
    assert!(next.content.contains("target-marker"));
    assert!(!result.content.contains("target-marker"));
}

#[tokio::test]
async fn failed_archived_test_keeps_verdict_and_handle_in_preview() {
    let temp = temp_root();
    let root = temp.join("repo");
    std::fs::create_dir(&root).unwrap();
    let tools = experimental_tools(
        policy(root, true),
        ExperimentalToolsConfig {
            output_dir: Some(temp.join("archive")),
            ..Default::default()
        },
    )
    .unwrap();
    let check = tools
        .iter()
        .find(|t| t.definition().name == "test")
        .unwrap();
    let result = invoke(
        check.as_ref(),
        serde_json::json!({"cmd":"python3 -c \"import sys; print('X'*9000); print('failed-marker'); sys.exit(1)\""}),
    )
    .await
    .unwrap();
    let preview = &result.content[..result.content.len().min(120)];
    assert!(!result.success);
    assert!(
        preview.contains("output handle="),
        "handle missing from preview: {preview:?}"
    );
    assert!(preview.contains("\nFAIL:"));
    let handle = preview
        .split("handle=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let read = tools
        .iter()
        .find(|t| t.definition().name == "output_read")
        .unwrap();
    let page = invoke(
        read.as_ref(),
        serde_json::json!({"handle":handle,"offset":9000,"limit":64}),
    )
    .await
    .unwrap();
    assert!(page.content.contains("failed-marker"));
}

#[tokio::test]
async fn output_read_pages_long_lines_without_loading_the_whole_artifact() {
    let temp = temp_root();
    let root = temp.join("repo");
    std::fs::create_dir(&root).unwrap();
    let out = temp.join("archive");
    let tools = experimental_tools(
        policy(root, true),
        ExperimentalToolsConfig {
            output_dir: Some(out),
            ..Default::default()
        },
    )
    .unwrap();
    let exec = tools
        .iter()
        .find(|t| t.definition().name == "exec")
        .unwrap();
    let result = invoke(
        exec.as_ref(),
        serde_json::json!({"cmd":"python3 -c \"print('x'*12000, end=''); print('MARKER')\""}),
    )
    .await
    .unwrap();
    let handle = result
        .content
        .split("handle=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let read = tools
        .iter()
        .find(|t| t.definition().name == "output_read")
        .unwrap();
    let page = invoke(
        read.as_ref(),
        serde_json::json!({"handle":handle,"offset":11998,"limit":32}),
    )
    .await
    .unwrap();
    assert!(page.content.contains("MARKER"));
    assert!(page.content.len() < 256);
}

#[tokio::test]
async fn shell_supports_pipes_and_redirection() {
    let temp = temp_root();
    let tools = experimental_tools(
        policy(temp.clone(), true),
        ExperimentalToolsConfig {
            shell: true,
            ..Default::default()
        },
    )
    .unwrap();
    let shell = tools
        .iter()
        .find(|t| t.definition().name == "shell")
        .unwrap();
    let result = invoke(
        shell.as_ref(),
        serde_json::json!({"cmd":"printf 'ok\\n' | tr o O > shell.out"}),
    )
    .await
    .unwrap();
    assert!(result.success);
    assert_eq!(
        std::fs::read_to_string(temp.join("shell.out")).unwrap(),
        "Ok\n"
    );
}

#[tokio::test]
async fn shell_uses_the_exec_wrapper() {
    let temp = temp_root();
    let mut p = (*policy(temp.clone(), true)).clone();
    p.exec_wrap = Some(vec!["env".into()]);
    let tools = experimental_tools(
        Arc::new(p),
        ExperimentalToolsConfig {
            shell: true,
            ..Default::default()
        },
    )
    .unwrap();
    let shell = tools
        .iter()
        .find(|t| t.definition().name == "shell")
        .unwrap();
    let result = invoke(
        shell.as_ref(),
        serde_json::json!({"cmd":"printf wrapped > wrapped.out"}),
    )
    .await
    .unwrap();
    assert!(result.success);
    assert_eq!(
        std::fs::read_to_string(temp.join("wrapped.out")).unwrap(),
        "wrapped"
    );
}

#[tokio::test]
async fn repo_lookup_reports_literal_occurrences_with_test_classification() {
    let temp = temp_root();
    std::fs::create_dir_all(temp.join("tests")).unwrap();
    std::fs::write(temp.join("src.rs"), "fn target() {}\n").unwrap();
    std::fs::write(temp.join("tests/check.rs"), "target();\n").unwrap();
    let tools = experimental_tools(
        policy(temp.clone(), false),
        ExperimentalToolsConfig {
            repo_lookup: true,
            ..Default::default()
        },
    )
    .unwrap();
    let lookup = tools
        .iter()
        .find(|t| t.definition().name == "repo_lookup")
        .unwrap();
    let result = invoke(lookup.as_ref(), serde_json::json!({"symbol":"target"}))
        .await
        .unwrap();
    assert!(result.content.contains("tests/check.rs"));
    assert!(result.content.contains("literal"));
}

#[tokio::test]
async fn repo_evidence_groups_definitions_tests_and_configuration_with_shared_limit() {
    let temp = temp_root();
    std::fs::create_dir_all(temp.join("tests")).unwrap();
    std::fs::create_dir_all(temp.join("config")).unwrap();
    std::fs::write(
        temp.join("src.rs"),
        "pub fn plantedCaller() { plantedCaller(); }\n",
    )
    .unwrap();
    std::fs::write(temp.join("src.py"), "def plantedCaller(): pass\n").unwrap();
    std::fs::write(temp.join("tests/check.py"), "def plantedCaller(): pass\n").unwrap();
    std::fs::write(
        temp.join("config/settings.toml"),
        "entry = 'plantedCaller'\n",
    )
    .unwrap();
    let tools = experimental_tools(
        policy(temp, false),
        ExperimentalToolsConfig {
            repo_lookup: true,
            repo_evidence: true,
            ..Default::default()
        },
    )
    .unwrap();
    let lookup = tools
        .iter()
        .find(|t| t.definition().name == "repo_lookup")
        .unwrap();
    let definition = lookup.definition();
    assert_eq!(definition.schema["required"], serde_json::json!(["symbol"]));
    assert!(definition.schema["properties"]["limit"].is_object());
    let result = invoke(
        lookup.as_ref(),
        serde_json::json!({"symbol":"plantedCaller","limit":4}),
    )
    .await
    .unwrap();
    let definitions = result
        .content
        .split("definition candidates:\n")
        .nth(1)
        .unwrap()
        .split("\ntest path candidates:")
        .next()
        .unwrap();
    assert!(definitions.contains("src.rs:1"));
    assert!(definitions.contains("src.py:1"));
    assert!(result.content.contains("test path candidates"));
    assert!(result.content.contains("configuration path candidates"));
    assert_eq!(result.content.matches("src.rs:1").count(), 1);
    assert!(!result.content.contains("literal occurrences"));
    assert!(result.content.contains("tests/check.py:1"));
    assert!(result.content.contains("config/settings.toml:1"));
    let limited = invoke(
        lookup.as_ref(),
        serde_json::json!({"symbol":"plantedCaller","limit":2}),
    )
    .await
    .unwrap();
    assert_eq!(
        limited
            .content
            .lines()
            .filter(|line| line.contains(":1:"))
            .count(),
        2
    );
    assert!(limited.truncated && limited.content.contains("shared hit limit"));
}

#[tokio::test]
async fn repo_evidence_order_is_independent_of_file_creation_order() {
    let roots = [temp_root(), temp_root()];
    for (root, names) in roots.iter().zip([["z.rs", "a.rs"], ["a.rs", "z.rs"]]) {
        for name in names {
            std::fs::write(root.join(name), "needle\n").unwrap();
        }
    }
    let mut results = Vec::new();
    for root in roots {
        let tools = experimental_tools(
            policy(root, false),
            ExperimentalToolsConfig {
                repo_lookup: true,
                repo_evidence: true,
                ..Default::default()
            },
        )
        .unwrap();
        let lookup = tools
            .iter()
            .find(|t| t.definition().name == "repo_lookup")
            .unwrap();
        results.push(
            invoke(lookup.as_ref(), serde_json::json!({"symbol":"needle"}))
                .await
                .unwrap()
                .content,
        );
    }
    assert_eq!(results[0], results[1]);
}

#[tokio::test]
async fn repo_evidence_output_is_bounded_and_reports_omissions() {
    let temp = temp_root();
    let lines = (0..300).map(|i| format!("needle_{i} {}\n", "x".repeat(600)));
    std::fs::write(temp.join("source.txt"), lines.collect::<String>()).unwrap();
    let tools = experimental_tools(
        policy(temp, false),
        ExperimentalToolsConfig {
            repo_lookup: true,
            repo_evidence: true,
            ..Default::default()
        },
    )
    .unwrap();
    let lookup = tools
        .iter()
        .find(|t| t.definition().name == "repo_lookup")
        .unwrap();
    let result = invoke(lookup.as_ref(), serde_json::json!({"symbol":"needle"}))
        .await
        .unwrap();
    assert!(result.content.chars().count() <= 8_000);
    assert!(result.content.contains("omitted"));
    assert!(result.content.contains("shortened to 400 characters"));
    assert!(result.content.contains("8000-character limit"));
    assert!(result.truncated);
}

#[test]
fn repo_evidence_requires_repo_lookup() {
    let temp = temp_root();
    let result = experimental_tools(
        policy(temp, false),
        ExperimentalToolsConfig {
            repo_evidence: true,
            ..Default::default()
        },
    );
    assert!(result.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn repo_lookup_skips_symlink_loops_and_external_targets() {
    for repo_evidence in [false, true] {
        use std::os::unix::fs::symlink;
        let temp = temp_root();
        let root = temp.join("repo");
        std::fs::create_dir(&root).unwrap();
        let outside = temp.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret.rs"), "needle\n").unwrap();
        symlink(&root, root.join("loop")).unwrap();
        symlink(&outside, root.join("external")).unwrap();
        let tools = experimental_tools(
            policy(root, false),
            ExperimentalToolsConfig {
                repo_lookup: true,
                repo_evidence,
                ..Default::default()
            },
        )
        .unwrap();
        let lookup = tools
            .iter()
            .find(|t| t.definition().name == "repo_lookup")
            .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            invoke(lookup.as_ref(), serde_json::json!({"symbol":"needle"})),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!result.content.contains("secret.rs"));
    }
}

#[tokio::test]
async fn repo_lookup_observes_cancellation() {
    for repo_evidence in [false, true] {
        let root = temp_root();
        let tools = experimental_tools(
            policy(root, false),
            ExperimentalToolsConfig {
                repo_lookup: true,
                repo_evidence,
                ..Default::default()
            },
        )
        .unwrap();
        let lookup = tools
            .iter()
            .find(|t| t.definition().name == "repo_lookup")
            .unwrap();
        let call = ToolCall {
            call_id: "cancel-lookup".into(),
            name: "repo_lookup".into(),
            args: serde_json::json!({"symbol":"needle"}),
        };
        let CallStatus::Dispatch(inv) = lookup.prepare(&call) else {
            panic!("dispatch")
        };
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(lookup.execute(inv, cancel).await.is_err());
    }
}

#[tokio::test]
async fn multi_edit_validates_every_hunk_before_writing() {
    let temp = temp_root();
    let a = temp.join("a.txt");
    let b = temp.join("b.txt");
    std::fs::write(&a, "one\n").unwrap();
    std::fs::write(&b, "two\n").unwrap();
    let tools = experimental_tools(
        policy(temp.clone(), false),
        ExperimentalToolsConfig {
            multi_edit: true,
            ..Default::default()
        },
    )
    .unwrap();
    let edit = tools
        .iter()
        .find(|t| t.definition().name == "multi_edit")
        .unwrap();
    let result = invoke(
        edit.as_ref(),
        serde_json::json!({"edits":[
            {"path":"a.txt","search":"one","replace":"ONE"},
            {"path":"b.txt","search":"missing","replace":"TWO"}
        ]}),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(a).unwrap(), "one\n");
    assert_eq!(std::fs::read_to_string(b).unwrap(), "two\n");
}

#[tokio::test]
async fn multi_edit_updates_multiple_files_and_rejects_escape() {
    let temp = temp_root();
    std::fs::write(temp.join("a.txt"), "one\n").unwrap();
    std::fs::write(temp.join("b.txt"), "two\n").unwrap();
    let tools = experimental_tools(
        policy(temp.clone(), false),
        ExperimentalToolsConfig {
            multi_edit: true,
            ..Default::default()
        },
    )
    .unwrap();
    let edit = tools
        .iter()
        .find(|t| t.definition().name == "multi_edit")
        .unwrap();
    let ok = invoke(
        edit.as_ref(),
        serde_json::json!({"edits":[
            {"path":"a.txt","search":"one","replace":"ONE"},
            {"path":"b.txt","search":"two","replace":"TWO"}
        ]}),
    )
    .await
    .unwrap();
    assert!(ok.success);
    assert_eq!(
        std::fs::read_to_string(temp.join("a.txt")).unwrap(),
        "ONE\n"
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("b.txt")).unwrap(),
        "TWO\n"
    );
    assert!(invoke(
        edit.as_ref(),
        serde_json::json!({"edits":[{"path":"../outside","search":"x","replace":"y"}]})
    )
    .await
    .is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn multi_edit_rejects_symlink_escape() {
    let temp = temp_root();
    let root = temp.join("repo");
    std::fs::create_dir(&root).unwrap();
    let outside = temp.join("outside.txt");
    std::fs::write(&outside, "secret").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link.txt")).unwrap();
    let tools = experimental_tools(
        policy(root.clone(), false),
        ExperimentalToolsConfig {
            multi_edit: true,
            ..Default::default()
        },
    )
    .unwrap();
    let edit = tools
        .iter()
        .find(|t| t.definition().name == "multi_edit")
        .unwrap();
    assert!(invoke(
        edit.as_ref(),
        serde_json::json!({"edits":[{"path":"link.txt","search":"secret","replace":"changed"}]})
    )
    .await
    .is_err());
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "secret");
}

#[tokio::test]
async fn process_pipe_session_accepts_input_and_stops() {
    let temp = temp_root();
    let tools = experimental_tools(
        policy(temp, true),
        ExperimentalToolsConfig {
            processes: true,
            ..Default::default()
        },
    )
    .unwrap();
    let proc = tools
        .iter()
        .find(|t| t.definition().name == "process")
        .unwrap();
    let start = invoke(
        proc.as_ref(),
        serde_json::json!({"action":"start","cmd":"cat"}),
    )
    .await
    .unwrap();
    let id = start
        .content
        .split("id=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    invoke(
        proc.as_ref(),
        serde_json::json!({"action":"input","id":id,"data":"hello\\n"}),
    )
    .await
    .unwrap();
    let poll = invoke(proc.as_ref(), serde_json::json!({"action":"poll","id":id}))
        .await
        .unwrap();
    assert!(poll.content.contains("hello"));
    let stop = invoke(proc.as_ref(), serde_json::json!({"action":"stop","id":id}))
        .await
        .unwrap();
    assert!(stop.content.contains("reaped"));
    assert!(
        invoke(proc.as_ref(), serde_json::json!({"action":"poll","id":id}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn process_poll_reports_nonzero_exit_as_failure() {
    let temp = temp_root();
    let tools = experimental_tools(
        policy(temp, true),
        ExperimentalToolsConfig {
            processes: true,
            ..Default::default()
        },
    )
    .unwrap();
    let proc = tools
        .iter()
        .find(|t| t.definition().name == "process")
        .unwrap();
    let start = invoke(
        proc.as_ref(),
        serde_json::json!({"action":"start","cmd":"sh -c 'exit 7'"}),
    )
    .await
    .unwrap();
    let id = start
        .content
        .split("id=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let poll = invoke(proc.as_ref(), serde_json::json!({"action":"poll","id":id}))
        .await
        .unwrap();
    assert!(!poll.success);
    assert!(poll.content.contains("exit=exit status: 7") || poll.content.contains("exit=7"));
}

#[tokio::test]
async fn process_poll_reports_output_dropped_from_utf8_ring_buffer() {
    let temp = temp_root();
    let tools = experimental_tools(
        policy(temp, true),
        ExperimentalToolsConfig {
            processes: true,
            ..Default::default()
        },
    )
    .unwrap();
    let proc = tools
        .iter()
        .find(|t| t.definition().name == "process")
        .unwrap();
    let start = invoke(
        proc.as_ref(),
        serde_json::json!({"action":"start","cmd":"python3 -c \"print('🙂'*30000)\""}),
    )
    .await
    .unwrap();
    let id = start
        .content
        .split("id=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let poll = invoke(
        proc.as_ref(),
        serde_json::json!({"action":"poll","id":id,"limit":32768}),
    )
    .await
    .unwrap();
    assert!(
        poll.truncated,
        "loss of ring-buffer output must be reported even when retained chars fit"
    );
    invoke(proc.as_ref(), serde_json::json!({"action":"stop","id":id}))
        .await
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn process_adds_docker_exec_interactive_flag_and_forwards_input() {
    use std::os::unix::fs::PermissionsExt;
    let temp = temp_root();
    let docker = temp.join("docker");
    std::fs::write(
        &docker,
        "#!/bin/sh\nprintf '%s\\n' \"$*\"\nshift 3\nexec \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut p = (*policy(temp, true)).clone();
    p.exec_wrap = Some(vec![
        docker.display().to_string(),
        "exec".into(),
        "fake-container".into(),
    ]);
    let tools = experimental_tools(
        Arc::new(p),
        ExperimentalToolsConfig {
            processes: true,
            ..Default::default()
        },
    )
    .unwrap();
    let proc = tools
        .iter()
        .find(|t| t.definition().name == "process")
        .unwrap();
    let start = invoke(
        proc.as_ref(),
        serde_json::json!({"action":"start","cmd":"cat"}),
    )
    .await
    .unwrap();
    let id = start
        .content
        .split("id=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    invoke(
        proc.as_ref(),
        serde_json::json!({"action":"input","id":id,"data":"from-container\\n"}),
    )
    .await
    .unwrap();
    let poll = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let poll = invoke(proc.as_ref(), serde_json::json!({"action":"poll","id":id}))
                .await
                .unwrap()
                .content;
            if poll.contains("from-container") {
                break poll;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("forwarded process input before deadline");
    assert!(poll.contains("-i fake-container cat"));
    invoke(proc.as_ref(), serde_json::json!({"action":"stop","id":id}))
        .await
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_process_tool_kills_its_process_group() {
    let temp = temp_root();
    let marker = temp.join("after-drop");
    let tools = experimental_tools(
        policy(temp.clone(), true),
        ExperimentalToolsConfig {
            processes: true,
            ..Default::default()
        },
    )
    .unwrap();
    let proc = tools
        .iter()
        .find(|t| t.definition().name == "process")
        .unwrap();
    let command = format!("sh -c 'sleep 0.5; touch {}'", marker.display());
    invoke(
        proc.as_ref(),
        serde_json::json!({"action":"start","cmd":command}),
    )
    .await
    .unwrap();
    drop(tools);
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    assert!(!marker.exists(), "descendant survived process tool drop");
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_session_after_leader_exit_kills_remaining_group() {
    let temp = temp_root();
    let marker = temp.join("orphan-survived");
    let command = format!(
        "python3 -c \"import subprocess; subprocess.Popen(['sh','-c','sleep 0.5; touch {}'])\"",
        marker.display()
    );
    let tools = experimental_tools(
        policy(temp.clone(), true),
        ExperimentalToolsConfig {
            processes: true,
            ..Default::default()
        },
    )
    .unwrap();
    let proc = tools
        .iter()
        .find(|t| t.definition().name == "process")
        .unwrap();
    let start = invoke(
        proc.as_ref(),
        serde_json::json!({"action":"start","cmd":command}),
    )
    .await
    .unwrap();
    let id = start
        .content
        .split("id=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    // interpreter start-up is unbounded under load: wait for the leader, don't guess
    let mut poll = invoke(proc.as_ref(), serde_json::json!({"action":"poll","id":id}))
        .await
        .unwrap();
    for _ in 0..100 {
        if !poll.content.contains("running") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        poll = invoke(proc.as_ref(), serde_json::json!({"action":"poll","id":id}))
            .await
            .unwrap();
    }
    assert!(
        !poll.content.contains("running"),
        "leader still running: {}",
        poll.content
    );
    drop(tools);
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    assert!(
        !marker.exists(),
        "descendant survived after leader exit and registry drop"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_process_stop_still_kills_and_reaps_child() {
    let temp = temp_root();
    let tools = experimental_tools(
        policy(temp, true),
        ExperimentalToolsConfig {
            processes: true,
            ..Default::default()
        },
    )
    .unwrap();
    let proc = tools
        .iter()
        .find(|t| t.definition().name == "process")
        .unwrap();
    let start = invoke(
        proc.as_ref(),
        serde_json::json!({"action":"start","cmd":"sleep 30"}),
    )
    .await
    .unwrap();
    let id = start
        .content
        .split("id=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let call = ToolCall {
        call_id: "cancel-stop".into(),
        name: "process".into(),
        args: serde_json::json!({"action":"stop","id":id}),
    };
    let CallStatus::Dispatch(inv) = proc.prepare(&call) else {
        panic!("dispatch")
    };
    let cancel = CancellationToken::new();
    cancel.cancel();
    let stopped = proc.execute(inv, cancel).await;
    assert!(stopped.is_err());
    let poll = invoke(proc.as_ref(), serde_json::json!({"action":"poll","id":id})).await;
    assert!(poll.is_err(), "cancelled stop must remove the session");
}
