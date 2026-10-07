//! `--dump-events` JSONL mirror: an incremental per-event listener with a
//! post-run completeness close-out.

use std::path::Path;
use std::sync::{Arc, Mutex};

use agent_event::{AgentEvent, Emitter};

/// Incremental `--dump-events` mirror: truncate the previous dump (one run,
/// one dump, same as before), then append each emitted event as one JSON
/// line with flush + fsync before returning, so a kill leaves a valid
/// prefix on disk instead of no file at all. The listener is sync and
/// infallible by construction (every failure is skipped, never panics), so
/// a slow disk cannot hang or break the run; the WAL stays the fail-closed
/// record, this file its best-effort mirror.
pub(crate) fn attach_dump(emitter: &mut Emitter, path: &str) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    if let Some(dir) = Path::new(path).parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let shared = Arc::new(Mutex::new(file));
    emitter.on(move |event| {
        if let Ok(line) = serde_json::to_string(event) {
            if let Ok(mut f) = shared.lock() {
                use std::io::Write as _;
                let mut buf = line.into_bytes();
                buf.push(b'\n');
                let _ = f.write_all(&buf);
                let _ = f.flush();
                let _ = f.sync_all();
            }
        }
    });
    Ok(())
}

/// One run's events as JSONL (one object per line, LF) via `trace::TraceSink`.
/// The sink only appends, so a dump replaces the previous file: one run, one dump.
pub(crate) fn write_dump(path: &str, events: &[AgentEvent]) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    let sink = trace::TraceSink::with_file(Path::new(path)).map_err(|e| e.to_string())?;
    for e in events {
        sink.emit(e.clone());
    }
    Ok(())
}

/// Post-run `--dump-events` close-out: the incremental listener is the
/// record, so when its line count already equals the lossless history the
/// kill-resilient prefix stands as-is (no truncating rewrite). Any
/// shortfall (attach failed, writes lost) falls back to the full replace
/// write, preserving the old path/flag/error behavior.
pub(crate) fn finalize_dump(path: &str, events: &[AgentEvent]) -> Result<(), String> {
    let complete = std::fs::read_to_string(path)
        .map(|t| t.lines().count() == events.len())
        .unwrap_or(false);
    if complete {
        return Ok(());
    }
    write_dump(path, events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{args_for, text_resp, tool_resp, ScriptClient};

    #[tokio::test]
    async fn dump_events_is_one_json_object_per_line() {
        use crate::execute;
        let dir = crate::fixtures::tmp();
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        let client = ScriptClient::new(vec![tool_resp(), text_resp()]);
        let r = execute(&client, &args_for(&dir, None)).await;
        assert!(!r.events.is_empty(), "fake run must produce events");
        let path = dir.join("dump.jsonl");
        let p = path.to_str().unwrap();
        write_dump(p, &r.events).unwrap();
        // Second dump to the same path replaces, never appends.
        write_dump(p, &r.events).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'), "file ends with LF");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), r.events.len(), "N events, N lines");
        for line in &lines {
            let v: serde_json::Value =
                serde_json::from_str(line).expect("every line incl. the last parses alone");
            assert!(v.is_object(), "line is a JSON object: {line}");
        }
    }

    #[tokio::test]
    async fn dump_events_exists_mid_run_and_wal_live() {
        use crate::execute;
        let dir = crate::fixtures::tmp();
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        let dump = dir.join("dump.jsonl");
        let client = ScriptClient::with_dump_probe(vec![tool_resp(), text_resp()], dump.clone());
        let mut args = args_for(&dir, None);
        args.dump_events = Some(dump.to_str().unwrap().into());
        let r = execute(&client, &args).await;
        assert!(
            *client.probe_hit.lock().unwrap(),
            "dump file holds JSON lines before the first provider call returns"
        );
        assert!(!r.events.is_empty(), "fake run must produce events");
        let text = std::fs::read_to_string(&dump).unwrap();
        assert_eq!(
            text.lines().count(),
            r.events.len(),
            "incremental prefix is the complete record post-run"
        );
        assert!(
            dir.join(".rof-events.jsonl").is_file(),
            "default WAL is live on the shipped path"
        );
        let patch = r.patch.unwrap();
        assert!(
            !patch.contains("dump.jsonl") && !patch.contains(".rof-events.jsonl"),
            "sidecars stay out of the reported patch: {patch}",
        );
    }
}
