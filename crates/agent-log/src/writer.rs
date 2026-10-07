use crate::Item;
use std::fs::OpenOptions;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

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
