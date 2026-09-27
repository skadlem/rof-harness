//! Engine-owned diff evidence as a durable trace event (§4.2).
//!
//! The diff pane of a later task reads these snapshots, so what is asserted
//! here is the contract the pane depends on: the patch text is bounded and
//! visibly truncated when it is cut, an untracked file still contributes
//! evidence (plain `git diff` cannot see one), a successful rollback clears
//! the record rather than leaving a pane showing changes the tree no longer
//! has, and the `App` reducer stores the snapshot without spending a
//! transcript or activity line on it. No terminal, no network, no model
//! beyond a canned fake: real git in a real temp workdir.

use async_trait::async_trait;
use rof::config::{AppConfig, PermissionPolicy};
use rof::engine::tree::{
    PatchText, TreeService, PATCH_MAX_BYTES, PATCH_MAX_LINES, PATCH_TRUNCATED_MARKER,
};
use rof::engine::{Orchestrator, Session};
use rof::llm::{ContextService, ExecutorService, LlmClient, LlmError, LlmReq, LlmResp};
use rof::obs::{TraceEvent, TraceSink};
use rof::tools::{FsListTool, FsPatchTool, FsReadTool, ToolRegistry};
use rof::tui::app::App;
use std::sync::Arc;

/// One named dir under the temp dir, cleared first, so every test gets a
/// hermetic git workdir without a temp-dir crate.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rof-diffsnap-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A committed one-file tree, the state an attempt starts from.
fn committed(dir: &std::path::Path) -> TreeService {
    std::fs::write(dir.join("a.txt"), "before\n").unwrap();
    let tree = TreeService::new(dir.to_path_buf());
    tree.ensure().unwrap();
    tree.baseline().unwrap();
    tree
}

/// The patch text and truncation flag, as the trace event will carry them.
fn patch_of(tree: &TreeService) -> PatchText {
    let diff = tree.diff().unwrap();
    tree.patch(&diff).unwrap()
}

