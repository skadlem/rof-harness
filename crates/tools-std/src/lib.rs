//! The 6 built-in tools + containment. Implements [`tool_core::Tool`].
//!
//! Schemas are hand-built `serde_json` values kept inside tool-core's strict
//! subset (`required`, `type`, `additionalProperties: false`, `maxLength`);
//! each tool also parses into a typed `Deserialize` args struct with
//! `deny_unknown_fields`, so schema and parser pin the same contract.
//! (ponytail: no `schemars` dep — five `json!` literals use the already-installed
//! `serde_json`.)
//!
//! ## Threat model (operator read-this)
//!
//! The path policy covers `view`/`edit`/`write` only: root-anchored,
//! symlink-safe, secrets-denied file access. `exec` and `test` run
//! allowlisted host commands with a cleared environment, null stdin, and a
//! bounded pipe read — but allowlisting is not isolation. Permitting
//! `cargo test`, `sh`, or any test runner hands the agent arbitrary code
//! execution on the host: a hostile command can still reach the network,
//! IPC, or sibling processes, and no output bound or timeout changes that.
//! OS-level isolation (container/namespace, no network, read-only mounts)
//! is the operator's job; this crate builds no sandbox.

mod common;
mod condense;
mod edit;
mod exec;
mod policy;
mod runner;
mod search;
mod test_tool;
mod view;
mod write;

