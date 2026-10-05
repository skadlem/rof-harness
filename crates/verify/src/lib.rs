//! Honesty kit: checks run before any verdict. Salvage of
//! ~/rof-harness/src/engine/session.rs (run_checks/render/checks_pass/condense).
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub cmd: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckOutput {
    pub code: i32,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub passed: bool,
    pub output: String,
}

/// Execution seam: tests inject fakes, production passes the std runner.
pub trait CommandRunner: Send + Sync {
    fn run(&self, cmd: &str, args: &[String], workdir: &Path) -> std::io::Result<CheckOutput>;
}

pub struct StdRunner {
    pub deadline: Duration,
}

impl Default for StdRunner {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(300),
        }
    }
}

impl StdRunner {
    pub fn new(deadline: Duration) -> Self {
        Self { deadline }
    }
}

impl CommandRunner for StdRunner {
    fn run(&self, cmd: &str, args: &[String], workdir: &Path) -> std::io::Result<CheckOutput> {
        use std::io::Read;
        let mut child = std::process::Command::new(cmd)
            .args(args)
            .current_dir(workdir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        // Drain pipes on threads so a chatty child can't wedge the
        // try_wait poll below on a full pipe buffer.
        let out_h = std::thread::spawn({
            let mut o = child.stdout.take();
            move || {
                let mut s = String::new();
                if let Some(ref mut o) = o {
                    let _ = o.read_to_string(&mut s);
                }
                s
            }
        });
        let err_h = std::thread::spawn({
            let mut e = child.stderr.take();
            move || {
                let mut s = String::new();
                if let Some(ref mut e) = e {
                    let _ = e.read_to_string(&mut s);
                }
                s
            }
        });
        let start = std::time::Instant::now();
        loop {
            match child.try_wait()? {
                Some(status) => {
                    let mut text = out_h.join().unwrap_or_default();
                    text.push_str(&err_h.join().unwrap_or_default());
                    return Ok(CheckOutput {
                        code: status.code().unwrap_or(-1),
                        output: text,
                    });
                }
                None if start.elapsed() >= self.deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let mut text = format!(
                        "timeout after {}s: deadline exceeded",
                        self.deadline.as_secs()
                    );
                    text.push_str(&out_h.join().unwrap_or_default());
                    text.push_str(&err_h.join().unwrap_or_default());
                    return Ok(CheckOutput {
                        code: -1,
                        output: text,
                    });
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}

fn cmdline(cmd: &str, args: &[String]) -> String {
    if args.is_empty() {
        cmd.to_string()
    } else {
        format!("{cmd} {}", args.join(" "))
    }
}

/// One runner call per check, in order. Output rendered as
/// "$ cmd\nSTATUS: PASSED|FAILED (code)\n<condensed>".
pub fn run_checks(
    runner: &dyn CommandRunner,
    checks: &[Check],
    workdir: &Path,
) -> Vec<CheckResult> {
    checks
        .iter()
        .map(|c| {
            let cmd = cmdline(&c.cmd, &c.args);
            match runner.run(&c.cmd, &c.args, workdir) {
                Ok(o) => {
                    let passed = o.code == 0;
                    CheckResult {
                        name: c.name.clone(),
                        passed,
                        output: format!(
                            "$ {cmd}\nSTATUS: {} (exit {})\n{}\n",
                            if passed { "PASSED" } else { "FAILED" },
                            o.code,
                            condense_output(&o.output, None)
                        ),
                    }
                }
                Err(e) => CheckResult {
                    name: c.name.clone(),
                    passed: false,
                    output: format!("$ {cmd}\nSTATUS: FAILED ({e})\n"),
                },
            }
        })
        .collect()
}

/// All-pass. Vacuous true on empty is documented, not accidental:
/// no-oracle tasks are refused elsewhere, never here.
pub fn checks_pass(results: &[CheckResult]) -> bool {
    results.iter().all(|c| c.passed)
}

fn is_noise(t: &str) -> bool {
    t.is_empty()
        || t.starts_with("Compiling")
        || t.starts_with("Finished")
        || t.starts_with("Running")
        || t.starts_with("Doc-tests")
        || t.starts_with("Blocking")
        || (t.starts_with("test ") && t.ends_with("... ok"))
}

const TAIL_LINES: usize = 12;
const TAIL_BYTES: usize = 2000;

/// Keep-list matched nothing: a bare Python traceback, panic backtrace or
/// timeout notice carries no signal substring, only a tail. Keep the last
/// `TAIL_LINES` raw lines within `TAIL_BYTES`; a pure-noise tail (all
/// `is_noise`) keeps the old empty-result sentence so green build logs stay
/// as terse as before.
fn condense_tail(s: &str) -> String {
    let all: Vec<&str> = s.lines().collect();
    if all.is_empty() {
        return "(no actionable lines in 0 lines of output)".to_string();
    }
    let mut first = all.len();
    let mut bytes = 0usize;
    while first > 0 && all.len() - first < TAIL_LINES {
        let cost = all[first - 1].len() + 1;
        if bytes + cost > TAIL_BYTES {
            break;
        }
        bytes += cost;
        first -= 1;
    }
    let mut body = if first == all.len() {
        // Even the last line alone exceeds the budget: keep its tail bytes.
        first -= 1;
        let line = all[first];
        let mut from = line.len().saturating_sub(TAIL_BYTES);
        while !line.is_char_boundary(from) {
            from += 1;
        }
        line[from..].to_string()
    } else {
        all[first..].join("\n")
    };
    if !all[first..].iter().any(|l| !is_noise(l.trim())) {
        return format!("(no actionable lines in {} lines of output)", all.len());
    }
    if first > 0 {
        body.push_str(&format!("\n[...{first} earlier lines omitted]"));
    }
    body
}

/// Drop build noise, keep signal substrings, cap line count. With a
/// baseline (`Some` pre-existing output), kept lines already present in
/// the baseline are dropped as noise so only new signal surfaces. When
/// the keep-list matches nothing at all, [`condense_tail`] returns the raw
/// tail instead of the empty-result sentence; baseline suppression alone
/// keeps that sentence.
pub fn condense_output(s: &str, baseline: Option<&str>) -> String {
    const KEEP: [&str; 9] = [
        "FAILED",
        "error",
        "panicked",
        "assertion",
        "warning",
        "test result",
        "failures:",
        "left:",
        "right:",
    ];
    let base: Option<HashSet<&str>> = baseline.map(|b| b.lines().collect());
    let mut kept: Vec<&str> = Vec::new();
    let mut matched = 0usize;
    let mut dropped = 0usize;
    for line in s.lines() {
        let t = line.trim();
        if is_noise(t) {
            dropped += 1;
            continue;
        }
        if KEEP.iter().any(|k| t.contains(k)) {
            matched += 1;
            if base.as_ref().is_some_and(|bs| bs.contains(line)) {
                dropped += 1;
                continue;
            }
            kept.push(line);
        } else {
            dropped += 1;
        }
    }
    if kept.is_empty() && matched == 0 {
        return condense_tail(s);
    }
    if kept.is_empty() {
        return format!(
            "(no actionable lines in {} lines of output)",
            s.lines().count()
        );
    }
    let omitted = kept.len().saturating_sub(80);
    let body = kept[..kept.len().min(80)].join("\n");
    if omitted > 0 || dropped > 0 {
        format!("{body}\n[...{dropped} noise lines and {omitted} kept-lines over cap omitted]")
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct FakeRunner(HashMap<String, Result<CheckOutput, String>>);

    impl CommandRunner for FakeRunner {
        fn run(
            &self,
            cmd: &str,
            _args: &[String],
            _workdir: &Path,
        ) -> std::io::Result<CheckOutput> {
            match self.0.get(cmd) {
                Some(Ok(o)) => Ok(o.clone()),
                Some(Err(e)) => Err(std::io::Error::other(e.clone())),
                None => panic!("unexpected cmd {cmd}"),
            }
        }
    }

    fn check(name: &str, cmd: &str) -> Check {
        Check {
            name: name.into(),
            cmd: cmd.into(),
            args: vec![],
        }
    }

    #[test]
    fn pass_renders_status_and_condensed_body() {
        let r = FakeRunner(HashMap::from([(
            "ok".into(),
            Ok(CheckOutput {
                code: 0,
                output: "Compiling foo\ntest result: ok. 1 passed\n".into(),
            }),
        )]));
        let out = run_checks(&r, &[check("n", "ok")], Path::new("/tmp"));
        assert!(out[0].passed);
        assert!(out[0].output.starts_with("$ ok\nSTATUS: PASSED (exit 0)\n"));
        assert!(out[0].output.contains("test result: ok"));
        assert!(!out[0].output.contains("Compiling"));
    }

    #[test]
    fn failing_check_carries_output() {
        let r = FakeRunner(HashMap::from([(
            "red".into(),
            Ok(CheckOutput {
                code: 1,
                output: "test b ... FAILED\nassertion failed\nleft: 1\nright: 2\n".into(),
            }),
        )]));
        let out = run_checks(&r, &[check("n", "red")], Path::new("/tmp"));
        assert!(!out[0].passed);
        assert!(out[0].output.contains("STATUS: FAILED (exit 1)"));
        assert!(out[0].output.contains("left: 1"));
        assert!(!checks_pass(&out));
    }

    #[test]
    fn runner_error_is_failed_with_reason() {
        let r = FakeRunner(HashMap::from([("x".into(), Err("boom".into()))]));
        let out = run_checks(&r, &[check("n", "x")], Path::new("/tmp"));
        assert!(!out[0].passed);
        assert!(out[0].output.contains("STATUS: FAILED (boom)"));
    }

    #[test]
    fn vacuous_true_pinned() {
        assert!(checks_pass(&[]));
    }

    #[test]
    fn checks_pass_reads_fields_not_log_text() {
        let ok = CheckResult {
            name: "a".into(),
            passed: true,
            output: "STATUS: FAILED in a quoted message".into(),
        };
        let bad = CheckResult {
            name: "b".into(),
            passed: false,
            output: String::new(),
        };
        assert!(checks_pass(std::slice::from_ref(&ok)));
        assert!(!checks_pass(&[ok, bad]));
    }

    #[test]
    fn condense_keeps_signal_drops_noise_and_caps() {
        let mut raw = String::from("test a ... ok\ntest b ... FAILED\ntest result: FAILED\n");
        for i in 0..100 {
            raw.push_str(&format!("error line {i}\n"));
        }
        let out = condense_output(&raw, None);
        assert!(out.contains("test b ... FAILED"));
        assert!(!out.contains("test a ... ok"));
        assert!(out.contains("over cap omitted"));
        assert_eq!(
            out.lines().filter(|l| l.contains("error line")).count(),
            80 - 2
        );
        let green = condense_output("   Compiling rof v0.1.0\n    Finished dev profile\n", None);
        assert!(green.contains("no actionable lines"));
    }

    #[test]
    fn condense_baseline_reports_only_new_lines() {
        let raw = "error old\nerror new\ntest result: FAILED\n";
        let without = condense_output(raw, None);
        assert!(without.contains("error old"));
        assert!(without.contains("error new"));
        let with = condense_output(raw, Some("error old\n"));
        assert!(!with.contains("error old"), "{with}");
        assert!(with.contains("error new"), "{with}");
        let all_old = condense_output("error old\n", Some("error old\n"));
        assert!(all_old.contains("no actionable lines"), "{all_old}");
    }

    #[test]
    fn condense_keeps_bare_traceback_tail() {
        // No KEEP substring anywhere ("ValueError" is not "error"): the only
        // actionable line sits past the keep-list's reach, at the end.
        let mut raw = String::from("Traceback (most recent call last):\n");
        for i in 0..40 {
            raw.push_str(&format!("  File frame_{i}.rs, in <module>\n"));
        }
        raw.push_str("ValueError: boom\n");
        let out = condense_output(&raw, None);
        assert!(out.contains("ValueError: boom"), "{out}");
        assert!(out.contains("earlier lines omitted"), "{out}");
        assert!(!out.contains("no actionable lines"), "{out}");
        assert!(out.lines().count() <= TAIL_LINES + 1, "{out}");
        // The byte budget holds even for one huge last line.
        let huge = format!("Traceback:\n{}\n", "z".repeat(10_000));
        let cut = condense_output(&huge, None);
        assert_eq!(cut.lines().next().unwrap().len(), TAIL_BYTES, "{cut}");
        // Pure-noise tail keeps the terse sentence (green build logs).
        let green = condense_output("   Compiling a\n    Finished dev\n", None);
        assert!(green.contains("no actionable lines"), "{green}");
    }

    #[test]
    fn condense_unchanged_when_keep_list_matches() {
        // Non-empty keep-list: byte-identical to the pre-fallback rendering.
        let raw = "   Compiling x\nerror: E0308\nfiller a\nfiller b\n";
        assert_eq!(
            condense_output(raw, None),
            "error: E0308\n[...3 noise lines and 0 kept-lines over cap omitted]"
        );
        assert_eq!(
            condense_output(raw, Some("error: E0308\n")),
            "(no actionable lines in 4 lines of output)"
        );
    }

    #[test]
    fn std_runner_deadline_kills_sleep() {
        let runner = StdRunner::new(Duration::from_millis(300));
        let start = std::time::Instant::now();
        let out = runner
            .run("sleep", &["10".to_string()], Path::new("/tmp"))
            .unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_ne!(out.code, 0);
        assert!(out.output.contains("deadline"), "{}", out.output);
        assert_eq!(StdRunner::default().deadline, Duration::from_secs(300));
    }
}
