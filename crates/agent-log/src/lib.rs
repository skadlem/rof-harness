//! Append-only durable Item log (JSONL). See `research/crate-agent-log.md`.
//!
//! Locked rules: pre-effect items are appended and flushed before the effect;
//! a failed pre-effect append is a hard turn failure (propagate it and end the
//! turn, never run the effect from process-only state). One file has one
//! `LogWriter`. Framing is LF-only over raw bytes: `U+2028`/`U+2029` inside
//! strings are data, never record boundaries.
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
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

fn invalid(msg: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg)
}

fn kind_name(kind: &ItemKind) -> &'static str {
    match kind {
        ItemKind::Header { .. } => "Header",
        ItemKind::Input { .. } => "Input",
        ItemKind::TurnStart { .. } => "TurnStart",
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

/// Drop bytes after the last LF (the whole file if it has no LF): a log whose
/// last byte is LF ends on a record boundary, anything else is a crash-torn
/// partial write. Recorded items are never touched — they always end in LF.
fn truncate_torn_tail(path: &Path) -> std::io::Result<()> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    let mut reader = &file;
    let mut last = [0u8; 1];
    reader.seek(SeekFrom::End(-1))?;
    reader.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(());
    }
    let mut raw = Vec::new();
    reader.seek(SeekFrom::Start(0))?;
    reader.read_to_end(&mut raw)?;
    let keep = raw.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    drop(file);
    OpenOptions::new()
        .write(true)
        .open(path)?
        .set_len(keep as u64)
}

impl LogWriter {
    /// Open for create+append, repairing a torn tail on disk first: bytes after
    /// the last LF (or the whole file, if it has no LF) are dropped, so an
    /// append after a crash cannot merge an item into a fragment. Recorded
    /// items are never edited; a clean log (empty, or ending in LF) is left
    /// untouched. Repairing a file to 0 bytes puts the Header obligation on
    /// the caller: `read_log` requires the first item to be a `Header`.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        truncate_torn_tail(path)?;
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

/// One-shot append for tests. The steady path should hold one
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tmp_path() -> std::path::PathBuf {
        let n = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
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
    fn torn_tail_then_append() {
        let p = tmp_path();
        let mut w = LogWriter::open(&p).unwrap();
        w.append(&header(1)).unwrap();
        drop(w);
        // Crash mid-write: append a partial line with no LF.
        let mut raw = std::fs::read(&p).unwrap();
        raw.extend(b"{\"seq\":2,\"id\":\"id0002\",\"parent_id\":null");
        std::fs::write(&p, raw).unwrap();

        let mut w = LogWriter::open(&p).unwrap();
        w.append(&turn_start(2, "t1")).unwrap();
        w.append(&turn_end(3, "t1")).unwrap();
        drop(w);

        let items = read_log(&p).unwrap();
        assert_eq!(
            items.iter().map(|i| i.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(matches!(items[0].kind, ItemKind::Header { .. }));
        assert!(matches!(items[2].kind, ItemKind::TurnEnd { .. }));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn partial_only_file_is_truncated_to_zero() {
        let p = tmp_path();
        std::fs::write(&p, b"{\"seq\":1,\"id\":").unwrap(); // no LF, no Header
        let mut w = LogWriter::open(&p).unwrap();
        w.append(&header(1)).unwrap();
        drop(w);
        let items = read_log(&p).unwrap();
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0].kind, ItemKind::Header { .. }));
        std::fs::remove_file(&p).ok();
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
}
