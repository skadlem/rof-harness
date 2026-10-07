use super::*;
use std::time::{Duration, SystemTime};

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
