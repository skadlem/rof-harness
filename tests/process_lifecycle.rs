#![cfg(unix)]
use rof::tools::{ProcRunTool, Tool};
use std::{path::PathBuf, time::Duration};

fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!("rof-process-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

// A cancelled tool call must not leave its check or descendants writing into
// a tree that the harness has already rolled back or handed to another round.
#[tokio::test]
async fn cancelling_a_check_stops_descendant_writes() {
    let root = fixture();
    let script = root.join("check.py");
    std::fs::write(&script, r#"import subprocess, sys, time, pathlib, os
child = subprocess.Popen([sys.executable, '-c', "import time,pathlib; time.sleep(0.6); pathlib.Path('late-write').write_text('stale')"])
pathlib.Path('pids').write_text(str(os.getpid()) + ' ' + str(child.pid))
time.sleep(30)
"#).unwrap();
    let cmd = format!("python3 {}", script.display());
    let tool = ProcRunTool::new(root.clone(), vec![cmd.clone()]);
    let task = tokio::spawn(async move { tool.exec(serde_json::json!({"cmd":cmd})).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.join("pids").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    let late_write = root.join("late-write").exists();
    // Clean old buggy children too, so the red test leaves no live check.
    for pid in std::fs::read_to_string(root.join("pids"))
        .unwrap()
        .split_whitespace()
    {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", pid])
            .output();
    }
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        !late_write,
        "cancelled check left a descendant modifying the tree"
    );
}

#[tokio::test]
async fn deadline_stops_descendants_and_reports_timeout() {
    let root = fixture();
    std::fs::write(root.join("check.py"), "import subprocess,sys,time\nsubprocess.Popen([sys.executable,'-c',\"import time,pathlib; time.sleep(.5); pathlib.Path('late-write').write_text('late')\"])\ntime.sleep(30)\n").unwrap();
    let result = rof::process::run(
        &root,
        "python3 check.py",
        &Default::default(),
        Duration::from_millis(100),
    )
    .await;
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(!root.join("late-write").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn noisy_check_is_drained_with_bounded_capture() {
    let root = fixture();
    std::fs::write(
        root.join("check.py"),
        "import sys\nsys.stdout.write('x' * 1000000)\nsys.stderr.write('y' * 1000000)\n",
    )
    .unwrap();
    let result = rof::process::run(
        &root,
        "python3 check.py",
        &Default::default(),
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert!(result.success);
    assert!(result.truncated);
    assert_eq!(result.stdout.len(), rof::process::OUTPUT_LIMIT);
    assert_eq!(result.stderr.len(), rof::process::OUTPUT_LIMIT);
    std::fs::remove_dir_all(root).unwrap();
}