#[test]
fn a_one_file_edit_yields_patch_text_and_the_name() {
    let dir = scratch("one-edit");
    let tree = committed(&dir);
    std::fs::write(dir.join("a.txt"), "before\nafter\n").unwrap();

    let diff = tree.diff().unwrap();
    assert_eq!(diff.names, vec!["a.txt"]);
    let patch = tree.patch(&diff).unwrap();
    assert!(
        patch.text.contains("+after"),
        "the patch carries the added line: {}",
        patch.text
    );
    assert!(!patch.truncated, "a one-line edit is not truncated");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Byte cap: a diff of long lines is cut, the cut is visible in the text,
/// and the retained bytes are whole UTF-8 codepoints (the file is multi-byte
/// on purpose, so a byte-indexed cut would show a replacement char).
#[test]
fn a_huge_diff_is_bounded_by_bytes_without_splitting_a_codepoint() {
    let dir = scratch("huge-bytes");
    let tree = committed(&dir);
    let big: String = (0..2_000)
        .map(|i| format!("ünïcödé line {i} — with em dash\n"))
        .collect();
    std::fs::write(dir.join("a.txt"), &big).unwrap();

    let diff = tree.diff().unwrap();
    let patch = tree.patch(&diff).unwrap();
    assert!(patch.truncated, "a huge diff must report truncation");
    assert!(
        patch.text.contains(PATCH_TRUNCATED_MARKER),
        "the truncation is visible in the text: {:?}",
        patch.text
    );
    assert!(
        !patch.text.contains('\u{FFFD}'),
        "the cut is on a codepoint boundary"
    );
    let body = patch
        .text
        .split(PATCH_TRUNCATED_MARKER)
        .next()
        .unwrap_or_default();
    assert!(
        body.len() <= PATCH_MAX_BYTES,
        "the patch body respects the byte cap: {}",
        body.len()
    );
    assert!(
        patch.text.len() < big.len(),
        "the returned text is bounded, not the whole diff: {} vs {}",
        patch.text.len(),
        big.len()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Line cap: hundreds of tiny lines stay under the byte cap, so only the
/// line count can bound them. Both caps exist for this reason.
#[test]
fn a_diff_of_many_short_lines_is_bounded_by_lines() {
    let dir = scratch("huge-lines");
    let tree = committed(&dir);
    // One character per line, so the whole patch is far under the byte cap
    // and only the line count can bound it.
    let many: String = (0..400)
        .map(|i| format!("{}\n", (b'a' + (i % 26) as u8) as char))
        .collect();
    std::fs::write(dir.join("a.txt"), &many).unwrap();

    let patch = patch_of(&tree);
    assert!(patch.truncated, "too many lines must report truncation");
    assert!(patch.text.contains(PATCH_TRUNCATED_MARKER));
    let shown = patch
        .text
        .split(PATCH_TRUNCATED_MARKER)
        .next()
        .unwrap_or_default()
        .lines()
        .count();
    assert!(
        shown <= PATCH_MAX_LINES,
        "at most the line cap is shown: {shown}"
    );
    // The line cap is what bites here, not the byte cap: this diff is well
    // under PATCH_MAX_BYTES, so a bound on bytes alone would not hold.
    assert!(
        many.len() < PATCH_MAX_BYTES,
        "the fixture is under the byte cap"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `git diff` is empty for a file the attempt created, so the patch evidence
/// has to name it from the name set `diff()` already collected — otherwise a
/// pane would show "no changes" for a new file.
#[test]
fn a_new_untracked_file_still_contributes_evidence() {
    let dir = scratch("untracked");
    let tree = committed(&dir);
    std::fs::write(dir.join("new.txt"), "brand new\n").unwrap();

    let diff = tree.diff().unwrap();
    let patch = tree.patch(&diff).unwrap();
    assert_eq!(diff.names, vec!["new.txt"]);
    assert!(
        patch.text.contains("new.txt"),
        "the untracked file is named in the evidence: {}",
        patch.text
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A committed `DiffSnapshot` line is the durable form of the pane's data:
/// the pinned shape is what a recorded session's JSONL must keep parsing as.
#[test]
fn the_diff_snapshot_json_shape_round_trips() {
    let ev = TraceEvent::DiffSnapshot {
        names: vec!["a.txt".into(), "b.txt".into()],
        stat: " a.txt | 1 +\n".into(),
        patch: "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,2 @@\n before\n+after\n".into(),
        truncated: true,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert_eq!(
        json,
        r#"{"DiffSnapshot":{"names":["a.txt","b.txt"],"stat":" a.txt | 1 +\n","patch":"--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,2 @@\n before\n+after\n","truncated":true}}"#
    );
    let back: TraceEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(serde_json::to_string(&back).unwrap(), json);
}

/// Answers the three roles a pipeline run needs. Round 1 patches a file (so
/// the write gate sees a real change) and the reviewer fails, which is what
/// makes the loop roll back.
struct RollbackClient;

#[async_trait]
impl LlmClient for RollbackClient {
    async fn complete(&self, _model: &str, req: LlmReq) -> Result<LlmResp, LlmError> {
        let text = if req.system.contains("reviewer") {
            r#"{"pass": false, "feedback": "not yet"}"#
        } else if req.system.contains("implementer") {
            r#"{"patches":[{"path":"a.txt","search":"before","replace":"after"}],"notes":"edited"}"#
        } else {
            r#"{"artifact": "did it", "notes": "ok"}"#
        };
        Ok(LlmResp {
            text: text.to_string(),
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 1,
            cost_usd: None,
            cached_input_tokens: 0,
            attempts: 1,
        })
    }
}

#[tokio::test]
async fn a_successful_rollback_emits_an_empty_snapshot() {
    let root = scratch("rollback");
    std::fs::write(root.join("a.txt"), "before\n").unwrap();
    let policy = PermissionPolicy {
        allowed_dirs: vec![root.clone()],
        ..Default::default()
    };
    let mut reg = ToolRegistry::new(policy);
    reg.register(FsListTool::new(root.clone()));
    reg.register(FsReadTool::new(root.clone()));
    reg.register(FsPatchTool::new(root.clone()));

    let trace = Arc::new(TraceSink::new());
    let cfg = AppConfig {
        max_review_rounds: 2,
        budgets: rof::config::TokenBudgets {
            long_term: 2000,
            mid_term: 4000,
            short_term: 6000,
        },
        ..Default::default()
    };
    let client = Arc::new(RollbackClient);
    let orch = Orchestrator::new(
        cfg,
        trace.clone(),
        ContextService::new(client.clone(), "fake-ctx".into()),
        ExecutorService::new(client.clone(), "fake-exec".into(), None),
        ExecutorService::new(client, "fake-verify".into(), None),
    );
    orch.run_loop(&Session::new("g".into()), &reg, &root).await;

    let events = trace.events();
    let snapshots: Vec<&TraceEvent> = events
        .iter()
        .filter(|e| matches!(e, TraceEvent::DiffSnapshot { .. }))
        .collect();
    assert!(
        !snapshots.is_empty(),
        "the diff sites must emit a snapshot: {:?}",
        events
    );
    let rolled = events
        .iter()
        .position(|e| {
            matches!(
                e,
                TraceEvent::StateTransition { to, .. } if to == "rolled_back"
            )
        })
        .expect("the failing round must roll back");
    match &events[rolled + 1] {
        TraceEvent::DiffSnapshot {
            names,
            stat,
            patch,
            truncated,
        } => {
            assert!(names.is_empty(), "rollback clears the names: {names:?}");
            assert!(stat.is_empty(), "rollback clears the stat: {stat:?}");
            assert!(patch.is_empty(), "rollback clears the patch: {patch:?}");
            assert!(!*truncated);
        }
        other => panic!("the empty snapshot follows the rollback: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The reducer stores the snapshot and spends no scrollback on it: the pane
/// is a view of the latest evidence, not a transcript entry.
#[test]
fn the_reducer_stores_the_snapshot_without_a_transcript_or_activity_line() {
    let mut app = App::new();
    app.begin_run("g");
    let before = (app.transcript.len(), app.activity.len());

    app.on_event(&TraceEvent::DiffSnapshot {
        names: vec!["a.txt".into()],
        stat: " a.txt | 1 +\n".into(),
        patch: "+after\n".into(),
        truncated: false,
    });

    let snap = app.diff_snapshot().expect("the snapshot is stored");
    assert_eq!(snap.names, vec!["a.txt"]);
    assert_eq!(snap.patch, "+after\n");
    assert!(!snap.truncated);
    assert_eq!(
        (app.transcript.len(), app.activity.len()),
        before,
        "a diff snapshot is a pane view, not scrollback"
    );
}

/// The empty snapshot a rollback records clears the pane, so the stored
/// state must be replaced, not merged.
#[test]
fn a_later_empty_snapshot_replaces_the_stored_one() {
    let mut app = App::new();
    app.on_event(&TraceEvent::DiffSnapshot {
        names: vec!["a.txt".into()],
        stat: "stat".into(),
        patch: "patch".into(),
        truncated: true,
    });
    app.on_event(&TraceEvent::DiffSnapshot {
        names: vec![],
        stat: String::new(),
        patch: String::new(),
        truncated: false,
    });
    let snap = app.diff_snapshot().expect("the snapshot is stored");
    assert!(snap.names.is_empty());
    assert!(snap.patch.is_empty());
    assert!(!snap.truncated);
}
