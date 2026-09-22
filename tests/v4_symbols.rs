// v4 symbols: deterministic definition index.
#[test]
fn symbol_index_finds_rust_struct_and_fn() {
    let dir = std::env::temp_dir().join(format!("rof-v4-sym-rs-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/a.rs"),
        "pub struct ProcRunTool {\n}\nimpl ProcRunTool {\n}\nfn helper() {}\n",
    )
    .unwrap();
    let syms = rof::context::symbols::index_workdir(&dir, 500);
    assert!(syms.iter().any(|s| s.name == "ProcRunTool"));
    assert!(syms.iter().any(|s| s.name == "helper"));
    std::fs::remove_dir_all(&dir).ok();
}
