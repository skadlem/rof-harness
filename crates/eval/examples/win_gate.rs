//! Winnability gate for a frozen slice: every id's task dir must declare
//! winnable artifacts (patch|create), parseable task.toml, tests + oracle.
//! Usage: win_gate <slice.json> <tasks-root>; exit 1 on any violation.
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let raw = std::fs::read_to_string(&args[1]).expect("slice file must read");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("slice file must parse");
    let caps = eval::ToolCaps {
        can_create: true,
        can_patch: true,
        exec_prefixes: vec!["python3".into()],
    };
    let mut bad = 0usize;
    for id in v["ids"].as_array().expect("slice ids") {
        let id = id.as_str().unwrap();
        let dir = Path::new(&args[2]).join(id);
        let violations = eval::check_winnability(&dir, &caps);
        if violations.is_empty() {
            println!("OK   {id}");
        } else {
            bad += 1;
            println!("BAD  {id}: {violations:?}");
        }
    }
    println!(
        "winnability: {} bad / {} total",
        bad,
        v["ids"].as_array().unwrap().len()
    );
    std::process::exit(if bad > 0 { 1 } else { 0 });
}