pub use common::{apply_hunk, prefix_allowed};
pub use edit::{edit_tool, EditTool};
pub use exec::{exec_tool, ExecTool};
pub use policy::{default_denied_globs, glob_match, resolve_under, Policy, ToolPathError};
pub use search::{search_tool, SearchTool};
pub use test_tool::{test_tool, TestTool};
pub use view::{view_tool, ViewTool};
pub use write::{write_tool, WriteTool};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{EDIT_FILE_CAP, EDIT_REPLACE_CAP, EXEC_TIMEOUT, OUT_CAP, VIEW_CAP};
    use crate::runner::{run_allowed, split_cmd, stage_and_check};
    use crate::view::{view_page, view_read_cap};
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;
    use tool_core::{CallStatus, GrantGate, Registry, ToolCall, ToolOutcome};

    static N: AtomicUsize = AtomicUsize::new(0);

    fn tmp_root() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "tools-std-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn policy(root: &Path) -> Arc<Policy> {
        Arc::new(Policy {
            root: root.to_path_buf(),
            allowed_commands: vec!["true".to_string(), "false".to_string()],
            allowed_prefixes: vec!["echo".to_string(), "cargo test".to_string()],
            syntax_cmd: None,
            denied_globs: default_denied_globs(),
            pass_env: Vec::new(),
        })
    }

    fn syntax_policy(root: &Path, argv: &[&str]) -> Arc<Policy> {
        // The checker runs as `argv + tempfile` through `run_allowed`, so
        // the checker binary must be prefix-allowlisted.
        let prefix = argv.join(" ");
        Arc::new(Policy {
            root: root.to_path_buf(),
            allowed_commands: vec!["true".to_string(), "false".to_string()],
            allowed_prefixes: vec!["echo".to_string(), prefix],
            syntax_cmd: Some(argv.iter().map(|s| s.to_string()).collect()),
            denied_globs: default_denied_globs(),
            pass_env: Vec::new(),
        })
    }

    fn reg(p: Arc<Policy>) -> Registry {
        let mut r = Registry::new(Arc::new(GrantGate::new(
            [(
                "agent".to_string(),
                vec!["view", "search", "edit", "write", "exec", "test"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            )]
            .into_iter()
            .collect::<HashMap<_, _>>(),
        )));
        r.register(Arc::new(view_tool(p.clone())));
        r.register(Arc::new(search_tool(p.clone())));
        r.register(Arc::new(edit_tool(p.clone())));
        r.register(Arc::new(write_tool(p.clone())));
        r.register(Arc::new(exec_tool(p.clone())));
        r.register(Arc::new(test_tool(p)));
        r
    }

    fn call(id: &str, name: &str, args: Value) -> ToolCall {
        ToolCall {
            call_id: id.into(),
            name: name.into(),
            args,
        }
    }

    async fn run(r: &Registry, name: &str, args: Value) -> Result<ToolOutcome, String> {
        let status = r.prepare("agent", call("c1", name, args));
        let inv = match status {
            CallStatus::Dispatch(inv) => inv,
            CallStatus::Result(res) => return Err(res.content),
        };
        let tool = r.resolve(name).unwrap();
        tool.execute(inv, CancellationToken::new())
            .await
            .map_err(|e| e.to_string())
    }

    // --- containment matrix ---

    #[test]
    fn git_write_denied_read_allowed() {
        let root = tmp_root();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        for rel in [".git/config", "a/.git/x", "a/.git", ".git"] {
            match resolve_under(&root, rel, true, &[]) {
                Err(ToolPathError::Denied(_)) => {}
                other => panic!("{rel} write must be denied, got {other:?}"),
            }
        }
        // Reads may traverse .git (still root-anchored + symlink-safe).
        assert!(resolve_under(&root, ".git/HEAD", false, &[]).is_ok());
    }

    #[test]
    fn git_symlink_alias_write_denied_read_allowed() {
        let root = tmp_root();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), "x\n").unwrap();
        // A symlinked dir pointing at the substrate must not launder writes.
        std::os::unix::fs::symlink(root.join(".git"), root.join("evil")).unwrap();
        for rel in ["evil/config", "evil/new", "evil"] {
            match resolve_under(&root, rel, true, &[]) {
                Err(ToolPathError::Denied(_)) => {}
                other => panic!("{rel} write must be denied, got {other:?}"),
            }
        }
        // Reads through the alias stay allowed (same rule as plain `.git`).
        assert!(resolve_under(&root, "evil/config", false, &[]).is_ok());
        // A final-component link straight at a substrate file is denied too.
        std::os::unix::fs::symlink(root.join(".git/config"), root.join("head-link")).unwrap();
        match resolve_under(&root, "head-link", true, &[]) {
            Err(ToolPathError::Denied(_)) => {}
            other => panic!("head-link write must be denied, got {other:?}"),
        }
    }

    #[test]
    fn secrets_globs_cover_keys_and_cloud_dirs() {
        for (pat, hit, miss) in [
            ("**/.env", ".env", ".env.example"),
            ("**/.env", "a/.env", "a/.envx"),
            ("**/.env.*", ".env.local", ".env"),
            ("**/.env.*", "a/.env.local", "a/env.local"),
            ("**/*.pem", "k.pem", "k.pemx"),
            ("**/*.pem", "a/b/k.pem", "a/b/pem"),
            ("**/*.key", "a/tls.key", "a/key"),
            ("**/id_rsa*", "id_rsa", "x_id_rsa"),
            ("**/id_rsa*", "a/id_rsa.pub", "a/id_rsa_pub/x"),
            ("**/.npmrc", ".npmrc", "npmrc"),
            ("**/.npmrc", "a/.npmrc", "a/.npmrcx"),
            ("**/.netrc", "sub/.netrc", "sub/netrc"),
            ("**/.aws", ".aws", ".awsx"),
            ("**/.aws", "a/.aws", "a/aws"),
            ("**/.aws/**", "a/.aws/config", "a/aws/config"),
            ("**/.ssh", "a/.ssh", "a/ssh"),
            ("**/.ssh/**", ".ssh/id_rsa", ".sshx/id_rsa"),
        ] {
            assert!(glob_match(pat, hit), "{pat} must match {hit}");
            assert!(!glob_match(pat, miss), "{pat} must not match {miss}");
        }
    }

    #[test]
    fn env_template_allowed_but_local_and_keys_denied() {
        let root = tmp_root();
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let globs = default_denied_globs();
        for rel in [
            ".env",
            ".env.local",
            "a/.env",
            "a/.env.production",
            "k.pem",
            "a/tls.key",
            "id_rsa",
            "a/id_rsa.pub",
            ".npmrc",
            "sub/.netrc",
            ".aws/config",
            "a/.aws/credentials",
            ".ssh/id_rsa",
        ] {
            for write in [true, false] {
                match resolve_under(&root, rel, write, &globs) {
                    Err(ToolPathError::Denied(_)) => {}
                    other => panic!("{rel} must be denied, got {other:?}"),
                }
            }
        }
        // Templates stay usable on both reads and writes.
        for rel in [".env.example", ".env.sample", "a/.env.template"] {
            assert!(
                resolve_under(&root, rel, false, &globs).is_ok(),
                "{rel} readable"
            );
            assert!(
                resolve_under(&root, rel, true, &globs).is_ok(),
                "{rel} writable"
            );
        }
    }

    #[test]
    fn escape_denied() {
        let root = tmp_root();
        for rel in ["../evil", "/etc/passwd", "a/../../evil"] {
            for write in [true, false] {
                assert!(
                    matches!(
                        resolve_under(&root, rel, write, &[]),
                        Err(ToolPathError::Denied(_))
                    ),
                    "{rel} must be denied"
                );
            }
        }
    }

    #[test]
    fn symlink_escape_denied() {
        let root = tmp_root();
        let outside = tmp_root();
        std::fs::write(outside.join("secret"), "x").unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), root.join("link")).unwrap();
        assert!(matches!(
            resolve_under(&root, "link", true, &[]),
            Err(ToolPathError::Denied(_))
        ));
        std::os::unix::fs::symlink(&outside, root.join("dirlink")).unwrap();
        std::fs::create_dir_all(root.join("real")).unwrap();
        assert!(matches!(
            resolve_under(&root, "dirlink/secret", true, &[]),
            Err(ToolPathError::Denied(_))
        ));
    }

    #[test]
    fn missing_parent_is_recoverable_never_denial() {
        let root = tmp_root();
        match resolve_under(&root, "no/such/file.txt", true, &[]) {
            Err(ToolPathError::MissingParent(m)) => assert!(m.contains("create"), "{m}"),
            other => panic!("must be MissingParent, got {other:?}"),
        }
    }

    // --- patch ---

    #[test]
    fn hunk_exact_once() {
        assert_eq!(
            apply_hunk("a\nfoo\nb\n", "foo\n", "bar\n").unwrap(),
            "a\nbar\nb\n"
        );
    }

    #[test]
    fn hunk_whitespace_tolerant() {
        assert_eq!(
            apply_hunk("fn f() {\n    x  =  1;\n}\n", "x = 1;", "x = 2;").unwrap(),
            "fn f() {\n    x = 2;\n}\n"
        );
    }

    #[test]
    fn hunk_ambiguous_fails() {
        assert!(apply_hunk("foo\nfoo\n", "foo\n", "bar\n").is_err());
        assert!(apply_hunk("a  b\nc\na   b\n", "a b", "z").is_err());
        assert!(apply_hunk("x\n", "nope", "z").is_err());
        assert!(apply_hunk("x\n", "", "z").is_err());
    }

    #[tokio::test]
    async fn edit_roundtrip_via_registry() {
        let root = tmp_root();
        std::fs::write(root.join("f.txt"), "hello\n").unwrap();
        let r = reg(policy(&root));
        let out = run(
            &r,
            "edit",
            json!({"path": "f.txt", "search": "hello", "replace": "bye"}),
        )
        .await
        .unwrap();
        assert!(out.content.contains("patched"));
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "bye\n"
        );
        assert!(
            run(&r, "view", json!({"path": "f.txt"}))
                .await
                .unwrap()
                .content
                == "bye\n"
        );
    }

    // --- write ---

    #[tokio::test]
    async fn write_creates_new_with_parents_then_overwrites() {
        let root = tmp_root();
        let r = reg(policy(&root));
        let out = run(
            &r,
            "write",
            json!({"path": "a/b/new.txt", "content": "first\n"}),
        )
        .await
        .unwrap();
        assert!(out.content.contains("wrote a/b/new.txt"), "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(root.join("a/b/new.txt")).unwrap(),
            "first\n"
        );
        // Overwrite is the whole point: no search hunk, no read-modify-write.
        run(
            &r,
            "write",
            json!({"path": "a/b/new.txt", "content": "second\n"}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("a/b/new.txt")).unwrap(),
            "second\n"
        );
    }

    #[tokio::test]
    async fn write_jail_escape_and_glob_denied() {
        let root = tmp_root();
        let outside = tmp_root();
        let r = reg(policy(&root));
        for rel in ["../evil.txt", "/etc/passwd"] {
            let err = run(&r, "write", json!({"path": rel, "content": "x"}))
                .await
                .unwrap_err();
            assert!(err.contains("denied"), "{rel}: {err}");
        }
        let err = run(&r, "write", json!({"path": ".env", "content": "SECRET=1"}))
            .await
            .unwrap_err();
        assert!(err.contains("denied"), "{err}");
        assert!(!outside.join("evil.txt").exists());
        assert!(!root.join(".env").exists());
        // A symlinked dir must not be used as a mkdir tunnel out of the root.
        std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
        let err = run(
            &r,
            "write",
            json!({"path": "out/tunneled.txt", "content": "x"}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("denied"), "{err}");
        assert!(!outside.join("tunneled.txt").exists());
    }

    #[tokio::test]
    async fn write_refuses_directory() {
        let root = tmp_root();
        std::fs::create_dir_all(root.join("d")).unwrap();
        let r = reg(policy(&root));
        let err = run(&r, "write", json!({"path": "d", "content": "x"}))
            .await
            .unwrap_err();
        assert!(err.contains("directory"), "{err}");
    }

    #[tokio::test]
    async fn write_caps_enforced() {
        let root = tmp_root();
        let r = reg(policy(&root));
        let big = "x".repeat(EDIT_REPLACE_CAP + 1);
        let err = run(&r, "write", json!({"path": "big.txt", "content": big}))
            .await
            .unwrap_err();
        assert!(err.contains("256KB cap"), "{err}");
        assert!(!root.join("big.txt").exists());

        std::fs::write(root.join("huge.txt"), "y".repeat(EDIT_FILE_CAP + 1)).unwrap();
        let err = run(
            &r,
            "write",
            json!({"path": "huge.txt", "content": "small\n"}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("512KB cap"), "{err}");
        assert_eq!(
            std::fs::read_to_string(root.join("huge.txt"))
                .unwrap()
                .len(),
            EDIT_FILE_CAP + 1
        );
    }

    #[tokio::test]
    async fn write_syntax_veto_leaves_file_untouched() {
        let root = tmp_root();
        std::fs::write(root.join("f.txt"), "hello\n").unwrap();
        let r = reg(syntax_policy(&root, &["false"]));
        let err = run(&r, "write", json!({"path": "f.txt", "content": "bye\n"}))
            .await
            .unwrap_err();
        assert!(err.contains("syntax check failed"), "{err}");
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "hello\n"
        );
    }

    // --- exec ---

    #[tokio::test]
    async fn edit_syntax_veto_leaves_file_untouched() {
        let root = tmp_root();
        std::fs::write(root.join("f.txt"), "hello\n").unwrap();
        let r = reg(syntax_policy(&root, &["false"]));
        let err = run(
            &r,
            "edit",
            json!({"path": "f.txt", "search": "hello", "replace": "bye"}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("syntax check failed"), "{err}");
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "hello\n"
        );
    }

    #[tokio::test]
    async fn edit_syntax_pass_on_true_writes() {
        let root = tmp_root();
        std::fs::write(root.join("f.txt"), "hello\n").unwrap();
        let r = reg(syntax_policy(&root, &["true"]));
        let out = run(
            &r,
            "edit",
            json!({"path": "f.txt", "search": "hello", "replace": "bye"}),
        )
        .await
        .unwrap();
        assert!(out.content.contains("patched"));
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "bye\n"
        );
    }

    #[tokio::test]
    async fn edit_unconfigured_syntax_skips_with_zero_behavior_change() {
        let root = tmp_root();
        std::fs::write(root.join("f.txt"), "hello\n").unwrap();
        assert!(policy(&root).syntax_cmd.is_none());
        let r = reg(policy(&root));
        run(
            &r,
            "edit",
            json!({"path": "f.txt", "search": "hello", "replace": "bye"}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "bye\n"
        );
    }

    #[test]
    fn denied_glob_blocks_env_allows_example() {
        assert!(glob_match("**/*.env", ".env"));
        assert!(glob_match("**/*.env", "a/.env"));
        assert!(!glob_match("**/*.env", ".env.example"));
        assert!(!glob_match("**/*.env", "a/.env.example"));
        let root = tmp_root();
        let globs = default_denied_globs();
        for rel in [".env", "a/.env"] {
            match resolve_under(&root, rel, false, &globs) {
                Err(ToolPathError::Denied(_)) => {}
                other => panic!("{rel} must be denied, got {other:?}"),
            }
            match resolve_under(&root, rel, true, &globs) {
                Err(ToolPathError::Denied(_)) => {}
                other => panic!("{rel} write must be denied, got {other:?}"),
            }
        }
        assert!(resolve_under(&root, ".env.example", false, &globs).is_ok());
    }

    #[tokio::test]
    async fn git_view_allowed_write_denied_via_registry() {
        let root = tmp_root();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(root.join("f.txt"), "hello\n").unwrap();
        let r = reg(policy(&root));
        let out = run(&r, "view", json!({"path": ".git/HEAD"})).await.unwrap();
        assert!(out.content.contains("ref:"), "{}", out.content);
        let err = run(
            &r,
            "edit",
            json!({"path": ".git/HEAD", "search": "ref:", "replace": "x"}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("denied"), "{err}");
    }

    #[tokio::test]
    async fn view_max_bytes_beyond_view_cap_still_truncates() {
        let root = tmp_root();
        std::fs::write(root.join("big.txt"), "a".repeat(VIEW_CAP + 100)).unwrap();
        let r = reg(policy(&root));
        let out = run(
            &r,
            "view",
            json!({"path": "big.txt", "max_bytes": 10_000_000u64}),
        )
        .await
        .unwrap();
        assert!(out.truncated);
        assert!(out.content.chars().count() <= OUT_CAP);
        // Honesty bound: max_bytes narrows the read, never widens it.
        assert_eq!(view_read_cap(None), VIEW_CAP);
        assert_eq!(view_read_cap(Some(u64::MAX)), VIEW_CAP);
        assert_eq!(view_read_cap(Some(VIEW_CAP as u64 + 1)), VIEW_CAP);
        assert_eq!(view_read_cap(Some(42)), 42);
    }

    #[tokio::test]
    async fn view_no_offset_path_unchanged() {
        let root = tmp_root();
        std::fs::write(root.join("f.txt"), "alpha\nbeta\n").unwrap();
        std::fs::write(root.join("big.txt"), "a".repeat(VIEW_CAP + 100)).unwrap();
        let r = reg(policy(&root));
        // Small file: exact bytes, no notice, not truncated.
        let whole = run(&r, "view", json!({"path": "f.txt"})).await.unwrap();
        assert_eq!(whole.content, "alpha\nbeta\n");
        assert!(!whole.truncated);
        // max_bytes is still a pure byte narrowing.
        let capped = run(&r, "view", json!({"path": "f.txt", "max_bytes": 3}))
            .await
            .unwrap();
        assert_eq!(capped.content, "alp");
        assert!(capped.truncated);
        // Over-cap file: first OUT_CAP chars, still no notice.
        let big = run(&r, "view", json!({"path": "big.txt"})).await.unwrap();
        assert_eq!(big.content, "a".repeat(OUT_CAP));
        assert!(big.truncated);
    }

    #[tokio::test]
    async fn view_offset_pages_and_names_next_offset() {
        let root = tmp_root();
        let text: String = (1..=100).map(|i| format!("line {i:03}\n")).collect();
        std::fs::write(root.join("f.txt"), &text).unwrap();
        let many: String = (1..=1000).map(|i| format!("line {i:04}\n")).collect();
        std::fs::write(root.join("many.txt"), &many).unwrap();
        let r = reg(policy(&root));
        // max_bytes 40 = four 9-byte lines; the fifth does not fit.
        let p1 = run(
            &r,
            "view",
            json!({"path": "f.txt", "offset": 1, "max_bytes": 40}),
        )
        .await
        .unwrap();
        assert_eq!(
            p1.content,
            "line 001\nline 002\nline 003\nline 004\n[Showing lines 1-4 of 100. Use offset=5 to continue.]"
        );
        assert!(p1.truncated);
        // The named offset resumes exactly where the previous page stopped.
        let p2 = run(
            &r,
            "view",
            json!({"path": "f.txt", "offset": 5, "max_bytes": 40}),
        )
        .await
        .unwrap();
        assert_eq!(
            p2.content,
            "line 005\nline 006\nline 007\nline 008\n[Showing lines 5-8 of 100. Use offset=9 to continue.]"
        );
        // OUT_CAP chars is the other half of the page bound (no max_bytes):
        // 7999 chars = 800 nine-char rows plus separators, and the notice
        // counts from there.
        let many_page = run(&r, "view", json!({"path": "many.txt", "offset": 1}))
            .await
            .unwrap();
        assert!(
            many_page
                .content
                .contains("[Showing lines 1-800 of 1000. Use offset=801 to continue.]"),
            "{}",
            &many_page.content[many_page.content.len().saturating_sub(120)..]
        );
        assert!(many_page.content.chars().count() <= OUT_CAP + 80);
        // Tail page: everything from the offset fits, no notice.
        let tail = run(&r, "view", json!({"path": "f.txt", "offset": 99}))
            .await
            .unwrap();
        assert_eq!(tail.content, "line 099\nline 100");
        assert!(!tail.truncated);
        // Out-of-range offset fails loudly and names the line count.
        let err = run(&r, "view", json!({"path": "f.txt", "offset": 101}))
            .await
            .unwrap_err();
        assert!(err.contains("beyond end of file"), "{err}");
        // Optional with a default: absent dispatches (checked above), and the
        // argument is never coerced from a string.
        assert!(run(&r, "view", json!({"path": "f.txt", "offset": "5"}))
            .await
            .is_err());
        assert!(run(&r, "view", json!({"path": "f.txt", "offset": -1}))
            .await
            .is_err());
    }

    #[test]
    fn view_page_cut_first_line_flags_truncation() {
        let long = "x".repeat(VIEW_CAP);
        let all = vec![long.as_str(), "b"];
        let (content, truncated) = view_page(&all, 1, VIEW_CAP).unwrap();
        assert!(content.starts_with(&"x".repeat(OUT_CAP)), "{content}");
        assert!(
            content.contains("exceeds the read cap and was truncated. Use offset=2 to continue."),
            "{content}"
        );
        assert!(truncated);
    }

    #[test]
    fn prefix_boundary() {
        let p = ["cargo test".to_string()];
        assert!(prefix_allowed(&p, "cargo test"));
        assert!(prefix_allowed(&p, "cargo test foo"));
        assert!(!prefix_allowed(&p, "cargo test-evil"));
        assert!(!prefix_allowed(&p, "cargo testx"));
        assert!(!prefix_allowed(&p, ""));
        assert!(!prefix_allowed(&[" ".to_string()], "cargo test"));
    }

    #[tokio::test]
    async fn exec_deny_unlisted() {
        let root = tmp_root();
        let r = reg(policy(&root));
        let err = run(&r, "exec", json!({"cmd": "rm -rf /"}))
            .await
            .unwrap_err();
        assert!(err.contains("denied"), "{err}");
        let err = run(&r, "exec", json!({"cmd": "cargo test-evil"}))
            .await
            .unwrap_err();
        assert!(err.contains("denied"), "{err}");
    }

    #[tokio::test]
    async fn exec_no_shell_proof() {
        // Under a shell `echo hello; echo PWNED error` prints two lines; with
        // argv exec the `;` is a literal argument to echo. The `error` word
        // keeps the echoed line in the condensed tool output.
        let root = tmp_root();
        let r = reg(policy(&root));
        let out = run(&r, "exec", json!({"cmd": "echo hello; echo PWNED error"}))
            .await
            .unwrap();
        assert_eq!(out.content, "hello; echo PWNED error");
    }

    #[tokio::test]
    async fn exec_over_cap_output_marks_truncated() {
        // Raw stdout above OUT_CAP chars: cap_chars really cuts bytes, so the
        // outcome must flag it (agent-loop renders the `[truncated]` marker).
        let root = tmp_root();
        let mut pol = (*policy(&root)).clone();
        pol.allowed_prefixes.push("cat".to_string());
        let r = reg(Arc::new(pol));
        let line = format!("error: {} x\n", "a".repeat(120));
        std::fs::write(root.join("big.txt"), line.repeat(100)).unwrap();
        let out = run(&r, "exec", json!({"cmd": "cat big.txt"}))
            .await
            .unwrap();
        assert!(out.truncated, "cap-cut exec must be flagged");
        assert_eq!(out.content.chars().count(), OUT_CAP);
    }

    #[tokio::test]
    async fn exec_condensed_under_cap_not_truncated() {
        // >80 matched lines is condensation (kept-lines omitted), not a cut:
        // no bytes were discarded by OUT_CAP, so truncated must stay false.
        let root = tmp_root();
        let mut pol = (*policy(&root)).clone();
        pol.allowed_prefixes.push("cat".to_string());
        let r = reg(Arc::new(pol));
        let line = format!("error: {}\n", "a".repeat(48));
        std::fs::write(root.join("many.txt"), line.repeat(100)).unwrap();
        let out = run(&r, "exec", json!({"cmd": "cat many.txt"}))
            .await
            .unwrap();
        assert!(
            out.content.contains("kept-lines over cap omitted"),
            "{}",
            out.content
        );
        assert!(!out.truncated, "condensed output is not a truncation");
    }

    #[tokio::test]
    async fn exec_timeout_kills_child() {
        // Child would touch the marker at t=1s; the 100ms timeout must kill
        // it first (kill_on_drop) or the timed-out exec keeps mutating root.
        let root = tmp_root();
        let pol = Policy {
            root: root.clone(),
            allowed_commands: vec![],
            allowed_prefixes: vec!["sh".to_string()],
            syntax_cmd: None,
            denied_globs: default_denied_globs(),
            pass_env: Vec::new(),
        };
        let err = run_allowed(
            &pol,
            "sh -c 'sleep 1; touch late-marker'",
            Duration::from_millis(100),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("timeout"), "{err}");
        std::thread::sleep(Duration::from_millis(1100));
        assert!(
            !root.join("late-marker").exists(),
            "child survived the timeout"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_children_run_niced() {
        // Batch courtesy: agent-spawned host children take nice 19 at
        // spawn (and descendants inherit), measured via the child's own
        // view of its niceness.
        let root = tmp_root();
        let pol = Policy {
            root: root.clone(),
            allowed_commands: vec![],
            allowed_prefixes: vec!["python3".to_string()],
            syntax_cmd: None,
            denied_globs: default_denied_globs(),
            pass_env: Vec::new(),
        };
        let (ok, out, _) = run_allowed(
            &pol,
            "python3 -c \"import os; print(os.nice(0))\"",
            EXEC_TIMEOUT,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(ok, "{out}");
        assert!(out.contains("19"), "child runs niced: {out}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn exec_timeout_kills_grandchild() {
        // Middle `sh` backgrounds a grandchild `sh` and waits: killing only
        // the direct child would orphan the grandchild, which touches the
        // marker a second later. The process-group kill must take both.
        let root = tmp_root();
        let pol = Policy {
            root: root.clone(),
            allowed_commands: vec![],
            allowed_prefixes: vec!["sh".to_string()],
            syntax_cmd: None,
            denied_globs: default_denied_globs(),
            pass_env: Vec::new(),
        };
        let err = run_allowed(
            &pol,
            "sh -c 'sh -c \"sleep 1; touch grandchild-marker\" & wait'",
            Duration::from_millis(200),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("timeout"), "{err}");
        std::thread::sleep(Duration::from_millis(1500));
        assert!(
            !root.join("grandchild-marker").exists(),
            "grandchild survived the timeout"
        );
    }

    #[tokio::test]
    async fn exec_cancel_kills_group_promptly() {
        let root = tmp_root();
        let pol = Policy {
            root: root.clone(),
            allowed_commands: vec![],
            allowed_prefixes: vec!["sh".to_string()],
            syntax_cmd: None,
            denied_globs: default_denied_globs(),
            pass_env: Vec::new(),
        };
        let cancel = CancellationToken::new();
        let killer = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            killer.cancel();
        });
        let start = std::time::Instant::now();
        let err = run_allowed(
            &pol,
            "sh -c 'sh -c \"sleep 1; touch cancel-marker\" & wait'",
            Duration::from_secs(60),
            &cancel,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("cancel"), "{err}");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "cancel must return promptly"
        );
        std::thread::sleep(Duration::from_millis(1500));
        assert!(
            !root.join("cancel-marker").exists(),
            "grandchild survived the cancel"
        );
    }

    #[tokio::test]
    async fn exec_output_flood_stays_bounded() {
        // 20MB of NUL bytes: retained pipes stay at 128KB per stream while
        // the verdict, condensing, and outer cap behave as before.
        let root = tmp_root();
        let pol = Policy {
            root: root.clone(),
            allowed_commands: vec![],
            allowed_prefixes: vec!["head".to_string(), "cat".to_string()],
            syntax_cmd: None,
            denied_globs: default_denied_globs(),
            pass_env: Vec::new(),
        };
        let start = std::time::Instant::now();
        let (ok, content, truncated) = run_allowed(
            &pol,
            "head -c 20000000 /dev/zero",
            Duration::from_secs(60),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(ok);
        assert!(truncated, "a 20MB flood must flag truncation");
        assert!(content.chars().count() <= OUT_CAP);
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "flood must return promptly"
        );
        // Null stdin: `cat` with no args sees EOF and exits at once instead
        // of blocking on an inherited terminal.
        let (ok, _, truncated) = run_allowed(
            &pol,
            "cat",
            Duration::from_secs(10),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(ok);
        assert!(!truncated);
    }

    #[tokio::test]
    async fn exec_env_hides_provider_key_forwards_pass_env() {
        std::env::set_var("TOOLS_STD_TEST_PING", "pong");
        std::env::set_var("OPENAI_API_KEY", "secret-should-not-leak");
        let root = tmp_root();
        let mut pol = (*policy(&root)).clone();
        pol.allowed_prefixes.push("printenv".to_string());
        pol.pass_env = vec![
            "TOOLS_STD_TEST_PING".to_string(),
            "OPENAI_API_KEY".to_string(),
        ];
        let r = reg(Arc::new(pol));
        let out = run(&r, "exec", json!({"cmd": "printenv TOOLS_STD_TEST_PING"}))
            .await
            .unwrap();
        assert!(out.content.contains("pong"), "{}", out.content);
        // Listed in pass_env but key-shaped: still withheld, exit nonzero.
        let out = run(&r, "exec", json!({"cmd": "printenv OPENAI_API_KEY"}))
            .await
            .unwrap();
        assert!(!out.success);
        assert!(
            !out.content.contains("secret-should-not-leak"),
            "{}",
            out.content
        );
        std::env::remove_var("TOOLS_STD_TEST_PING");
        std::env::remove_var("OPENAI_API_KEY");
    }

    #[tokio::test]
    async fn syntax_check_argv_handles_space_in_path() {
        // Staging under a dir with a space: the old join-then-split
        // round-trip fed `cat` three paths and failed; real argv passes.
        let base = tmp_root().join("dir with space");
        tokio::fs::create_dir_all(&base).await.unwrap();
        let root = tmp_root();
        let pol = Policy {
            root: root.clone(),
            allowed_commands: vec![],
            allowed_prefixes: vec!["cat".to_string()],
            syntax_cmd: Some(vec!["cat".to_string()]),
            denied_globs: default_denied_globs(),
            pass_env: Vec::new(),
        };
        stage_and_check(
            &pol,
            &["cat".to_string()],
            "hello\n",
            &CancellationToken::new(),
            &base,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn write_syntax_passes_with_space_in_root() {
        let root = tmp_root().join("work dir");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("f.txt"), "hello\n").unwrap();
        let r = reg(syntax_policy(&root, &["true"]));
        run(&r, "write", json!({"path": "f.txt", "content": "bye\n"}))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "bye\n"
        );
    }

    #[test]
    fn split_cmd_honors_quotes() {
        assert_eq!(split_cmd("ls -la"), vec!["ls", "-la"]);
        assert_eq!(
            split_cmd("python3 -c \"print(open('f').read())\""),
            vec!["python3", "-c", "print(open('f').read())"]
        );
        assert_eq!(split_cmd("echo 'a b' c"), vec!["echo", "a b", "c"]);
        assert!(split_cmd("").is_empty());
    }

    // --- test tool verdict ---

    #[tokio::test]
    async fn test_verdict_shape() {
        let root = tmp_root();
        let r = reg(policy(&root));
        let pass = run(&r, "test", json!({"cmd": "true"})).await.unwrap();
        assert!(pass.content.starts_with("PASS: true"), "{}", pass.content);
        assert!(pass.success);
        let fail = run(&r, "test", json!({"cmd": "false"})).await.unwrap();
        assert!(fail.content.starts_with("FAIL: false"), "{}", fail.content);
        assert!(!fail.success);
    }

    #[tokio::test]
    async fn exec_and_test_exit_status_shapes() {
        // Exit status propagates into `ToolOutcome.success`: zero exit is
        // success, nonzero exit is still an Ok outcome with success false.
        let root = tmp_root();
        let r = reg(policy(&root));
        let ok = run(&r, "exec", json!({"cmd": "true"})).await.unwrap();
        assert!(ok.success);
        assert!(!ok.truncated);
        let err_exit = run(&r, "exec", json!({"cmd": "false"})).await.unwrap();
        assert!(!err_exit.success);
        assert!(!err_exit.truncated);
    }

    #[tokio::test]
    async fn test_noisy_output_condensed_verdict_still_pass() {
        // Early PASS marker + noise storm: the marker survives condensing,
        // filler does not, the verdict (exit status, decided on raw output)
        // stays PASS, and the evidence stays within the output bound.
        let root = tmp_root();
        let mut noisy = String::from("test result: ok. 1 passed; 0 failed\n");
        for i in 0..20_000 {
            noisy.push_str(&format!("Compiling noise v0.1.0\nfiller line {i}\n"));
        }
        assert!(noisy.len() > OUT_CAP);
        std::fs::write(root.join("noisy.txt"), &noisy).unwrap();
        let pol = Arc::new(Policy {
            root: root.clone(),
            allowed_commands: vec![],
            allowed_prefixes: vec!["cat".to_string()],
            syntax_cmd: None,
            denied_globs: default_denied_globs(),
            pass_env: Vec::new(),
        });
        let out = run(&reg(pol), "test", json!({"cmd": "cat noisy.txt"}))
            .await
            .unwrap();
        assert!(
            out.content.starts_with("PASS: cat noisy.txt"),
            "{}",
            out.content
        );
        assert!(
            out.content.contains("test result: ok. 1 passed; 0 failed"),
            "{}",
            out.content
        );
        assert!(!out.content.contains("filler line"), "{}", out.content);
        assert!(
            out.content.chars().count() <= OUT_CAP,
            "{}",
            out.content.chars().count()
        );
    }

    // --- search ---

    #[tokio::test]
    async fn search_finds_lines_skips_git() {
        let root = tmp_root();
        std::fs::write(root.join("a.txt"), "needle here\nplain\n").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/needle.txt"), "needle hidden\n").unwrap();
        let r = reg(policy(&root));
        let out = run(&r, "search", json!({"pattern": "needle"}))
            .await
            .unwrap();
        assert!(out.content.contains("a.txt:1:"), "{}", out.content);
        assert!(!out.content.contains(".git"), "{}", out.content);
    }

    #[tokio::test]
    async fn search_single_file_not_starved_by_sibling_limit() {
        let root = tmp_root();
        // a.txt sorts first and alone exceeds the limit, so a sibling scan
        // would cut z.txt's lines; the direct file search must still find them.
        std::fs::write(root.join("a.txt"), "needle sibling\n".repeat(60)).unwrap();
        std::fs::write(root.join("z.txt"), "needle target\n").unwrap();
        let r = reg(policy(&root));
        let out = run(&r, "search", json!({"path": "z.txt", "pattern": "needle"}))
            .await
            .unwrap();
        assert!(
            out.content.contains("z.txt:1: needle target"),
            "{}",
            out.content
        );
        assert!(!out.content.contains("a.txt"), "{}", out.content);
    }

    // --- schema strictness incl no-coercion ---

    #[test]
    fn schemas_strict_no_coercion() {
        let root = tmp_root();
        let r = reg(policy(&root));
        for (name, valid) in [
            ("view", json!({"path": "f"})),
            ("search", json!({"pattern": "x"})),
            ("edit", json!({"path": "f", "search": "a", "replace": "b"})),
            ("write", json!({"path": "f", "content": "b"})),
            ("exec", json!({"cmd": "true"})),
            ("test", json!({"cmd": "true"})),
        ] {
            assert!(
                matches!(
                    r.prepare("agent", call("v", name, valid)),
                    CallStatus::Dispatch(_)
                ),
                "{name} valid args must dispatch"
            );
        }
        // Extra property rejected on every tool.
        for name in ["view", "search", "edit", "write", "exec", "test"] {
            let args = match name {
                "view" => json!({"path": "f", "zzz": 1}),
                "search" => json!({"pattern": "x", "zzz": 1}),
                "edit" => json!({"path": "f", "search": "a", "replace": "b", "zzz": 1}),
                "write" => json!({"path": "f", "content": "b", "zzz": 1}),
                _ => json!({"cmd": "true", "zzz": 1}),
            };
            match r.prepare("agent", call("x", name, args)) {
                CallStatus::Result(res) => assert!(res.is_error, "{name} extra prop"),
                CallStatus::Dispatch(_) => panic!("{name} must reject extra property"),
            }
        }
        // No coercion: string "42" is not an integer; missing required fails.
        match r.prepare(
            "agent",
            call("n", "view", json!({"path": "f", "max_bytes": "42"})),
        ) {
            CallStatus::Result(res) => assert!(res.is_error),
            CallStatus::Dispatch(_) => panic!("view must reject string-for-integer"),
        }
        match r.prepare("agent", call("m", "view", json!({}))) {
            CallStatus::Result(res) => assert!(res.is_error),
            CallStatus::Dispatch(_) => panic!("view must reject missing required"),
        }
        match r.prepare("agent", call("w", "exec", json!({"cmd": 42}))) {
            CallStatus::Result(res) => assert!(res.is_error),
            CallStatus::Dispatch(_) => panic!("exec must reject wrong type"),
        }
    }

    #[test]
    fn definitions_pinned() {
        let root = tmp_root();
        let r = reg(policy(&root));
        let defs: Vec<String> = r.definitions().iter().map(|d| d.name.clone()).collect();
        assert_eq!(
            defs,
            vec!["edit", "exec", "search", "test", "view", "write"]
        );
        for d in r.definitions() {
            assert_eq!(
                d.schema.get("additionalProperties"),
                Some(&Value::Bool(false)),
                "{} must be closed",
                d.name
            );
            assert!(
                d.schema.get("required").is_some(),
                "{} needs required",
                d.name
            );
        }
    }
}
