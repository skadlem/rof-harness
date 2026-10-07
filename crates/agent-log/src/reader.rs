use crate::{Item, ItemKind, LOG_VERSION};
use std::path::Path;

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
