use rof::config::PermissionPolicy;
use rof::tools::{FsListTool, FsReadTool, FsWriteTool, ProcRunTool, ToolRegistry};
use std::path::PathBuf;

fn policy_for(root: PathBuf) -> PermissionPolicy {
    // Test the real default grants, anchored to the temp root.
    PermissionPolicy {
        allowed_dirs: vec![root],
        allowed_commands: vec!["echo hi".to_string()],
        ..Default::default()
    }
}

#[tokio::test]
async fn read_only_tools_allow_and_deny() {
    let root = std::env::temp_dir().join(format!("rof-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), "hello").unwrap();
    // file outside the allowed root
    let outside = std::env::temp_dir().join(format!("rof-outside-{}", std::process::id()));
    std::fs::create_dir_all(&outside).unwrap();

    let mut reg = ToolRegistry::new(policy_for(root.clone()));
    reg.register(FsListTool::new(root.clone()));
    reg.register(FsReadTool::new(root.clone()));
    reg.register(FsWriteTool::new(root.clone()));
    reg.register(ProcRunTool::new(root.clone(), vec!["echo hi".to_string()]));

    // allowed: planner lists + reads inside root
    let (r, _) = reg
        .call(
            "planner",
            "fs.list",
            Some(&root),
            serde_json::json!({"path": "."}),
        )
        .await;
    assert!(r.unwrap().ok);
    let (r, _) = reg
        .call(
            "planner",
            "fs.read",
            Some(&root.join("a.txt")),
            serde_json::json!({"path": "a.txt"}),
        )
        .await;
    assert_eq!(r.unwrap().output, "hello");

    // denied: unknown agent, path outside allowlist, unknown tool
    let (r, _) = reg
        .call(
            "intruder",
            "fs.read",
            Some(&root),
            serde_json::json!({"path": "a.txt"}),
        )
        .await;
    assert!(r.is_err());
    let (r, _) = reg
        .call(
            "planner",
            "fs.read",
            Some(&outside),
            serde_json::json!({"path": "x"}),
        )
        .await;
    assert!(r.is_err());
    let (r, _) = reg
        .call("planner", "shell.exec", Some(&root), serde_json::json!({}))
        .await;
    assert!(r.is_err());

    // denied: sibling-prefix (/tmp/rof-test-N2 vs /tmp/rof-test-N) and .. escape
    let sibling = std::env::temp_dir().join(format!("rof-test-{}2", std::process::id()));
    std::fs::create_dir_all(&sibling).unwrap();
    let (r, _) = reg
        .call(
            "planner",
            "fs.read",
            Some(&sibling),
            serde_json::json!({"path": "a.txt"}),
        )
        .await;
    assert!(r.is_err());
    let (r, _) = reg
        .call(
            "planner",
            "fs.read",
            Some(&root.join("sub").join("..").join("..").join("etc")),
            serde_json::json!({"path": "sub/../../etc/passwd"}),
        )
        .await;
    assert!(r.is_err());
    std::fs::remove_dir_all(&sibling).ok();

    // implementer (default policy) may write under root; reviewer may not
    let (r, _) = reg
        .call(
            "implementer",
            "fs.write",
            Some(&root.join("b.txt")),
            serde_json::json!({"path": "b.txt", "content": "hi"}),
        )
        .await;
    assert!(r.unwrap().ok);
    assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "hi");
    // reviewer stays read-only
    let (r, _) = reg
        .call(
            "reviewer",
            "fs.write",
            Some(&root.join("c.txt")),
            serde_json::json!({"path": "c.txt", "content": "no"}),
        )
        .await;
    assert!(r.is_err());
    // tool-layer escape refused even when the policy path looks fine
    let (r, _) = reg
        .call(
            "implementer",
            "fs.write",
            Some(&root),
            serde_json::json!({"path": "../evil.txt", "content": "x"}),
        )
        .await;
    assert!(r.is_err());

    // proc.run: allowlisted exact command runs; everything else is denied
    let (r, _) = reg
        .call(
            "reviewer",
            "proc.run",
            Some(&root),
            serde_json::json!({"cmd": "echo hi"}),
        )
        .await;
    assert!(r.unwrap().output.contains("hi"));
    for bad in ["echo hi; rm -rf /", "echo hi && rm -rf /", "rm -rf /"] {
        let (r, _) = reg
            .call(
                "reviewer",
                "proc.run",
                Some(&root),
                serde_json::json!({"cmd": bad}),
            )
            .await;
        assert!(r.is_err(), "must deny {bad}");
    }
    // implementer has no proc.run grant
    let (r, _) = reg
        .call(
            "implementer",
            "proc.run",
            Some(&root),
            serde_json::json!({"cmd": "echo hi"}),
        )
        .await;
    assert!(r.is_err());

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&outside).ok();
}

/// §4.2: `.git` is the tree-state substrate the write gate and rollback read,
/// so no agent may read or write it — not at the top level, not nested, and not
/// via a listing that would hand it a handle. An agent that reached it could
/// forge the gate's evidence or undo a rollback.
#[tokio::test]
async fn git_substrate_is_unreachable_by_any_agent() {
    let root = std::env::temp_dir().join(format!("rof-git-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main").unwrap();
    std::fs::write(root.join("src"), "").ok();
    std::fs::create_dir_all(root.join("sub")).unwrap();

    let mut reg = ToolRegistry::new(policy_for(root.clone()));
    reg.register(FsListTool::new(root.clone()));
    reg.register(FsReadTool::new(root.clone()));
    reg.register(FsWriteTool::new(root.clone()));
    reg.register(ProcRunTool::new(root.clone(), vec!["echo hi".to_string()]));

    // No read of the substrate, at any depth, for any agent.
    for agent in ["planner", "implementer", "reviewer"] {
        for path in [
            ".git/HEAD",
            ".git",
            "sub/.git/config",
            "sub/../../.git/HEAD",
        ] {
            let (r, _) = reg
                .call(
                    agent,
                    "fs.read",
                    Some(&root),
                    serde_json::json!({"path": path}),
                )
                .await;
            assert!(r.is_err(), "{agent} must not read {path}");
        }
    }

    // No writing into it either: a commit forged by the model would be
    // indistinguishable from a harness baseline.
    let (r, _) = reg
        .call(
            "implementer",
            "fs.write",
            Some(&root),
            serde_json::json!({"path": ".git/HEAD", "content": "ref: refs/heads/pwned"}),
        )
        .await;
    assert!(r.is_err());
    assert_eq!(
        std::fs::read_to_string(root.join(".git/HEAD")).unwrap(),
        "ref: refs/heads/main",
        "the substrate must be unchanged"
    );

    // A listing never offers the handle, so the model cannot probe it.
    let (r, _) = reg
        .call(
            "planner",
            "fs.list",
            Some(&root),
            serde_json::json!({"path": "."}),
        )
        .await;
    let listed = r.unwrap().output;
    assert!(
        !listed.contains(".git"),
        "listing leaks the substrate: {listed}"
    );

    std::fs::remove_dir_all(&root).ok();
}
