//! Append-only durable Item log (JSONL). See `research/crate-agent-log.md`.
//!
//! Locked rules: pre-effect items are appended and flushed before the effect;
//! a failed pre-effect append is a hard turn failure (propagate it and end the
//! turn, never run the effect from process-only state). One file has one
//! `LogWriter`. Framing is LF-only over raw bytes: `U+2028`/`U+2029` inside
//! strings are data, never record boundaries.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

pub type Seq = u64;
pub type ItemId = String;
pub type InputId = String;
pub type TurnId = String;
pub type CallId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputSource {
    External,
    Control,
    Crash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnEndReason {
    Completed,
    Error(String),
    Interrupted,
    Budget,
    MaxTokens,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryCode {
    ToolNotStarted,
    ToolOutcomeUnknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ItemKind {
    Header {
        version: u32,
        session_id: String,
        cwd: String,
        model: String,
    },
    Input {
        input_id: InputId,
        text: String,
        source: InputSource,
    },
    TurnStart {
        turn_id: TurnId,
        prev_turn_id: Option<TurnId>,
    },
    System {
        sections: BTreeMap<String, Option<String>>,
        tools_added: Vec<serde_json::Value>,
        tools_removed: Vec<String>,
    },
    Assistant {
        message: serde_json::Value,
        stop_reason: String,
        interrupted: bool,
    },
    Attempt {
        error: String,
        will_retry: bool,
    },
    ToolCall {
        call_id: CallId,
        tool: String,
        args: serde_json::Value,
    },
    ToolResult {
        call_id: CallId,
        content: String,
        is_error: bool,
        recovery: Option<RecoveryCode>,
    },
    TurnEnd {
        turn_id: TurnId,
        reason: TurnEndReason,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub seq: Seq,
    pub id: ItemId,
    pub parent_id: Option<ItemId>,
    pub recorded_at: SystemTime,
    #[serde(flatten)]
    pub kind: ItemKind,
}

/// Current log format version. Unknown versions are rejected loudly on read;
/// there are no migrations.
pub const LOG_VERSION: u32 = 1;

/// Model-visible guidance carried by synthetic results for calls whose outcome
/// was never durably recorded. It tells the model to verify before retrying.
pub const UNKNOWN_OUTCOME_TEXT: &str = "The tool call was interrupted after it was recorded, but no result was durably recorded. Its outcome is unknown. Retry only if the operation is read-only or idempotent; if it may have side effects, first verify external state or ask the user. Do not retry blindly.";

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn new_id() -> ItemId {
    let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}{n:x}")
}

fn invalid(msg: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg)
}

fn kind_name(kind: &ItemKind) -> &'static str {
    match kind {
        ItemKind::Header { .. } => "Header",
        ItemKind::Input { .. } => "Input",
        ItemKind::TurnStart { .. } => "TurnStart",
        ItemKind::System { .. } => "System",
        ItemKind::Assistant { .. } => "Assistant",
        ItemKind::Attempt { .. } => "Attempt",
        ItemKind::ToolCall { .. } => "ToolCall",
        ItemKind::ToolResult { .. } => "ToolResult",
        ItemKind::TurnEnd { .. } => "TurnEnd",
    }
}

/// Single-owner append handle for one log file: one buffered `body + b'\n'`
/// write per item, flushed with an fsync barrier before `append` returns `Ok`,
/// so `Ok` means durable. Keep at most one live writer per file.
///
/// A failed pre-effect append is a hard turn failure: propagate the error and
/// end the turn, never run the effect from process-only state.
pub struct LogWriter {
    out: BufWriter<std::fs::File>,
}

impl LogWriter {
    /// Open for create+append. Never truncates, never edits in place.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            out: BufWriter::new(file),
        })
    }

    /// Append one item: single `body + b'\n'` write, then flush + `sync_all`.
    /// Only the in-flight line can be torn by a crash, never the middle.
    pub fn append(&mut self, item: &Item) -> std::io::Result<()> {
        let mut body = serde_json::to_vec(item)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        body.push(b'\n');
        self.out.write_all(&body)?;
        self.out.flush()?;
        self.out.get_ref().sync_all()?;
        Ok(())
    }
}

