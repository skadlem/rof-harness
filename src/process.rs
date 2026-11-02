//! Shared bounded command execution for tools and harness-owned checks.
use std::{collections::HashMap, path::Path, process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
pub const OUTPUT_LIMIT: usize = 64 * 1024;

#[derive(Debug)]
pub struct ProcessOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}

// Killing only the leader leaves build/test workers running in a rolled-back
// tree. The guard also runs when the enclosing future is cancelled.
struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}

async fn collect(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    cap: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let keep = n.min(cap.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&buf[..keep]);
        truncated |= keep < n;
    }
    Ok((bytes, truncated))
}

/// Executes argv without a shell. Callers enforce their command allowlist;
/// this layer owns deadlines, bounded collection and descendant cleanup.
pub async fn run(
    root: &Path,
    cmd: &str,
    env: &HashMap<String, String>,
    timeout: Duration,
) -> std::io::Result<ProcessOutput> {
    let mut parts = cmd.split_whitespace();
    let bin = parts
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"))?;
    let mut command = tokio::process::Command::new(bin);
    command
        .args(parts)
        .current_dir(root)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn()?;
    let _group = ProcessGroup(
        child
            .id()
            .ok_or_else(|| std::io::Error::other("child has no pid"))?,
    );
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let result = tokio::time::timeout(timeout, async {
        let (status, out, err) = tokio::try_join!(
            child.wait(),
            collect(stdout, OUTPUT_LIMIT),
            collect(stderr, OUTPUT_LIMIT)
        )?;
        Ok::<_, std::io::Error>(ProcessOutput {
            success: status.success(),
            code: status.code(),
            stdout: out.0,
            stderr: err.0,
            truncated: out.1 || err.1,
        })
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => {
            // Kill the process group before awaiting the leader's reap.
            #[cfg(unix)]
            unsafe {
                libc::kill(-(_group.0 as i32), libc::SIGKILL);
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("command timeout after {}ms", timeout.as_millis()),
            ))
        }
    }
}
