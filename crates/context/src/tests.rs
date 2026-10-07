use super::*;

fn mk(path: &str, region: &str, text: &str, fidelity: Fidelity, must: bool) -> ContextItem {
    ContextItem {
        key: ItemKey {
            path: path.into(),
            region: region.into(),
            role: "t".into(),
        },
        fidelity,
        must_include: must,
        text: text.into(),
    }
}

#[test]
fn dedupe_by_key() {
    let mut a = ContextAssembler::new(100_000);
    a.add(mk("s/a.rs", "cur", "impl A {}", Fidelity::Exact, true));
    a.add(mk("s/a.rs", "cur", "impl A {}", Fidelity::Exact, true));
    assert_eq!(a.assemble().matches("impl A {}").count(), 1);
}

#[test]
fn same_path_two_regions_is_not_a_duplicate() {
    let mut a = ContextAssembler::new(100_000);
    a.add(mk("s/a.rs", "head", "pub mod a;", Fidelity::Exact, true));
    a.add(mk("s/a.rs", "tail", "fn main() {}", Fidelity::Exact, true));
    let out = a.assemble();
    assert!(out.contains("pub mod a;") && out.contains("fn main() {}"));
}

#[test]
fn must_include_survives_tiny_budget() {
    let mut a = ContextAssembler::new(10);
    a.add(mk(
        "big.rs",
        "cur",
        &"x".repeat(5000),
        Fidelity::Exact,
        true,
    ));
    a.add(mk(
        "opt.rs",
        "cur",
        &"y".repeat(5000),
        Fidelity::Drop,
        false,
    ));
    let out = a.assemble();
    assert!(out.contains('x') && !out.contains('y'));
}

#[test]
fn windowed_narrows_by_halving_until_it_fits() {
    let mut a = ContextAssembler::new(3000);
    a.add(mk(
        "s/a.rs",
        "cur",
        &"fn anchor_sym() {}\n".repeat(300),
        Fidelity::Windowed {
            anchor: "anchor_sym".into(),
            cap: 4000,
        },
        true,
    ));
    let out = a.assemble();
    assert!(out.contains("anchor_sym"));
    assert!(out.chars().count() < 3000);
}

#[test]
fn oversized_must_include_narrows_to_floor_never_silent_cut() {
    let mut a = ContextAssembler::new(10);
    a.add(mk(
        "s/a.rs",
        "cur",
        &"fn anchor_sym() {}\n".repeat(600),
        Fidelity::Windowed {
            anchor: "anchor_sym".into(),
            cap: 8000,
        },
        true,
    ));
    let out = a.assemble();
    assert!(out.contains("anchor_sym"));
    assert!(out.chars().count() <= MIN_WINDOW + 200);
}

#[test]
fn window_anchored_on_symbol() {
    let text = "a\n".repeat(3000) + "fn target_sym() {}\n" + &"b\n".repeat(3000);
    let w = window_anchored(&text, "target_sym", 2000);
    assert!(w.contains("target_sym") && w.chars().count() <= 2000 + 100);
    let w2 = window_anchored(&text, "absent", 2000);
    assert!(w2.contains("elided"));
}

#[test]
fn cut_chars_respects_multibyte_boundaries() {
    let t = "λ".repeat(10);
    let c = cut_chars(&t, 4);
    assert_eq!(c.chars().count(), 4);
    assert_eq!(c, "λλλλ");
    assert_eq!(cut_chars("abc", 99), "abc");
}

#[test]
fn collapse_boundary_steps_in_h_batches_and_h0_is_the_tail_rule() {
    assert_eq!(COLLAPSE_HYSTERESIS, 0); // default = today
    for h in 1..=8 {
        let mut moves = Vec::new();
        let mut prev = 0;
        for t in 0..=80 {
            let b = collapse_boundary(t, COLLAPSE_KEEP, h);
            assert_eq!(b % h, 0, "t={t} h={h}: boundary {b} not an H multiple");
            let window = t - b;
            assert!(window <= COLLAPSE_KEEP + h, "t={t} h={h}: window {window}");
            assert!(
                window >= COLLAPSE_KEEP.min(t),
                "t={t} h={h}: window {window}"
            );
            if b != prev {
                moves.push(t);
                prev = b;
            }
        }
        // First move at keep+h, then exactly every h rows: one batch per h.
        let want: Vec<usize> = (0..(80 - COLLAPSE_KEEP) / h)
            .map(|n| COLLAPSE_KEEP + h + n * h)
            .collect();
        assert_eq!(moves, want, "h={h}");
    }
    for t in 0..=80 {
        assert_eq!(
            collapse_boundary(t, COLLAPSE_KEEP, 0),
            t.saturating_sub(COLLAPSE_KEEP)
        );
    }
}