/// One-shot append for tests and repair. The steady path should hold one
/// `LogWriter` instead of reopening per item.
pub fn append_item(path: &Path, item: &Item) -> std::io::Result<()> {
    LogWriter::open(path)?.append(item)
}

fn strip_cr(line: &[u8]) -> &[u8] {
    match line.split_last() {
        Some((&b'\r', rest)) => rest,
        _ => line,
    }
}

/// Read and validate the whole log. LF-only framing over raw bytes (never
/// `str::lines()`, which splits `U+2028`/`U+2029` inside strings); an optional
/// preceding CR is stripped. An unparseable final line is a torn tail and is
/// dropped; corruption anywhere else is fatal. The first entry must be a
/// `Header` with a supported version, and `seq` must be gapless from 1.
pub fn read_log(path: &Path) -> std::io::Result<Vec<Item>> {
    let raw = std::fs::read(path)?;
    let chunks: Vec<(usize, &[u8])> = raw
        .split(|b| *b == b'\n')
        .enumerate()
        .map(|(i, c)| (i + 1, strip_cr(c)))
        .filter(|(_, c)| !c.is_empty())
        .collect();
    let n = chunks.len();
    let mut items = Vec::with_capacity(n);
    for (idx, (lineno, chunk)) in chunks.into_iter().enumerate() {
        match serde_json::from_slice::<Item>(chunk) {
            Ok(item) => items.push(item),
            Err(_) if idx + 1 == n => break, // torn tail: crash mid-write, drop it
            Err(e) => return Err(invalid(format!("line {lineno}: corrupt entry: {e}"))),
        }
    }
    validate_log(&items)?;
    Ok(items)
}

fn validate_log(items: &[Item]) -> std::io::Result<()> {
    let Some(first) = items.first() else {
        return Ok(());
    };
    let ItemKind::Header { version, .. } = &first.kind else {
        return Err(invalid(format!(
            "first entry must be Header, found {}",
            kind_name(&first.kind)
        )));
    };
    if *version != LOG_VERSION {
        return Err(invalid(format!(
            "unsupported log version {version}, expected {LOG_VERSION}; no migrations"
        )));
    }
    for (i, item) in items.iter().enumerate() {
        let want = i as u64 + 1;
        if item.seq != want {
            return Err(invalid(format!(
                "seq gap: expected {want}, found {}",
                item.seq
            )));
        }
    }
    Ok(())
}

/// Synthesize closers for an open turn: one `ToolResult` with
/// `ToolOutcomeUnknown` per `ToolCall` lacking a result, then `TurnEnd` with
/// `Interrupted`. `seq` continues past the last real item and timestamps reuse
/// the last real event, so repair invents no future. Balanced or empty logs
/// return nothing. Closed turn boundaries discard pending calls.
pub fn open_turn_closers(items: &[Item]) -> Vec<Item> {
    let mut open: Option<&Item> = None;
    let mut pending: Vec<&CallId> = Vec::new();
    for item in items {
        match &item.kind {
            ItemKind::TurnStart { .. } => {
                open = Some(item);
                pending.clear();
            }
            ItemKind::TurnEnd { .. } => {
                open = None;
                pending.clear();
            }
            ItemKind::ToolCall { call_id, .. } => {
                if open.is_some() && !pending.contains(&call_id) {
                    pending.push(call_id);
                }
            }
            ItemKind::ToolResult { call_id, .. } => {
                pending.retain(|id| *id != call_id);
            }
            _ => {}
        }
    }
    let Some(start) = open else {
        return Vec::new();
    };
    let turn_id = match &start.kind {
        ItemKind::TurnStart { turn_id, .. } => turn_id.clone(),
        _ => return Vec::new(),
    };
    let mut seq = items.last().map(|i| i.seq + 1).unwrap_or(1);
    let recorded_at = items
        .last()
        .map(|i| i.recorded_at)
        .unwrap_or_else(SystemTime::now);
    let mut out = Vec::with_capacity(pending.len() + 1);
    for call_id in pending {
        out.push(Item {
            seq,
            id: new_id(),
            parent_id: None,
            recorded_at,
            kind: ItemKind::ToolResult {
                call_id: call_id.clone(),
                content: UNKNOWN_OUTCOME_TEXT.to_string(),
                is_error: true,
                recovery: Some(RecoveryCode::ToolOutcomeUnknown),
            },
        });
        seq += 1;
    }
    out.push(Item {
        seq,
        id: new_id(),
        parent_id: None,
        recorded_at,
        kind: ItemKind::TurnEnd {
            turn_id,
            reason: TurnEndReason::Interrupted,
        },
    });
    out
}

