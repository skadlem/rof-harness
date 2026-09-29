//! Honesty kit: checks run before any verdict. Salvage of
//! ~/rof-harness/src/engine/session.rs (run_checks/render/checks_pass/condense).
use serde::{Deserialize, Serialize};
use std::path::Path;

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

pub struct StdRunner;

// ponytail: no timeout in v1; add deadline/kill on first real hang.
impl CommandRunner for StdRunner {
    fn run(&self, cmd: &str, args: &[String], workdir: &Path) -> std::io::Result<CheckOutput> {
        let out = std::process::Command::new(cmd)
            .args(args)
            .current_dir(workdir)
            .output()?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok(CheckOutput {
            code: out.status.code().unwrap_or(-1),
            output: text,
        })
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
                            condense_output(&o.output)
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

/// Drop build noise, keep signal substrings, cap line count.
pub fn condense_output(s: &str) -> String {
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
    let mut kept: Vec<&str> = Vec::new();
    let mut dropped = 0usize;
    for line in s.lines() {
        let t = line.trim();
        let noise = t.is_empty()
            || t.starts_with("Compiling")
            || t.starts_with("Finished")
            || t.starts_with("Running")
            || t.starts_with("Doc-tests")
            || t.starts_with("Blocking")
            || (t.starts_with("test ") && t.ends_with("... ok"));
        if noise {
            dropped += 1;
            continue;
        }
        if KEEP.iter().any(|k| t.contains(k)) {
            kept.push(line);
        } else {
            dropped += 1;
        }
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
        let out = condense_output(&raw);
        assert!(out.contains("test b ... FAILED"));
        assert!(!out.contains("test a ... ok"));
        assert!(out.contains("over cap omitted"));
        assert_eq!(
            out.lines().filter(|l| l.contains("error line")).count(),
            80 - 2
        );
        let green = condense_output("   Compiling rof v0.1.0\n    Finished dev profile\n");
        assert!(green.contains("no actionable lines"));
    }
}