#[test]
fn named_file_cap_returns_whole_small_file_and_cuts_big() {
    let dir = std::env::temp_dir().join(format!("ctx-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("small.rs"), "fn f() {}").unwrap();
    std::fs::write(dir.join("big.rs"), "λ".repeat(5000)).unwrap();
    assert_eq!(
        named_file_contents(&dir, "small.rs", 2000).unwrap(),
        "fn f() {}"
    );
    let big = named_file_contents(&dir, "big.rs", 2000).unwrap();
    assert_eq!(big.chars().count(), 2000);
    std::fs::remove_dir_all(&dir).unwrap();
}

fn msg(role: &str, chars: usize) -> CompactMessage {
    CompactMessage {
        role: role.into(),
        content: "x".repeat(chars),
    }
}

#[test]
fn estimate_is_anchor_plus_chars4_over_the_tail() {
    let msgs = vec![msg("user", 4), msg("assistant", 8)];
    let anchor = UsageAnchor {
        messages: 1,
        input_tokens: 100,
    };
    assert_eq!(estimate_tokens(&msgs, Some(&anchor)), 102);
    // Whole list when nothing settled yet: ceil(12 / 4).
    assert_eq!(estimate_tokens(&msgs, None), 3);
    // An anchor longer than the list is stale, not authoritative.
    let stale = UsageAnchor {
        messages: 9,
        input_tokens: 100,
    };
    assert_eq!(estimate_tokens(&msgs, Some(&stale)), 3);
    // Chars/4 rounds up: a 1-char tail is not free.
    assert_eq!(estimate_tokens(&[msg("user", 1)], None), 1);
}

#[test]
fn compaction_due_is_strict_and_disabled_never_fires() {
    let cfg = CompactionConfig {
        enabled: true,
        frac: 0.5,
        keep_tokens: 20_000,
    };
    assert!(!compaction_due(100_000, 200_000, &cfg), "at the threshold");
    assert!(compaction_due(100_001, 200_000, &cfg), "just above");
    assert!(!compaction_due(99_999, 200_000, &cfg), "just below");
    let off = CompactionConfig {
        enabled: false,
        ..cfg
    };
    assert!(!compaction_due(u64::MAX, 1, &off));
}

/// The cut walks back until the kept tail reaches the budget and lands on
/// a non-tool message; each message here is 40 chars = 10 tokens.
#[test]
fn cut_point_keeps_recent_tail_and_never_starts_on_a_tool_result() {
    let msgs = vec![
        msg("user", 4),
        msg("assistant", 40),
        msg("tool", 40),
        msg("assistant", 40),
        msg("tool", 40),
    ];
    // Crossing lands on the assistant at 3: keep [3, 4] = 20 tokens.
    assert_eq!(cut_point(&msgs, 15), Some(3));
    // Crossing lands on the tool result at 4: the cut backs up to the
    // assistant call that produced it instead of orphaning the result.
    assert_eq!(cut_point(&msgs, 10), Some(3));
    assert_eq!(msgs[cut_point(&msgs, 10).unwrap()].role, "assistant");
    // Whole history below the keep budget: nothing to summarize.
    assert_eq!(cut_point(&msgs, 100), None);
    // A lone trailing tool result cannot be cut around at all.
    assert_eq!(cut_point(&[msg("user", 4), msg("tool", 40)], 1), None);
    assert_eq!(cut_point(&[msg("user", 4)], 1), None);
}

#[test]
fn summary_payload_caps_tool_results_only() {
    let payload = summary_payload(&[
        msg("user", 4),
        msg("tool", 2_500),
        CompactMessage {
            role: "assistant".into(),
            content: "short".into(),
        },
    ]);
    assert!(payload.starts_with("[user]: xxxx\n\n[tool]: x"));
    assert!(payload.contains("[... 500 more characters truncated]"));
    assert!(payload.ends_with("[assistant]: short"));
    // Only the tool block is cut: 2500 x's became 2000.
    assert_eq!(payload.matches('x').count(), 4 + SUMMARY_TOOL_CAP);
    let small = summary_payload(&[msg("tool", 10)]);
    assert_eq!(small, format!("[tool]: {}", "x".repeat(10)));
}

fn file_map_tmp(prefix: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ctx-filemap-{prefix}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn file_map_is_sorted_truncated_and_repeatable() {
    let dir = file_map_tmp("sorted");
    for name in ["c.rs", "a.rs", "b.rs"] {
        std::fs::write(dir.join(name), "x").unwrap();
    }
    let first = file_map(&dir, 2);
    assert_eq!(first, vec!["a.rs".to_string(), "b.rs".to_string()]);
    assert_eq!(
        file_map(&dir, 99),
        first
            .iter()
            .chain([&"c.rs".to_string()])
            .cloned()
            .collect::<Vec<_>>()
    );
    assert_eq!(file_map(&dir, 99), file_map(&dir, 99));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn file_map_matches_sorted_full_walk_past_the_cap() {
    let dir = file_map_tmp("largetree");
    let total = FILE_MAP_WALK_CAP + 300;
    for i in 0..total {
        std::fs::write(dir.join(format!("f{i:05}.rs")), "x").unwrap();
    }
    // Independent oracle: no cap, zero-padded names already sort lexically.
    let mut full: Vec<String> = (0..total).map(|i| format!("f{i:05}.rs")).collect();
    full.sort();
    let got200 = file_map(&dir, 200);
    assert_eq!(got200.len(), 200);
    assert_eq!(got200, full[..200]);
    assert!(got200.windows(2).all(|w| w[0] <= w[1]));
    // Oversized asks clamp at the documented post-sort cap, still alpha-first.
    let got_big = file_map(&dir, total + 1000);
    assert_eq!(got_big.len(), FILE_MAP_WALK_CAP);
    assert_eq!(got_big, full[..FILE_MAP_WALK_CAP]);
    // Same tree, repeated runs: byte-identical.
    assert_eq!(file_map(&dir, 200), got200);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn file_map_identical_for_reversed_creation_order() {
    let mk = |prefix: &str, rev: bool| {
        let dir = file_map_tmp(prefix);
        let mut names: Vec<String> = (0..50).map(|i| format!("g{i:03}.rs")).collect();
        if rev {
            names.reverse();
        }
        for n in &names {
            std::fs::write(dir.join(n), "x").unwrap();
        }
        let out = file_map(&dir, 50);
        std::fs::remove_dir_all(&dir).unwrap();
        out
    };
    assert_eq!(mk("fwd", false), mk("rev", true));
}