/// Crash resume: read + validate, append any closers through a single-owner
/// handle, return the full state. Balanced logs are untouched (no write).
pub fn resume_log(path: &Path) -> std::io::Result<Vec<Item>> {
    let mut items = read_log(path)?;
    let closers = open_turn_closers(&items);
    if !closers.is_empty() {
        let mut writer = LogWriter::open(path)?;
        for closer in closers {
            writer.append(&closer)?;
            items.push(closer);
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tmp_path() -> std::path::PathBuf {
        let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("agent-log-test-{}-{n}", std::process::id()))
    }

    fn ts() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    fn item(seq: Seq, kind: ItemKind) -> Item {
        Item {
            seq,
            id: format!("id{seq:04}"),
            parent_id: None,
            recorded_at: ts(),
            kind,
        }
    }

    fn header(seq: Seq) -> Item {
        item(
            seq,
            ItemKind::Header {
                version: LOG_VERSION,
                session_id: "s".into(),
                cwd: "/".into(),
                model: "m".into(),
            },
        )
    }

    fn turn_start(seq: Seq, turn: &str) -> Item {
        item(
            seq,
            ItemKind::TurnStart {
                turn_id: turn.into(),
                prev_turn_id: None,
            },
        )
    }

    fn tool_call(seq: Seq, call: &str) -> Item {
        item(
            seq,
            ItemKind::ToolCall {
                call_id: call.into(),
                tool: "write".into(),
                args: serde_json::json!({}),
            },
        )
    }

    fn tool_result(seq: Seq, call: &str) -> Item {
        item(
            seq,
            ItemKind::ToolResult {
                call_id: call.into(),
                content: "ok".into(),
                is_error: false,
                recovery: None,
            },
        )
    }

    fn turn_end(seq: Seq, turn: &str) -> Item {
        item(
            seq,
            ItemKind::TurnEnd {
                turn_id: turn.into(),
                reason: TurnEndReason::Completed,
            },
        )
    }

    fn raw_line(item: &Item) -> Vec<u8> {
        let mut v = serde_json::to_vec(item).unwrap();
        v.push(b'\n');
        v
    }

    #[test]
    fn torn_tail_is_dropped() {
        let p = tmp_path();
        let mut raw = raw_line(&header(1));
        raw.extend(raw_line(&turn_start(2, "t1")));
        raw.extend(b"{\"seq\":3,\"id\":\"id0003\""); // truncated mid-write
        std::fs::write(&p, raw).unwrap();
        let items = read_log(&p).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].seq, 2);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn mid_file_corruption_is_fatal() {
        let p = tmp_path();
        let mut raw = raw_line(&header(1));
        raw.extend(raw_line(&turn_start(2, "t1")));
        raw.extend(b"not json\n");
        raw.extend(raw_line(&turn_end(3, "t1")));
        std::fs::write(&p, raw).unwrap();
        let err = read_log(&p).unwrap_err();
        assert!(err.to_string().contains("line 3"), "{err}");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn seq_gap_is_fatal() {
        let p = tmp_path();
        let mut raw = raw_line(&header(1));
        raw.extend(raw_line(&turn_start(2, "t1")));
        raw.extend(raw_line(&turn_end(4, "t1")));
        std::fs::write(&p, raw).unwrap();
        let err = read_log(&p).unwrap_err();
        assert!(err.to_string().contains("seq gap"), "{err}");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn first_entry_must_be_header() {
        let p = tmp_path();
        std::fs::write(&p, raw_line(&turn_start(1, "t1"))).unwrap();
        let err = read_log(&p).unwrap_err();
        assert!(err.to_string().contains("Header"), "{err}");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn unknown_version_rejected_loudly() {
        let p = tmp_path();
        let bad = item(
            1,
            ItemKind::Header {
                version: LOG_VERSION + 1,
                session_id: "s".into(),
                cwd: "/".into(),
                model: "m".into(),
            },
        );
        std::fs::write(&p, raw_line(&bad)).unwrap();
        let err = read_log(&p).unwrap_err();
        assert!(err.to_string().contains("version"), "{err}");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn framing_is_lf_only_and_u2028_safe() {
        let p = tmp_path();
        let mut raw = raw_line(&header(1));
        let input = item(
            2,
            ItemKind::Input {
                input_id: "i".into(),
                text: "a<>b".into(),
                source: InputSource::External,
            },
        );
        let mut line = raw_line(&input);
        // Splice literal U+2028 bytes (E2 80 A8) into the string: a
        // str::lines() reader would split the record here; b'\n' split must not.
        let needle: &[u8] = b"<>";
        let pos = line.windows(2).position(|w| w == needle).unwrap();
        line.splice(pos..pos + 2, [0xE2, 0x80, 0xA8]);
        raw.extend(&line);
        // CRLF tolerance: a \r\n-terminated line still parses as one record.
        let mut last = raw_line(&turn_end(3, "t1"));
        last.pop();
        last.extend(b"\r\n");
        raw.extend(&last);
        std::fs::write(&p, raw).unwrap();
        let items = read_log(&p).unwrap();
        assert_eq!(items.len(), 3);
        match &items[1].kind {
            ItemKind::Input { text, .. } => assert_eq!(text, "a\u{2028}b"),
            other => panic!("expected Input, got {other:?}"),
        }
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn closers_synthesize_unknown_outcome_and_interrupted_end() {
        let items = vec![header(1), turn_start(2, "t1"), tool_call(3, "c1")];
        let closers = open_turn_closers(&items);
        assert_eq!(closers.len(), 2);
        assert_eq!(closers[0].seq, 4);
        assert_eq!(closers[1].seq, 5);
        assert_eq!(closers[0].recorded_at, ts());
        assert_eq!(closers[1].recorded_at, ts());
        match &closers[0].kind {
            ItemKind::ToolResult {
                call_id,
                content,
                is_error: true,
                recovery: Some(RecoveryCode::ToolOutcomeUnknown),
            } => {
                assert_eq!(call_id, "c1");
                assert!(content.contains("Do not retry blindly"), "{content}");
            }
            other => panic!("expected synthetic ToolResult, got {other:?}"),
        }
        match &closers[1].kind {
            ItemKind::TurnEnd {
                turn_id,
                reason: TurnEndReason::Interrupted,
            } => assert_eq!(turn_id, "t1"),
            other => panic!("expected Interrupted TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn balanced_log_needs_no_closers() {
        let full = vec![
            header(1),
            turn_start(2, "t1"),
            tool_call(3, "c1"),
            tool_result(4, "c1"),
            turn_end(5, "t1"),
        ];
        assert!(open_turn_closers(&full).is_empty());
        assert!(open_turn_closers(&[]).is_empty());
        // Answered call but still-open turn closes just the turn.
        let open = vec![
            header(1),
            turn_start(2, "t1"),
            tool_call(3, "c1"),
            tool_result(4, "c1"),
        ];
        let closers = open_turn_closers(&open);
        assert_eq!(closers.len(), 1);
        assert!(matches!(
            &closers[0].kind,
            ItemKind::TurnEnd {
                reason: TurnEndReason::Interrupted,
                ..
            }
        ));
    }

    #[test]
    fn writer_round_trip() {
        let p = tmp_path();
        let mut w = LogWriter::open(&p).unwrap();
        w.append(&header(1)).unwrap();
        w.append(&turn_start(2, "t1")).unwrap();
        drop(w);
        let items = read_log(&p).unwrap();
        assert_eq!(items.len(), 2);
        assert!(matches!(items[0].kind, ItemKind::Header { .. }));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn resume_appends_closers_and_is_idempotent() {
        let p = tmp_path();
        let mut w = LogWriter::open(&p).unwrap();
        w.append(&header(1)).unwrap();
        w.append(&turn_start(2, "t1")).unwrap();
        w.append(&tool_call(3, "c1")).unwrap();
        drop(w);
        let items = resume_log(&p).unwrap();
        assert_eq!(items.len(), 5);
        assert!(matches!(items[3].kind, ItemKind::ToolResult { .. }));
        assert!(matches!(items[4].kind, ItemKind::TurnEnd { .. }));
        let again = resume_log(&p).unwrap();
        assert_eq!(again.len(), 5);
        std::fs::remove_file(&p).ok();
    }
}
