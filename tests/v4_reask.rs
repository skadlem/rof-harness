// Re-ask diet: the second turn of a `reads` round re-sends the whole assembled
// context plus the requested files (measured 11-15k chars on live runs, dying
// ~50% of the time at both truncation and transport), so an operator-set
// budget lets the re-ask assemble lean with the turn's own evidence first.
// Off by default: no env means the old assemble path, byte for byte.
//
// Env-isolation: these tests never touch process env at all — the budget fn
// takes Option<&str> precisely so no test in this binary can race another
// binary's vars. Keep it that way.
use rof::agents::reask_diet_budget;
use rof::context::{Assembly, ContextAssembler, ContextItem, Fidelity, ItemKey};

fn item(path: &str, region: &str, text: &str, fidelity: Fidelity, must: bool) -> ContextItem {
    ContextItem {
        key: ItemKey {
            path: path.to_string(),
            region: region.to_string(),
            role: "implementer".to_string(),
        },
        label: format!("--- {path}"),
        text: text.to_string(),
        fidelity,
        must_include: must,
    }
}

fn build() -> ContextAssembler<'static> {
    // 'static head: the assembler only borrows it, and string literals live
    // forever, so one builder serves both assemblies under test.
    let mut asm = ContextAssembler::new("HEAD", 100_000);
    asm.add(item("map", "map", "[repo map]", Fidelity::Drop, false));
    asm.add(item(
        "goal.rs",
        "goal-named",
        &"g".repeat(2000),
        Fidelity::Exact,
        false,
    ));
    asm.add_volatile(item(
        "req.rs",
        "requested",
        &"r".repeat(2500),
        Fidelity::Exact,
        false,
    ));
    asm
}

#[test]
fn diet_budget_mapping() {
    // Unset or garbage: no diet, the old path runs unchanged.
    assert_eq!(reask_diet_budget(None, 12_000), None);
    assert_eq!(reask_diet_budget(Some("banana"), 12_000), None);
    assert_eq!(reask_diet_budget(Some(""), 12_000), None);
    // Set below the volatile budget: the diet engages.
    assert_eq!(reask_diet_budget(Some("8000"), 12_000), Some(8000));
    // Floor: under it an item can only vanish, never narrow.
    assert_eq!(reask_diet_budget(Some("100"), 12_000), Some(2000));
    // At or above the budget the diet would be a no-op, so it stays off.
    assert_eq!(reask_diet_budget(Some("12000"), 12_000), None);
    assert_eq!(reask_diet_budget(Some("99999"), 12_000), None);
    // A degenerate configured budget leaves no room for any diet.
    assert_eq!(reask_diet_budget(Some("8000"), 1000), None);
}

#[test]
fn reask_with_ample_budget_matches_assemble_bytes() {
    // When everything fits, fitting order is unobservable: the diet path
    // must render exactly what the old path renders.
    let mut a = build();
    let mut b = build();
    let full_a = match a.assemble() {
        Assembly::Ok(p) => p.full(),
        Assembly::SelectionFailure { .. } => panic!("100k budget must fit"),
    };
    let full_b = match b.assemble_reask(100_000) {
        Assembly::Ok(p) => p.full(),
        Assembly::SelectionFailure { .. } => panic!("100k diet must fit"),
    };
    assert_eq!(full_a, full_b);
}

#[test]
fn reask_starves_background_before_evidence() {
    // Budget 3000: the 2500-char requested file fits, the 2000-char
    // background does not once evidence goes first. Items-first order would
    // deliver the background and drop the evidence instead.
    let mut asm = ContextAssembler::new("HEAD", 100_000);
    asm.add(item(
        "back.rs",
        "tail",
        &"b".repeat(2000),
        Fidelity::Exact,
        false,
    ));
    asm.add_volatile(item(
        "req.rs",
        "requested",
        &"r".repeat(2500),
        Fidelity::Exact,
        false,
    ));
    match asm.assemble_reask(3000) {
        Assembly::Ok(parts) => {
            assert!(
                parts.volatile_tail.contains(&"r".repeat(100)),
                "the requested evidence must survive the diet"
            );
            assert!(
                !parts.tail.contains(&"b".repeat(100)),
                "background must starve before evidence"
            );
        }
        Assembly::SelectionFailure { .. } => panic!("optional drops are not failures"),
    }
}

#[test]
fn reask_names_starved_must_include_as_excess() {
    // A must-include file (a skill body, an explicitly required read) that
    // cannot fit even at the narrowest window is excess with a name — the
    // caller turns that into an honest cut instead of a silent drop.
    let mut asm = ContextAssembler::new("HEAD", 100_000);
    asm.add_volatile(item(
        "big.rs",
        "requested",
        &"x".repeat(10_000),
        Fidelity::Exact,
        true,
    ));
    match asm.assemble_reask(3000) {
        Assembly::SelectionFailure { excess, .. } => {
            assert_eq!(excess.len(), 1);
            assert_eq!(excess[0].key.path, "big.rs");
        }
        Assembly::Ok(_) => panic!("a 10k exact item cannot fit in 3000 chars"),
    }
}
