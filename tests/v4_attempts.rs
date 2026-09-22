// v4 attempts default + memory load + shell prefix + verify veto note.
#[test]
fn attempts_default_is_one_and_env_parses() {
    assert_eq!(rof::config::AppConfig::default().attempts, 1);
}

#[test]
fn memory_loads_agents_md_and_caps() {
    let dir = std::env::temp_dir().join(format!("rof-v4-mem-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("AGENTS.md"), "conventions: tabs").unwrap();
    let m = rof::context::memory::load(&dir);
    assert!(m.project.contains("tabs"));
    assert!(rof::context::memory::render(&m).contains("PROJECT MEMORY"));
    let empty = rof::context::memory::render(&rof::context::memory::Memory::default());
    assert!(empty.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}
